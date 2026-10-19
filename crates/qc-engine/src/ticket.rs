//! 常驻进程交给界面进程的任务单：写成 JSON 文件，路径作为命令行参数传过去。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Job;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ticket {
    pub job: Job,
    /// 按下 Ctrl+V 的时刻（Unix 毫秒），界面进程用它算“按键到窗口出现”的延迟。
    pub key_ms: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// `%LOCALAPPDATA%\QuickCopy`
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("QuickCopy")
}

pub fn write_ticket(t: &Ticket) -> std::io::Result<PathBuf> {
    let dir = data_dir().join("jobs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}-{}.json", t.key_ms, std::process::id()));
    std::fs::write(&path, serde_json::to_vec(t)?)?;
    Ok(path)
}

/// 读出后删掉任务单文件。
pub fn take_ticket(path: &Path) -> std::io::Result<Ticket> {
    let data = std::fs::read(path)?;
    let _ = std::fs::remove_file(path);
    Ok(serde_json::from_slice(&data)?)
}

/// 追加一行到 `%LOCALAPPDATA%\QuickCopy\quickcopy.log`。
pub fn log(line: &str) {
    use std::io::Write;
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("quickcopy.log")) {
        let _ = writeln!(f, "{} [{}] {line}", now_ms(), std::process::id());
    }
}

/// 本进程的峰值工作集和当前私有内存，形如 `工作集峰值 12.3 MB / 私有 4.5 MB`。
pub fn peak_memory() -> String {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) };
    if ok == 0 {
        return "未知".into();
    }
    let mb = |b: usize| b as f64 / 1048576.0;
    format!("工作集峰值 {:.1} MB / 私有 {:.1} MB", mb(c.PeakWorkingSetSize), mb(c.PrivateUsage))
}

/// 本进程加上全部子孙进程（Tauri 版的 WebView2 进程都挂在下面）的内存合计。
pub fn tree_memory() -> String {
    use std::collections::HashSet;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let mut procs = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return "未知".into();
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            procs.push((e.th32ProcessID, e.th32ParentProcessID));
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    let mut tree: HashSet<u32> = HashSet::from([std::process::id()]);
    loop {
        let before = tree.len();
        for &(pid, ppid) in &procs {
            if tree.contains(&ppid) {
                tree.insert(pid);
            }
        }
        if tree.len() == before {
            break;
        }
    }
    let (mut peak, mut private) = (0usize, 0usize);
    for &pid in &tree {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                continue;
            }
            let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            if GetProcessMemoryInfo(h, &mut c as *mut _ as *mut _, c.cb) != 0 {
                peak += c.PeakWorkingSetSize;
                private += c.PrivateUsage;
            }
            CloseHandle(h);
        }
    }
    let mb = |b: usize| b as f64 / 1048576.0;
    format!("{} 个进程 工作集峰值合计 {:.1} MB / 私有合计 {:.1} MB", tree.len(), mb(peak), mb(private))
}
