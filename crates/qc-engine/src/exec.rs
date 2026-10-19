//! 执行阶段：建目录，多线程复制（或改名）文件，报进度，响应暂停和取消。
//! 跨盘移动是先复制、核对大小、再删源。替换已有文件时先把旧文件改名藏起来当备份，
//! 新文件到位后才算替换完成，撤销时再改回来；任何时候都不直接覆盖。

use std::cell::Cell;
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    CopyFileExW, COPY_FILE_COPY_SYMLINK, COPY_FILE_FAIL_IF_EXISTS, COPY_FILE_NO_BUFFERING, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_READONLY,
};

use crate::plan::{numbered_name, FileItem, Mode, Plan};
use crate::win::{attrs, move_path, set_attrs, wide};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    Replace,
    Skip,
    KeepBoth,
}

/// 被替换掉的旧文件：改名成隐藏的 `original` 同目录文件 `backup`，撤销时改回来。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backup {
    pub backup: PathBuf,
    pub original: PathBuf,
    /// 旧文件原来的属性，还原时恢复。
    pub attrs: u32,
}

/// 界面线程轮询读取；所有字段都是原子量，不加锁。
#[derive(Default)]
pub struct Progress {
    pub total_bytes: AtomicU64,
    pub total_files: AtomicU64,
    pub done_bytes: AtomicU64,
    pub done_files: AtomicU64,
    /// 最近开始处理的文件下标 + 1，0 表示还没开始。
    pub current: AtomicUsize,
    pub paused: AtomicBool,
    pub cancel: AtomicBool,
}

pub struct Options {
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        // 本机实测（i7-13650HX，20 逻辑核，NVMe）：超过 16 线程后小文件反而变慢一倍
        let cpus = thread::available_parallelism().map_or(8, |n| n.get());
        Options { threads: cpus.min(16) }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub copied_files: u64,
    pub copied_bytes: u64,
    pub skipped: Vec<PathBuf>,
    pub failures: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    /// 复制：本次新建出来的东西（撤销时送回收站）。
    pub created: Vec<PathBuf>,
    /// 移动：(原位置, 新位置)，能整体记的按顶层条目记。
    pub moved: Vec<(PathBuf, PathBuf)>,
    pub backups: Vec<Backup>,
    /// 在目标位置新建的文件夹（撤销时空了就删；复制时不含新建顶层文件夹里面的）。
    pub made_dirs: Vec<PathBuf>,
    /// 移动：搬空后删掉的源文件夹（撤销时重建）。
    pub removed_dirs: Vec<PathBuf>,
    pub elapsed: Duration,
}

impl Report {
    /// 有没有改动过磁盘（决定要不要记进撤销日志）。
    pub fn changed_anything(&self) -> bool {
        !(self.created.is_empty() && self.moved.is_empty() && self.backups.is_empty() && self.made_dirs.is_empty())
    }
}

const PROGRESS_CONTINUE: u32 = 0;
const PROGRESS_CANCEL: u32 = 1;

struct CbState<'a> {
    prog: &'a Progress,
    reported: Cell<u64>,
}

unsafe extern "system" fn on_chunk(
    _total: i64,
    transferred: i64,
    _stream_size: i64,
    _stream_transferred: i64,
    _stream: u32,
    _reason: u32,
    _src: HANDLE,
    _dst: HANDLE,
    data: *const c_void,
) -> u32 {
    let st = &*(data as *const CbState);
    let now = transferred as u64;
    st.prog.done_bytes.fetch_add(now - st.reported.get(), Relaxed);
    st.reported.set(now);
    // 暂停：卡在回调里，复制自然停住
    while st.prog.paused.load(Relaxed) && !st.prog.cancel.load(Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
    if st.prog.cancel.load(Relaxed) { PROGRESS_CANCEL } else { PROGRESS_CONTINUE }
}

/// 复制一个文件，目标已存在就失败，绝不覆盖。
fn copy_file(src: &Path, dst: &Path, size: u64, prog: &Progress) -> Result<(), u32> {
    let (s, d) = (wide(src), wide(dst));
    let mut flags = COPY_FILE_COPY_SYMLINK | COPY_FILE_FAIL_IF_EXISTS;
    let ok = if size >= crate::BIG_FILE {
        flags |= COPY_FILE_NO_BUFFERING;
        let st = CbState { prog, reported: Cell::new(0) };
        let ok = unsafe {
            CopyFileExW(s.as_ptr(), d.as_ptr(), Some(on_chunk), &st as *const _ as *const c_void, std::ptr::null_mut(), flags)
        };
        if ok == 0 {
            // 失败或取消：退回已计入的字节
            prog.done_bytes.fetch_sub(st.reported.get(), Relaxed);
        }
        ok
    } else {
        let ok = unsafe { CopyFileExW(s.as_ptr(), d.as_ptr(), None, std::ptr::null(), std::ptr::null_mut(), flags) };
        if ok != 0 {
            prog.done_bytes.fetch_add(size, Relaxed);
        }
        ok
    };
    if ok != 0 { Ok(()) } else { Err(unsafe { GetLastError() }) }
}

/// 同卷改名，目标已存在就失败。
fn rename_file(src: &Path, dst: &Path, size: u64, prog: &Progress) -> Result<(), u32> {
    move_path(src, dst, false)?;
    prog.done_bytes.fetch_add(size, Relaxed);
    Ok(())
}

pub(crate) fn win_err(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

/// 备份名：`原名.~qc-时间-进程-序号`，和原文件在同一目录，改名瞬间完成。
fn backup_name(dst: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    let tag = format!("{:x}-{:x}-{}", crate::ticket::now_ms(), std::process::id(), N.fetch_add(1, Relaxed));
    dst.with_file_name(format!("{name}.~qc-{tag}"))
}

/// 把旧文件改名藏起来；之后调用 `put` 把新文件放到原位置，失败就把旧文件改回去。
fn replace_with(dst: &Path, put: impl FnOnce() -> Result<(), u32>) -> Result<Backup, u32> {
    let bak = backup_name(dst);
    let a = attrs(dst).unwrap_or(0);
    move_path(dst, &bak, false)?;
    set_attrs(&bak, a | FILE_ATTRIBUTE_HIDDEN);
    match put() {
        Ok(()) => Ok(Backup { backup: bak, original: dst.to_path_buf(), attrs: a }),
        Err(e) => {
            if move_path(&bak, dst, false).is_ok() {
                set_attrs(dst, a);
            }
            Err(e)
        }
    }
}

/// 把备份改回原位置；原位置被占着就不动，返回错误。
pub(crate) fn restore_backup(b: &Backup) -> Result<(), String> {
    if b.original.exists() {
        return Err("原位置已经有文件，备份保留在旁边".into());
    }
    move_path(&b.backup, &b.original, false).map_err(win_err)?;
    set_attrs(&b.original, b.attrs);
    Ok(())
}

enum Outcome {
    Done(PathBuf, Option<Backup>),
    Skipped,
    Failed(String),
    Cancelled,
}

fn do_file(f: &FileItem, res: Resolution, rename: bool, prog: &Progress) -> Outcome {
    let put = |dst: &Path| if rename { rename_file(&f.src, dst, f.size, prog) } else { copy_file(&f.src, dst, f.size, prog) };
    let result = match (f.conflict, res) {
        (true, Resolution::Skip) => return Outcome::Skipped,
        (true, Resolution::Replace) => replace_with(&f.dst, || put(&f.dst)).map(|b| (f.dst.clone(), Some(b))),
        (true, Resolution::KeepBoth) => {
            let mut n = 2;
            loop {
                let cand = numbered_name(&f.dst, n);
                match put(&cand) {
                    Ok(()) => break Ok((cand, None)),
                    Err(e) if e == ERROR_FILE_EXISTS || e == ERROR_ALREADY_EXISTS => n += 1,
                    Err(e) => break Err(e),
                }
            }
        }
        (false, _) => put(&f.dst).map(|_| (f.dst.clone(), None)),
    };
    match result {
        Ok((dst, b)) => Outcome::Done(dst, b),
        Err(_) if prog.cancel.load(Relaxed) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(win_err(e)),
    }
}

/// 按计划执行。`resolve` 给出每个冲突文件的处理方式。
pub fn execute(plan: &Plan, opts: &Options, prog: &Progress, resolve: &(dyn Fn(&FileItem) -> Resolution + Sync)) -> Report {
    let t0 = Instant::now();
    prog.total_bytes.store(plan.total_bytes, Relaxed);
    prog.total_files.store(plan.files.len() as u64, Relaxed);

    let mut report = Report::default();
    if prog.cancel.load(Relaxed) {
        // 冲突对话框里点了取消：连目录也不建
        report.cancelled = true;
        return report;
    }
    let dir_results: Vec<Result<PathBuf, (PathBuf, String)>> =
        plan.dirs.par_iter().map(|d| fs::create_dir_all(d).map(|_| d.clone()).map_err(|e| (d.clone(), e.to_string()))).collect();
    // 新建的顶层文件夹整体记，里面的文件和文件夹不再逐个记
    let new_tops: Vec<bool> = plan.tops.iter().map(|t| t.is_dir && !t.merge).collect();
    let new_top_dsts: std::collections::HashSet<String> =
        plan.tops.iter().zip(&new_tops).filter(|(_, &n)| n).map(|(t, _)| key(&t.dst)).collect();
    for r in dir_results {
        match r {
            // 复制时新建的顶层文件夹会整个送回收站，只记合并进已有文件夹时新建的子文件夹
            Ok(d) if plan.mode == Mode::Move || !d.ancestors().any(|a| new_top_dsts.contains(&key(a))) => report.made_dirs.push(d),
            Ok(_) => {}
            Err(e) => report.failures.push(e),
        }
    }
    if plan.mode == Mode::Copy {
        for (t, &new) in plan.tops.iter().zip(&new_tops) {
            if new {
                report.created.push(t.dst.clone());
            }
        }
    }

    let done = run_files(plan, opts, prog, resolve, &mut report);
    match (plan.mode, plan.rename) {
        (Mode::Copy, _) => {
            for (i, dst) in done {
                if !new_tops[plan.files[i].top as usize] {
                    report.created.push(dst);
                }
            }
        }
        (Mode::Move, true) => {
            report.moved = done.into_iter().map(|(i, dst)| (plan.files[i].src.clone(), dst)).collect();
            remove_empty_sources(plan, &mut report);
        }
        (Mode::Move, false) => finish_move(plan, done, &new_tops, &mut report),
    }
    report.cancelled = prog.cancel.load(Relaxed);
    report.elapsed = t0.elapsed();
    report
}

/// 多线程处理文件清单，返回做成了的 (下标, 实际目标路径)。
fn run_files(
    plan: &Plan,
    opts: &Options,
    prog: &Progress,
    resolve: &(dyn Fn(&FileItem) -> Resolution + Sync),
    report: &mut Report,
) -> Vec<(usize, PathBuf)> {
    let next = AtomicUsize::new(0);
    let shared = Mutex::new((report, Vec::new()));
    let threads = opts.threads.clamp(1, 256).min(plan.files.len().max(1));
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let (mut copied, mut copied_bytes) = (0u64, 0u64);
                let (mut done, mut backups, mut skipped, mut failures) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                loop {
                    while prog.paused.load(Relaxed) && !prog.cancel.load(Relaxed) {
                        thread::sleep(Duration::from_millis(50));
                    }
                    if prog.cancel.load(Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Relaxed);
                    let Some(f) = plan.files.get(i) else { break };
                    prog.current.store(i + 1, Relaxed);
                    let res = if f.conflict { resolve(f) } else { Resolution::Skip };
                    match do_file(f, res, plan.rename, prog) {
                        Outcome::Done(dst, b) => {
                            copied += 1;
                            copied_bytes += f.size;
                            done.push((i, dst));
                            backups.extend(b);
                        }
                        Outcome::Skipped => skipped.push(f.src.clone()),
                        Outcome::Failed(e) => failures.push((f.src.clone(), e)),
                        Outcome::Cancelled => break,
                    }
                    prog.done_files.fetch_add(1, Relaxed);
                }
                let mut g = shared.lock().unwrap();
                let (r, all) = &mut *g;
                r.copied_files += copied;
                r.copied_bytes += copied_bytes;
                r.backups.append(&mut backups);
                r.skipped.append(&mut skipped);
                r.failures.append(&mut failures);
                all.append(&mut done);
            });
        }
    });
    shared.into_inner().unwrap().1
}

fn key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

pub(crate) fn remove_file_force(p: &Path) -> std::io::Result<()> {
    match fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let a = attrs(p).unwrap_or(0);
            if a & FILE_ATTRIBUTE_READONLY == 0 {
                return Err(e);
            }
            set_attrs(p, a & !FILE_ATTRIBUTE_READONLY);
            fs::remove_file(p).inspect_err(|_| set_attrs(p, a))
        }
        r => r,
    }
}

/// 搬空了的源文件夹删掉（深的先删；没空的删不掉，原样留着）。
fn remove_empty_sources(plan: &Plan, report: &mut Report) {
    for d in &plan.src_dirs {
        if fs::remove_dir(d).is_ok() {
            report.removed_dirs.push(d.clone());
        }
    }
}

/// 跨盘移动的后半段：逐个核对目标大小，对得上才删源文件。
fn finish_move(plan: &Plan, done: Vec<(usize, PathBuf)>, new_tops: &[bool], report: &mut Report) {
    let results: Vec<Result<(u32, PathBuf, PathBuf), (PathBuf, String)>> = done
        .into_par_iter()
        .map(|(i, dst)| {
            let f = &plan.files[i];
            match fs::metadata(&dst) {
                Ok(m) if m.len() == f.size => {}
                Ok(m) => return Err((f.src.clone(), format!("核对不一致（目标 {} 字节，源 {} 字节），源文件保留", m.len(), f.size))),
                Err(e) => return Err((f.src.clone(), format!("核对时读不到目标：{e}，源文件保留"))),
            }
            remove_file_force(&f.src)
                .map(|_| (f.top, f.src.clone(), dst))
                .map_err(|e| (f.src.clone(), format!("已复制到目标，但删除源文件失败：{e}")))
        })
        .collect();
    let mut moved = Vec::new();
    for r in results {
        match r {
            Ok(m) => moved.push(m),
            Err(e) => report.failures.push(e),
        }
    }
    remove_empty_sources(plan, report);
    // 新建的顶层文件夹整个搬过去了（源文件夹已删）就只记一条，撤销日志小很多
    let whole: Vec<bool> = plan
        .tops
        .iter()
        .zip(new_tops)
        .map(|(t, &new)| new && report.removed_dirs.iter().any(|d| crate::same_path(d, &t.src)))
        .collect();
    for (i, t) in plan.tops.iter().enumerate() {
        if whole[i] {
            report.moved.push((t.src.clone(), t.dst.clone()));
        }
    }
    report.moved.extend(moved.into_iter().filter(|(top, ..)| !whole[*top as usize]).map(|(_, s, d)| (s, d)));
}
