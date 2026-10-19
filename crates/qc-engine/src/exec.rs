//! 执行阶段：建目录，多线程复制文件，报进度，响应暂停和取消。

use std::cell::Cell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    CopyFileExW, COPY_FILE_COPY_SYMLINK, COPY_FILE_FAIL_IF_EXISTS, COPY_FILE_NO_BUFFERING,
};

use crate::plan::{numbered_name, FileItem, Plan};
use crate::win::wide;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    Replace,
    Skip,
    KeepBoth,
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
    /// 本次新建出来的东西（撤销时删除它们）。
    pub created: Vec<PathBuf>,
    pub elapsed: Duration,
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

/// 复制一个文件。`overwrite = false` 时目标已存在就失败，绝不覆盖。
fn copy_file(src: &Path, dst: &Path, size: u64, overwrite: bool, prog: &Progress) -> Result<(), u32> {
    let (s, d) = (wide(src), wide(dst));
    let mut flags = COPY_FILE_COPY_SYMLINK;
    if !overwrite {
        flags |= COPY_FILE_FAIL_IF_EXISTS;
    }
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

fn win_err(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

enum Outcome {
    Copied(Option<PathBuf>),
    Skipped,
    Failed(String),
    Cancelled,
}

fn do_file(f: &FileItem, res: Resolution, prog: &Progress) -> Outcome {
    let result = match (f.conflict, res) {
        (true, Resolution::Skip) => return Outcome::Skipped,
        (true, Resolution::Replace) => copy_file(&f.src, &f.dst, f.size, true, prog).map(|_| None),
        (true, Resolution::KeepBoth) => {
            let mut n = 2;
            loop {
                let cand = numbered_name(&f.dst, n);
                match copy_file(&f.src, &cand, f.size, false, prog) {
                    Ok(()) => break Ok(Some(cand)),
                    Err(e) if e == ERROR_FILE_EXISTS || e == ERROR_ALREADY_EXISTS => n += 1,
                    Err(e) => break Err(e),
                }
            }
        }
        (false, _) => copy_file(&f.src, &f.dst, f.size, false, prog).map(|_| Some(f.dst.clone())),
    };
    match result {
        Ok(created) => Outcome::Copied(created),
        Err(_) if prog.cancel.load(Relaxed) => Outcome::Cancelled,
        Err(e) => Outcome::Failed(win_err(e)),
    }
}

/// 按计划执行复制。`resolve` 给出每个冲突文件的处理方式。
pub fn execute(plan: &Plan, opts: &Options, prog: &Progress, resolve: &(dyn Fn(&FileItem) -> Resolution + Sync)) -> Report {
    let t0 = Instant::now();
    prog.total_bytes.store(plan.total_bytes, Relaxed);
    prog.total_files.store(plan.files.len() as u64, Relaxed);

    let mut report = Report::default();
    let dir_errors: Vec<(PathBuf, String)> = plan
        .dirs
        .par_iter()
        .filter_map(|d| std::fs::create_dir_all(d).err().map(|e| (d.clone(), e.to_string())))
        .collect();
    report.failures.extend(dir_errors);

    // 新建的顶层条目整体记入；合并进已有文件夹的，逐个记新建的文件
    let new_tops: Vec<bool> = plan.tops.iter().map(|t| t.is_dir && !t.merge && !t.conflict).collect();
    for (t, &new) in plan.tops.iter().zip(&new_tops) {
        if new {
            report.created.push(t.dst.clone());
        }
    }

    let next = AtomicUsize::new(0);
    let shared = Mutex::new(&mut report);
    let threads = opts.threads.clamp(1, 256).min(plan.files.len().max(1));
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut copied = 0u64;
                let mut copied_bytes = 0u64;
                let mut created = Vec::new();
                let mut skipped = Vec::new();
                let mut failures = Vec::new();
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
                    match do_file(f, res, prog) {
                        Outcome::Copied(path) => {
                            copied += 1;
                            copied_bytes += f.size;
                            if let Some(p) = path {
                                if !new_tops[f.top as usize] {
                                    created.push(p);
                                }
                            }
                        }
                        Outcome::Skipped => skipped.push(f.src.clone()),
                        Outcome::Failed(e) => failures.push((f.src.clone(), e)),
                        Outcome::Cancelled => break,
                    }
                    prog.done_files.fetch_add(1, Relaxed);
                }
                let mut r = shared.lock().unwrap();
                r.copied_files += copied;
                r.copied_bytes += copied_bytes;
                r.created.append(&mut created);
                r.skipped.append(&mut skipped);
                r.failures.append(&mut failures);
            });
        }
    });
    report.cancelled = prog.cancel.load(Relaxed);
    report.elapsed = t0.elapsed();
    report
}
