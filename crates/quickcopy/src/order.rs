//! 让 Ctrl+Z 和系统撤销按时间顺序来：我们的操作之后用户又在资源管理器里做了别的文件操作，
//! 先把 Ctrl+Z 交给系统撤销那些，系统的撤完了才轮到我们的。
//!
//! 系统的撤销栈在 explorer.exe 里，读不到，只能估：注册外壳级别的变更通知（资源管理器的复制、
//! 删除、改名、新建都会发；我们自己的引擎直接调文件 API，不发），一阵连续的通知算一次操作。
//! 交给系统撤销一次就减一次。上限：两次操作间隔不到 1.5 秒会被算成一次；别的程序发的外壳通知
//! 也会被算进去，这时要多按几次 Ctrl+Z 才轮到我们的操作。

use std::sync::Mutex;

use qc_engine::ticket::{log, now_ms};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::*;

/// 两阵通知之间至少隔这么久才算两次操作。
const GAP_MS: u64 = 1500;
/// 我们的界面进程退出后再忽略这么久，它的撤销（送回收站）发的通知会晚到。
const GRACE_MS: u64 = 1500;
pub const WM_SHELL_CHANGE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

#[derive(Default)]
struct State {
    /// 每次系统操作开始的时刻。
    bursts: Vec<u64>,
    /// 因为上面有系统操作而把 Ctrl+Z 交给系统的时刻。
    handed: Vec<u64>,
    last_event: u64,
    /// 这个时刻之前的通知不算新操作（我们自己在操作，或系统正在撤销）。
    quiet_until: u64,
    /// 正在跑的界面进程数。
    busy: u32,
}

static STATE: Mutex<State> = Mutex::new(State { bursts: Vec::new(), handed: Vec::new(), last_event: 0, quiet_until: 0, busy: 0 });

pub fn register(hwnd: HWND) {
    unsafe {
        let Ok(desktop) = SHGetFolderLocation(None, 0 /* CSIDL_DESKTOP */, None, 0) else {
            log("注册外壳变更通知失败：取不到桌面");
            return;
        };
        let entry = SHChangeNotifyEntry { pidl: desktop, fRecursive: true.into() };
        let events = SHCNE_CREATE.0 | SHCNE_DELETE.0 | SHCNE_MKDIR.0 | SHCNE_RMDIR.0 | SHCNE_RENAMEITEM.0 | SHCNE_RENAMEFOLDER.0;
        let id = SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel, events as i32, WM_SHELL_CHANGE, 1, &entry);
        if id == 0 {
            log("注册外壳变更通知失败");
        }
    }
}

/// 收到一条外壳变更通知。
pub fn on_change() {
    let now = now_ms();
    let mut s = STATE.lock().unwrap();
    if s.busy > 0 || now < s.quiet_until {
        // 连续的通知一直顺延，系统撤销一个大文件夹可能要好几秒
        if s.busy == 0 {
            s.quiet_until = now + GAP_MS;
        }
        return;
    }
    if now.saturating_sub(s.last_event) > GAP_MS {
        s.bursts.push(now);
        if s.bursts.len() > 200 {
            s.bursts.remove(0);
        }
    }
    s.last_event = now;
}

pub fn ui_started() {
    STATE.lock().unwrap().busy += 1;
}

pub fn ui_exited() {
    let mut s = STATE.lock().unwrap();
    s.busy = s.busy.saturating_sub(1);
    s.quiet_until = now_ms() + GRACE_MS;
}

/// 我们 `op_ms` 时刻记下的操作上面还压着几次系统操作。
pub fn system_ops_after(op_ms: u64) -> usize {
    let s = STATE.lock().unwrap();
    let b = s.bursts.iter().filter(|&&t| t > op_ms).count();
    let h = s.handed.iter().filter(|&&t| t > op_ms).count();
    b.saturating_sub(h)
}

/// 记一次因顺序原因交给系统的 Ctrl+Z；系统随后撤销发出的通知不算新操作。
pub fn handed_to_system() {
    let now = now_ms();
    let mut s = STATE.lock().unwrap();
    s.handed.push(now);
    if s.handed.len() > 200 {
        s.handed.remove(0);
    }
    s.quiet_until = now + GAP_MS;
}
