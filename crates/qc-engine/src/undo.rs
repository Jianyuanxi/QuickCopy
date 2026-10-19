//! 撤销日志和撤销。每次粘贴做完记一条，资源管理器里按 Ctrl+Z 撤销最近一条没撤销过的。
//!
//! 日志是 `%LOCALAPPDATA%\QuickCopy\ops.json`，常驻进程和界面进程都会读写，用命名互斥量串起来。
//! 只留最近 `KEEP` 条；被挤出去的那条如果还留着替换时的备份，就把备份送进回收站。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

use crate::exec::{restore_backup, win_err, Backup};
use crate::plan::{expand, top_item, Mode};
use crate::win::{move_path, recycle, same_volume};
use crate::{execute, Job, Options, Progress, Report, Resolution};

const KEEP: usize = 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Op {
    pub id: u64,
    pub mode: Mode,
    pub dest: PathBuf,
    pub created: Vec<PathBuf>,
    pub moved: Vec<(PathBuf, PathBuf)>,
    pub backups: Vec<Backup>,
    pub made_dirs: Vec<PathBuf>,
    pub removed_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub undone: bool,
}

impl Op {
    pub fn from_report(job: &Job, r: &Report) -> Op {
        Op {
            id: crate::ticket::now_ms(),
            mode: job.mode,
            dest: job.dest.clone(),
            created: r.created.clone(),
            moved: r.moved.clone(),
            backups: r.backups.clone(),
            made_dirs: r.made_dirs.clone(),
            removed_dirs: r.removed_dirs.clone(),
            undone: false,
        }
    }

    /// 撤销要处理的条目数（进度用）。
    pub fn items(&self) -> usize {
        self.created.len() + self.moved.len() + self.backups.len()
    }
}

fn log_path() -> PathBuf {
    crate::ticket::data_dir().join("ops.json")
}

/// 持有期间独占日志文件。
struct Lock(*mut core::ffi::c_void);

impl Lock {
    fn take() -> Lock {
        let name: Vec<u16> = "Local\\QuickCopyOps\0".encode_utf16().collect();
        unsafe {
            let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if !h.is_null() {
                let w = WaitForSingleObject(h, 5000);
                if w != WAIT_OBJECT_0 && w != WAIT_ABANDONED {
                    CloseHandle(h);
                    return Lock(std::ptr::null_mut());
                }
            }
            Lock(h)
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                ReleaseMutex(self.0);
                CloseHandle(self.0);
            }
        }
    }
}

fn load() -> Vec<Op> {
    fs::read(log_path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

fn save(ops: &[Op]) -> std::io::Result<()> {
    let p = log_path();
    fs::create_dir_all(p.parent().unwrap())?;
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(ops)?)?;
    fs::rename(tmp, p)
}

/// 记一条操作。
pub fn record(op: Op) -> std::io::Result<()> {
    let _l = Lock::take();
    let mut ops = load();
    ops.push(op);
    let mut purge = Vec::new();
    while ops.len() > KEEP {
        let old = ops.remove(0);
        if !old.undone {
            purge.extend(old.backups.into_iter().map(|b| b.backup).filter(|p| p.exists()));
        }
    }
    save(&ops)?;
    let _ = recycle(&purge);
    Ok(())
}

/// 最近一条还没撤销的操作。
pub fn last_undoable() -> Option<Op> {
    let _l = Lock::take();
    load().into_iter().rev().find(|o| !o.undone)
}

fn mark_undone(id: u64) {
    let _l = Lock::take();
    let mut ops = load();
    if let Some(o) = ops.iter_mut().find(|o| o.id == id) {
        o.undone = true;
        let _ = save(&ops);
    }
}

/// 把 `dst` 搬回 `src`：同卷改名；跨卷的文件让系统搬，跨卷的文件夹用引擎复制再删。
fn move_back(dst: &Path, src: &Path, prog: &Progress) -> Result<(), String> {
    if !dst.exists() {
        return Err("找不到了（可能已被移走或删除）".into());
    }
    if src.exists() {
        return Err("原位置已有同名项目，没有动它".into());
    }
    if let Some(parent) = src.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let parent = src.parent().unwrap_or(src);
    if same_volume(dst, parent) || !dst.is_dir() {
        return move_path(dst, src, !same_volume(dst, parent)).map_err(win_err);
    }
    let mut plan = expand(vec![top_item(dst, src)?]);
    plan.mode = Mode::Move;
    // 只借用进度里的暂停/取消，不让子任务改总量
    let sub = Progress::default();
    let r = execute(&plan, &Options::default(), &sub, &|_| Resolution::Skip);
    prog.done_bytes.fetch_add(r.copied_bytes, Relaxed);
    match r.failures.first() {
        Some((p, e)) => Err(format!("{} 个文件没搬回去，例如 {}：{e}", r.failures.len(), p.display())),
        None => Ok(()),
    }
}

/// 撤销一条操作，结果按 `Report` 报（copied_files = 撤销了的条目数）。
pub fn undo(op: &Op, prog: &Progress) -> Report {
    let t0 = Instant::now();
    let mut r = Report::default();
    prog.total_files.store(op.items() as u64, Relaxed);
    let step = |r: &mut Report, what: &Path, res: Result<(), String>| {
        match res {
            Ok(()) => r.copied_files += 1,
            Err(e) => r.failures.push((what.to_path_buf(), e)),
        }
        prog.done_files.fetch_add(1, Relaxed);
    };
    match op.mode {
        Mode::Copy => {
            let live: Vec<PathBuf> = op.created.iter().filter(|p| p.exists()).cloned().collect();
            for p in op.created.iter().filter(|p| !p.exists()) {
                step(&mut r, p, Err("已经不在了".into()));
            }
            match recycle(&live) {
                Ok(()) => {
                    r.copied_files += live.len() as u64;
                    prog.done_files.fetch_add(live.len() as u64, Relaxed);
                }
                // 整批失败时逐个再试，能删的先删
                Err(_) => {
                    for p in &live {
                        step(&mut r, p, recycle(std::slice::from_ref(p)));
                    }
                }
            }
        }
        Mode::Move => {
            for (src, dst) in op.moved.iter().rev() {
                if prog.cancel.load(Relaxed) {
                    break;
                }
                prog.current.store(0, Relaxed);
                step(&mut r, dst, move_back(dst, src, prog));
            }
        }
    }
    for b in &op.backups {
        step(&mut r, &b.original, restore_backup(b));
    }
    // 目标位置为这次操作新建的文件夹，空了就删（深的先删）
    let mut dirs = op.made_dirs.clone();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in &dirs {
        let _ = fs::remove_dir(d);
    }
    for d in &op.removed_dirs {
        let _ = fs::create_dir_all(d);
    }
    r.cancelled = prog.cancel.load(Relaxed);
    r.elapsed = t0.elapsed();
    if !r.cancelled {
        mark_undone(op.id);
    }
    r
}
