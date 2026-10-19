//! QuickCopy 常驻进程：低级键盘钩子接管资源管理器和桌面里的 Ctrl+V。
//!
//! 钩子回调里只做几项便宜的判断（前台窗口、焦点控件、剪贴板有没有文件），
//! 命中就吞掉按键，交给工作线程去取目标文件夹、读剪贴板，再拉起界面进程执行。
//! 工作线程发现处理不了（虚拟位置、剪切等），补发一次带标记的 Ctrl+V 交还系统。
#![windows_subsystem = "windows"]

mod shell;

use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use qc_engine::ticket::{log, now_ms, write_ticket, Ticket};
use qc_engine::{Job, Mode};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 自己补发的按键带这个标记，钩子看到就放行。
pub const MAGIC: usize = 0x5143_5059; // "QCPY"

static TX: OnceLock<Sender<Request>> = OnceLock::new();
static mut V_SWALLOWED: bool = false;

enum Request {
    // HWND 不是 Send，按整数传
    Paste { fg: isize, defview: isize, key_ms: u64 },
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--quit") {
        unsafe {
            if let Ok(h) = FindWindowExW(Some(HWND_MESSAGE), None, w!("QuickCopyHook"), None) {
                let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        return;
    }
    unsafe {
        let _m = CreateMutexW(None, true, w!("Local\\QuickCopyHook")).unwrap();
        if windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
            return; // 已经在运行
        }
    }
    let ui = args
        .iter()
        .position(|a| a == "--ui")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "win32".into());

    let (tx, rx) = channel::<Request>();
    TX.set(tx).ok();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        for req in rx {
            match req {
                Request::Paste { fg, defview, key_ms } => {
                    handle_paste(HWND(fg as _), HWND(defview as _), key_ms, &ui)
                }
            }
        }
    });

    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: w!("QuickCopyHook"), ..Default::default() };
        RegisterClassW(&wc);
        let _ = CreateWindowExW(Default::default(), w!("QuickCopyHook"), w!("QuickCopyHook"), Default::default(), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), Some(hinst.into()), 0).expect("安装键盘钩子失败");
        log("started");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(hook);
        log("stopped");
    }
}

unsafe extern "system" fn wnd_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLOSE {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(h, msg, wp, lp)
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

/// 前台是资源管理器或桌面、焦点在文件列表上时，返回 (前台窗口, 文件列表所在的 SHELLDLL_DefView)。
/// 焦点在重命名框、地址栏、搜索框、导航树时返回 None，按键原样放行。
fn explorer_target() -> Option<(HWND, HWND)> {
    unsafe {
        let fg = GetForegroundWindow();
        if !matches!(class_of(fg).as_str(), "CabinetWClass" | "Progman" | "WorkerW") {
            return None;
        }
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gti = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        GetGUIThreadInfo(tid, &mut gti).ok()?;
        let focus = gti.hwndFocus;
        if !matches!(class_of(focus).as_str(), "DirectUIHWND" | "SysListView32") {
            return None;
        }
        let parent = GetParent(focus).ok()?;
        (class_of(parent) == "SHELLDLL_DefView").then_some((fg, parent))
    }
}

unsafe extern "system" fn ll_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if k.vkCode == VK_V.0 as u32 && k.dwExtraInfo != MAGIC {
            let is_down = wp.0 as u32 == WM_KEYDOWN || wp.0 as u32 == WM_SYSKEYDOWN;
            if !is_down {
                if V_SWALLOWED {
                    V_SWALLOWED = false;
                    return LRESULT(1);
                }
            } else if V_SWALLOWED {
                return LRESULT(1); // 按住不放的自动重复
            } else if down(VK_CONTROL) && !down(VK_SHIFT) && !down(VK_MENU) && !down(VK_LWIN) && !down(VK_RWIN) {
                if let Some((fg, defview)) = explorer_target() {
                    if IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() {
                        if let Some(tx) = TX.get() {
                            V_SWALLOWED = true;
                            let _ = tx.send(Request::Paste { fg: fg.0 as isize, defview: defview.0 as isize, key_ms: now_ms() });
                            return LRESULT(1);
                        }
                    }
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// 交还系统：补发一次 Ctrl+V。
fn give_back(fg: HWND, why: &str) {
    log(&format!("交还系统：{why}"));
    unsafe {
        if GetForegroundWindow() != fg {
            return;
        }
        let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, dwExtraInfo: MAGIC, ..Default::default() },
            },
        };
        let ctrl_held = down(VK_CONTROL);
        let mut seq = Vec::new();
        if !ctrl_held {
            seq.push(key(VK_CONTROL, false));
        }
        seq.push(key(VK_V, false));
        seq.push(key(VK_V, true));
        if !ctrl_held {
            seq.push(key(VK_CONTROL, true));
        }
        SendInput(&seq, size_of::<INPUT>() as i32);
    }
}

fn handle_paste(fg: HWND, defview: HWND, key_ms: u64, ui: &str) {
    let Some(clip) = shell::read_clipboard_files() else {
        return give_back(fg, "剪贴板里没有文件路径");
    };
    if clip.is_move {
        return give_back(fg, "剪切（样张阶段交给系统）");
    }
    if let Some(p) = clip.files.iter().find(|p| {
        let s = p.to_string_lossy().to_lowercase();
        s.contains("\\$recycle.bin\\") || !p.exists()
    }) {
        return give_back(fg, &format!("源在回收站或不存在：{}", p.display()));
    }
    let dest = if matches!(class_of(fg).as_str(), "Progman" | "WorkerW") {
        shell::desktop_dir()
    } else {
        shell::explorer_folder(fg, defview)
    };
    let dest: PathBuf = match dest {
        Some(d) if d.is_dir() => d,
        other => return give_back(fg, &format!("目标不是文件系统文件夹：{other:?}")),
    };
    let ticket = Ticket { job: Job { sources: clip.files, dest, mode: Mode::Copy }, key_ms };
    let path = match write_ticket(&ticket) {
        Ok(p) => p,
        Err(e) => return give_back(fg, &format!("写任务单失败：{e}")),
    };
    let exe = std::env::current_exe().unwrap().with_file_name(format!("qc-ui-{ui}.exe"));
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    match std::process::Command::new(&exe).arg(&path).spawn() {
        Ok(c) => log(&format!("拉起 {} pid={} 用时 {}ms", exe.display(), c.id(), now_ms() - key_ms)),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            give_back(fg, &format!("启动界面失败 {}：{e}", exe.display()));
        }
    }
}
