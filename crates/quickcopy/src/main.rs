//! QuickCopy 常驻进程：低级键盘钩子接管资源管理器和桌面里的 Ctrl+V 和 Ctrl+Z。
//!
//! 钩子回调里只做几项便宜的判断（前台窗口、焦点控件、剪贴板有没有文件），
//! 命中就吞掉按键，交给工作线程去取目标文件夹、读剪贴板，再拉起界面进程执行。
//! 工作线程发现处理不了（虚拟位置、没有可撤销的操作等），补发一次带标记的同一组合键交还系统。
#![windows_subsystem = "windows"]

mod autostart;
mod order;
mod shell;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use qc_engine::ticket::{log, now_ms, write_ticket, Ticket};
use qc_engine::undo;
use qc_engine::{Job, Mode};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::{GetClipboardSequenceNumber, IsClipboardFormatAvailable};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 自己补发的按键带这个标记，钩子看到就放行。
pub const MAGIC: usize = 0x5143_5059; // "QCPY"

static TX: OnceLock<Sender<Request>> = OnceLock::new();
/// 吞掉了按下的那个键（V 或 Z），它的自动重复和抬起也一并吞掉。
static mut SWALLOWED: u32 = 0;

enum Request {
    // HWND 不是 Send，按整数传
    Paste { fg: isize, defview: isize, key_ms: u64 },
    Undo { fg: isize, key_ms: u64 },
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--install" || a == "--uninstall") {
        let on = args.iter().any(|a| a == "--install");
        let ui = args.iter().position(|a| a == "--ui").and_then(|i| args.get(i + 1)).map(String::as_str);
        let r = if on { install(ui) } else { autostart::set(false, Path::new(""), ui).map_err(|e| e.to_string()) };
        if let Err(e) = r {
            log(&format!("{}失败：{e}", if on { "安装" } else { "取消开机启动" }));
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--quit") {
        quit_running();
        return;
    }
    unsafe {
        let _m = CreateMutexW(None, true, w!("Local\\QuickCopyHook")).unwrap();
        if windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
            return; // 已经在运行
        }
    }
    autostart::migrate();
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
                Request::Undo { fg, key_ms } => handle_undo(HWND(fg as _), key_ms, &ui),
            }
        }
    });

    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: w!("QuickCopyHook"), ..Default::default() };
        RegisterClassW(&wc);
        if let Ok(h) = CreateWindowExW(Default::default(), w!("QuickCopyHook"), w!("QuickCopyHook"), Default::default(), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None) {
            order::register(h);
        }
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

/// 让正在运行的常驻进程退出，最多等 3 秒它的窗口消失。
fn quit_running() {
    unsafe {
        let Ok(h) = FindWindowExW(Some(HWND_MESSAGE), None, w!("QuickCopyHook"), None) else { return };
        let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        for _ in 0..30 {
            if !IsWindow(Some(h)).as_bool() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

fn spawn_resident(exe: &Path, ui: Option<&str>) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    if let Some(ui) = ui {
        cmd.args(["--ui", ui]);
    }
    if let Some(d) = exe.parent() {
        cmd.current_dir(d);
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("启动 {} 失败：{e}", exe.display()))
}

/// `--install`：把常驻进程和界面复制到 `%LOCALAPPDATA%\QuickCopy\bin`，开机启动指向那份，再换成那份运行。
/// 这样开发时重新 `cargo build` 不会替换正在用的程序，也不会因为文件被占用而构建失败。
///
/// 先把新文件全部复制成 `*.new`，都成功了才动正在用的：旧文件改名成 `*.old`（运行中的 exe 也能改名，
/// 所以界面正在复制也不怕），再把 `*.new` 改成正式名字。中途失败就把 `*.old` 改回去，重新拉起原来的常驻进程。
fn install(ui: Option<&str>) -> Result<(), String> {
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let src_dir = src.parent().ok_or("取不到程序所在目录")?;
    let bin = qc_engine::ticket::data_dir().join("bin");
    let exe = bin.join("quickcopy.exe");
    if !qc_engine::same_path(src_dir, &bin) {
        std::fs::create_dir_all(&bin).map_err(|e| format!("建目录 {} 失败：{e}", bin.display()))?;
        let names = ["quickcopy.exe".to_string(), format!("qc-ui-{}.exe", ui.unwrap_or("win32"))];
        let with = |name: &str, ext: &str| bin.join(format!("{name}.{ext}"));
        for name in &names {
            // 上次安装留下的 .old 可能还被没退出的界面占着，删不掉就算了，下面改名时会覆盖
            let _ = std::fs::remove_file(with(name, "old"));
            if let Err(e) = std::fs::copy(src_dir.join(name), with(name, "new")) {
                for n in &names {
                    let _ = std::fs::remove_file(with(n, "new"));
                }
                return Err(format!("复制 {name} 失败，没有改动已安装的程序：{e}"));
            }
        }
        let was_running = unsafe { FindWindowExW(Some(HWND_MESSAGE), None, w!("QuickCopyHook"), None).is_ok() };
        quit_running();
        let mut swapped: Vec<&String> = Vec::new();
        let mut failed = None;
        for name in &names {
            let (cur, old, new) = (bin.join(name), with(name, "old"), with(name, "new"));
            if cur.exists() {
                if let Err(e) = std::fs::rename(&cur, &old) {
                    failed = Some(format!("{name} 改名失败：{e}"));
                    break;
                }
            }
            swapped.push(name);
            if let Err(e) = std::fs::rename(&new, &cur) {
                failed = Some(format!("{name} 换新失败：{e}"));
                break;
            }
        }
        if let Some(e) = failed {
            for name in swapped {
                let (cur, old) = (bin.join(name), with(name, "old"));
                if old.exists() {
                    let _ = std::fs::remove_file(&cur);
                    let _ = std::fs::rename(&old, &cur);
                }
            }
            for n in &names {
                let _ = std::fs::remove_file(with(n, "new"));
            }
            if was_running && exe.exists() {
                let _ = spawn_resident(&exe, ui);
            }
            return Err(format!("{e}，已还原原来的程序"));
        }
        for name in &names {
            let _ = std::fs::remove_file(with(name, "old"));
        }
    }
    autostart::set(true, &exe, ui).map_err(|e| e.to_string())?;
    spawn_resident(&exe, ui)?;
    log(&format!("已安装到 {}", bin.display()));
    Ok(())
}

unsafe extern "system" fn wnd_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == order::WM_SHELL_CHANGE {
        order::on_change();
        return LRESULT(0);
    }
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
        let vk = k.vkCode;
        if (vk == VK_V.0 as u32 || vk == VK_Z.0 as u32) && k.dwExtraInfo != MAGIC {
            let is_down = wp.0 as u32 == WM_KEYDOWN || wp.0 as u32 == WM_SYSKEYDOWN;
            if !is_down {
                if SWALLOWED == vk {
                    SWALLOWED = 0;
                    return LRESULT(1);
                }
            } else if SWALLOWED == vk {
                return LRESULT(1); // 按住不放的自动重复
            } else if down(VK_CONTROL) && !down(VK_SHIFT) && !down(VK_MENU) && !down(VK_LWIN) && !down(VK_RWIN) {
                if let (Some((fg, defview)), Some(tx)) = (explorer_target(), TX.get()) {
                    let (fg, key_ms) = (fg.0 as isize, now_ms());
                    let req = if vk == VK_Z.0 as u32 {
                        Some(Request::Undo { fg, key_ms })
                    } else if IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() {
                        Some(Request::Paste { fg, defview: defview.0 as isize, key_ms })
                    } else {
                        None
                    };
                    if let Some(req) = req {
                        SWALLOWED = vk;
                        let _ = tx.send(req);
                        return LRESULT(1);
                    }
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// 交还系统：补发一次 Ctrl+`vk`。
fn give_back(fg: HWND, vk: VIRTUAL_KEY, why: &str) {
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
        seq.push(key(vk, false));
        seq.push(key(vk, true));
        if !ctrl_held {
            seq.push(key(VK_CONTROL, true));
        }
        SendInput(&seq, size_of::<INPUT>() as i32);
    }
}

/// 写任务单并拉起界面进程；失败返回原因。
fn launch(ticket: &Ticket, ui: &str) -> Result<(), String> {
    let path = write_ticket(ticket).map_err(|e| format!("写任务单失败：{e}"))?;
    let exe = std::env::current_exe().unwrap().with_file_name(format!("qc-ui-{ui}.exe"));
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    match std::process::Command::new(&exe).arg(&path).spawn() {
        Ok(mut c) => {
            log(&format!("拉起 {} pid={} 用时 {}ms", exe.display(), c.id(), now_ms() - ticket.key_ms));
            order::ui_started();
            std::thread::spawn(move || {
                let _ = c.wait();
                order::ui_exited();
            });
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(format!("启动界面失败 {}：{e}", exe.display()))
        }
    }
}

/// Ctrl+Z：撤销日志里有没撤销的操作就撤销它，没有就交给系统撤销它自己的操作。
fn handle_undo(fg: HWND, key_ms: u64, ui: &str) {
    let Some(op) = undo::last_undoable() else {
        return give_back(fg, VK_Z, "没有可撤销的操作");
    };
    let above = order::system_ops_after(op.id);
    if above > 0 {
        order::handed_to_system();
        return give_back(fg, VK_Z, &format!("我们的操作之后还有 {above} 次系统操作，先让系统撤销"));
    }
    log(&format!("撤销 {:?} 到 {}（{} 项）", op.mode, op.dest.display(), op.items()));
    let job = Job { sources: Vec::new(), dest: op.dest.clone(), mode: op.mode };
    let ticket = Ticket { job, key_ms, undo: Some(op), clip_seq: 0 };
    if let Err(e) = launch(&ticket, ui) {
        give_back(fg, VK_Z, &e);
    }
}

fn handle_paste(fg: HWND, defview: HWND, key_ms: u64, ui: &str) {
    let clip_seq = unsafe { GetClipboardSequenceNumber() };
    let Some(clip) = shell::read_clipboard_files() else {
        return give_back(fg, VK_V, "剪贴板里没有文件路径");
    };
    if let Some(p) = clip.files.iter().find(|p| {
        let s = p.to_string_lossy().to_lowercase();
        s.contains("\\$recycle.bin\\") || !p.exists()
    }) {
        return give_back(fg, VK_V, &format!("源在回收站或不存在：{}", p.display()));
    }
    let dest = if matches!(class_of(fg).as_str(), "Progman" | "WorkerW") {
        shell::desktop_dir()
    } else {
        shell::explorer_folder(fg, defview)
    };
    let dest: PathBuf = match dest {
        Some(d) if d.is_dir() => d,
        other => return give_back(fg, VK_V, &format!("目标不是文件系统文件夹：{other:?}")),
    };
    let mode = if clip.is_move { Mode::Move } else { Mode::Copy };
    let ticket = Ticket { job: Job { sources: clip.files, dest, mode }, key_ms, undo: None, clip_seq };
    if let Err(e) = launch(&ticket, ui) {
        give_back(fg, VK_V, &e);
    }
}
