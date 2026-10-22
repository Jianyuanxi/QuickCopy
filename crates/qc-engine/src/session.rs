//! 一次粘贴操作的后台会话：扫描 → 等界面决定冲突 → 执行。界面只管轮询 `snapshot()`。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::undo::{self, Op};
use crate::{execute, prepare, DepDir, FileItem, Job, Mode, Options, Plan, Progress, Report, Resolution, DEP_ASK_MIN_FILES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Scanning = 0,
    /// 扫描完发现冲突，等界面调用 `decide()`。
    NeedDecision = 1,
    Copying = 2,
    Done = 3,
    /// 扫描完发现依赖目录（node_modules 等），等界面调用 `skip_deps()`。
    AskDeps = 4,
}

/// 界面对所有冲突给出的答复：按文件下标查，没有的用 `default`。
pub struct Decision {
    pub default: Resolution,
    pub per_file: std::collections::HashMap<usize, Resolution>,
}

pub struct Session {
    pub job: Job,
    /// 撤销会话：要撤销的那条操作。
    pub undo: Option<Op>,
    pub prog: Progress,
    pub plan: OnceLock<Plan>,
    pub report: OnceLock<Report>,
    pub error: OnceLock<String>,
    /// 要问的依赖目录，`Phase::AskDeps` 时有值。
    pub deps: OnceLock<Vec<DepDir>>,
    phase: AtomicU8,
    decide_tx: SyncSender<Decision>,
    deps_tx: SyncSender<bool>,
    meter: Mutex<VecDeque<(Instant, u64)>>,
    pub started: Instant,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub phase: Phase,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub total_files: u64,
    pub done_files: u64,
    pub current: Option<String>,
    pub bytes_per_sec: f64,
    pub eta: Option<Duration>,
    pub paused: bool,
}

impl Session {
    pub fn start(job: Job) -> Arc<Session> {
        Self::spawn(job, None)
    }

    /// 撤销一条操作，界面和粘贴共用同一套进度显示。
    pub fn start_undo(op: Op) -> Arc<Session> {
        let job = Job { sources: Vec::new(), dest: op.dest.clone(), mode: op.mode };
        Self::spawn(job, Some(op))
    }

    fn spawn(job: Job, undo: Option<Op>) -> Arc<Session> {
        let (tx, rx) = sync_channel(1);
        let (deps_tx, deps_rx) = sync_channel(1);
        let s = Arc::new(Session {
            job,
            undo,
            prog: Progress::default(),
            plan: OnceLock::new(),
            report: OnceLock::new(),
            error: OnceLock::new(),
            deps: OnceLock::new(),
            phase: AtomicU8::new(Phase::Scanning as u8),
            decide_tx: tx,
            deps_tx,
            meter: Mutex::new(VecDeque::new()),
            started: Instant::now(),
        });
        let s2 = s.clone();
        std::thread::spawn(move || s2.run(rx, deps_rx));
        s
    }

    fn run(&self, rx: Receiver<Decision>, deps_rx: Receiver<bool>) {
        if let Some(op) = &self.undo {
            self.phase.store(Phase::Copying as u8, Relaxed);
            let _ = self.report.set(undo::undo(op, &self.prog));
            self.phase.store(Phase::Done as u8, Relaxed);
            return;
        }
        let mut p = match prepare(&self.job) {
            Ok(p) => p,
            Err(e) => {
                let _ = self.error.set(e);
                self.phase.store(Phase::Done as u8, Relaxed);
                return;
            }
        };
        // 只在复制时问：跨盘移动跳过了依赖目录的话，源文件夹会删不干净
        if self.job.mode == Mode::Copy {
            let deps = p.dep_dirs();
            if deps.iter().map(|d| d.files).sum::<usize>() >= DEP_ASK_MIN_FILES {
                let _ = self.deps.set(deps);
                self.phase.store(Phase::AskDeps as u8, Relaxed);
                if deps_rx.recv().unwrap_or(false) {
                    p.drop_deps();
                }
                if self.prog.cancel.load(Relaxed) {
                    self.phase.store(Phase::Done as u8, Relaxed);
                    return;
                }
            }
        }
        let conflicts = p.conflict_count();
        let _ = self.plan.set(p);
        let plan = self.plan.get().unwrap();
        let decision = if conflicts > 0 {
            self.phase.store(Phase::NeedDecision as u8, Relaxed);
            match rx.recv() {
                Ok(d) => d,
                Err(_) => Decision { default: Resolution::Skip, per_file: Default::default() },
            }
        } else {
            Decision { default: Resolution::Skip, per_file: Default::default() }
        };
        self.phase.store(Phase::Copying as u8, Relaxed);
        let index = |f: &FileItem| (f as *const FileItem as usize - plan.files.as_ptr() as usize) / size_of::<FileItem>();
        let resolve = |f: &FileItem| *decision.per_file.get(&index(f)).unwrap_or(&decision.default);
        let r = execute(plan, &Options::default(), &self.prog, &resolve);
        if r.changed_anything() {
            if let Err(e) = undo::record(Op::from_report(&self.job, &r)) {
                crate::ticket::log(&format!("记撤销日志失败：{e}"));
            }
        }
        let _ = self.report.set(r);
        self.phase.store(Phase::Done as u8, Relaxed);
    }

    pub fn decide(&self, d: Decision) {
        let _ = self.decide_tx.try_send(d);
    }

    /// 回答依赖目录的询问：true 跳过它们。
    pub fn skip_deps(&self, skip: bool) {
        let _ = self.deps_tx.try_send(skip);
    }

    pub fn phase(&self) -> Phase {
        match self.phase.load(Relaxed) {
            0 => Phase::Scanning,
            1 => Phase::NeedDecision,
            2 => Phase::Copying,
            4 => Phase::AskDeps,
            _ => Phase::Done,
        }
    }

    pub fn set_paused(&self, on: bool) {
        self.prog.paused.store(on, Relaxed);
        if !on {
            self.meter.lock().unwrap().clear(); // 暂停期间不算进速度
        }
    }

    pub fn cancel(&self) {
        self.prog.cancel.store(true, Relaxed);
        self.prog.paused.store(false, Relaxed);
        self.skip_deps(false);
        self.decide(Decision { default: Resolution::Skip, per_file: Default::default() });
    }

    /// 速度取最近 3 秒的平均，剩余时间按这个速度估算。
    pub fn snapshot(&self) -> Snapshot {
        let p = &self.prog;
        let done = p.done_bytes.load(Relaxed);
        let total = p.total_bytes.load(Relaxed);
        let now = Instant::now();
        let paused = p.paused.load(Relaxed);
        let mut m = self.meter.lock().unwrap();
        if !paused {
            m.push_back((now, done));
        }
        while m.len() > 2 && now.duration_since(m.front().unwrap().0) > Duration::from_secs(3) {
            m.pop_front();
        }
        let bps = match (m.front(), m.back()) {
            (Some(a), Some(b)) if b.0 > a.0 => (b.1 - a.1.min(b.1)) as f64 / (b.0 - a.0).as_secs_f64(),
            _ => 0.0,
        };
        let eta = (bps > 0.0 && total > done).then(|| Duration::from_secs_f64((total - done) as f64 / bps));
        let current = match (p.current.load(Relaxed), self.plan.get()) {
            (i, Some(plan)) if i > 0 => plan.files.get(i - 1).map(|f| f.src.file_name().unwrap_or_default().to_string_lossy().into_owned()),
            _ => None,
        };
        Snapshot {
            phase: self.phase(),
            total_bytes: total,
            done_bytes: done,
            total_files: p.total_files.load(Relaxed),
            done_files: p.done_files.load(Relaxed),
            current,
            bytes_per_sec: bps,
            eta,
            paused,
        }
    }
}

pub fn fmt_bytes(b: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", U[i]) } else { format!("{v:.1} {}", U[i]) }
}

pub fn fmt_eta(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=4 => "几秒".into(),
        5..=59 => format!("约 {s} 秒"),
        60..=3599 => format!("约 {} 分 {} 秒", s / 60, s % 60),
        _ => format!("约 {} 小时 {} 分", s / 3600, s % 3600 / 60),
    }
}
