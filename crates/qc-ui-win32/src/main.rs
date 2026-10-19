//! QuickCopy 进度窗口（Win32 原生控件版）。参数：任务单 JSON 路径。
//! 粘贴和撤销都走这里：扫描 → 有冲突就逐个问 → 执行，超过 300ms 才显示进度窗口。
#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use qc_engine::session::{fmt_bytes, fmt_eta, Decision, Phase, Session};
use qc_engine::ticket::{log, now_ms, take_ticket};
use qc_engine::{numbered_name, FileItem, Mode, Resolution};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::WindowsAndMessaging::*;

/// 操作在这么短时间内做完就不弹窗，和系统一样。
const SHOW_DELAY: Duration = Duration::from_millis(300);
const ID_PAUSE: usize = 101;
const ID_CANCEL: usize = 102;

static SESSION: OnceLock<Arc<Session>> = OnceLock::new();

#[derive(Default)]
struct Ui {
    title: HWND,
    percent: HWND,
    bar: HWND,
    name: HWND,
    eta: HWND,
    detail: HWND,
    pause: HWND,
    cancel: HWND,
    font: HFONT,
    shown: bool,
    key_ms: u64,
    start_ms: u64,
    ready_ms: u64,
    shown_ms: u64,
    foreground: bool,
    clip_seq: u32,
    /// 正在弹冲突对话框（对话框自带消息循环，期间计时器还会进来）。
    asking: bool,
    /// 问完冲突的时刻；进度窗口的延迟从这里重新算，免得答完对话框就闪一下进度窗口。
    answered: Option<std::time::Instant>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn main() {
    let Some(path) = std::env::args().nth(1) else { return };
    let ticket = match take_ticket(path.as_ref()) {
        Ok(t) => t,
        Err(e) => {
            log(&format!("win32: 读任务单失败 {e}"));
            return;
        }
    };
    let start_ms = now_ms();

    unsafe {
        // 窗口里没有输入框；不关输入法的话，窗口一激活中文输入法（TSF）就加载进来，
        // 私有内存从 4 MB 涨到 40 MB，显示也慢 200ms 左右
        let _ = windows::Win32::UI::Input::Ime::ImmDisableIME(u32::MAX);
        // 复制时 16 个线程占满 CPU，界面线程不提优先级的话第一次显示窗口要卡 300ms 以上
        let _ = windows::Win32::System::Threading::SetThreadPriority(
            windows::Win32::System::Threading::GetCurrentThread(),
            windows::Win32::System::Threading::THREAD_PRIORITY_HIGHEST,
        );
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: w!("QuickCopyProgress"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("QuickCopyProgress"),
            w!("正在复制"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            None,
            None,
            Some(hinst),
            None,
        )
        .unwrap();
        UI.with(|u| {
            let mut u = u.borrow_mut();
            u.key_ms = ticket.key_ms;
            u.clip_seq = ticket.clip_seq;
        });
        let undoing = ticket.undo.is_some();
        build(hwnd, &ticket.job, undoing);
        let session = match ticket.undo {
            Some(op) => Session::start_undo(op),
            None => Session::start(ticket.job.clone()),
        };
        SESSION.set(session).ok();
        let ready = now_ms();
        UI.with(|u| {
            let mut u = u.borrow_mut();
            u.start_ms = start_ms;
            u.ready_ms = ready;
        });
        SetTimer(Some(hwnd), 1, 100, None);
        tick(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

fn scale(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

unsafe fn child(parent: HWND, class: PCWSTR, text: &str, style: u32, id: usize) -> HWND {
    let h = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        &HSTRING::from(text),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
        0,
        0,
        0,
        0,
        Some(parent),
        Some(HMENU(id as _)),
        None,
        None,
    )
    .unwrap();
    h
}

/// 建控件并按 DPI 排版。
/// 窗口标题和第一行用的动词。
fn verb(job: &qc_engine::Job, undoing: bool) -> &'static str {
    match (undoing, job.mode) {
        (true, _) => "撤销",
        (false, Mode::Copy) => "复制",
        (false, Mode::Move) => "移动",
    }
}

unsafe fn build(hwnd: HWND, job: &qc_engine::Job, undoing: bool) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut ncm = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let _ = SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, ncm.cbSize, Some(&mut ncm as *mut _ as _), 0, dpi);
    let font = CreateFontIndirectW(&ncm.lfMessageFont);
    const SS_ENDELLIPSIS: u32 = 0x4000;
    const SS_PATHELLIPSIS: u32 = 0x8000;
    let n = job.sources.len();
    let v = verb(job, undoing);
    let title = if undoing { format!("正在撤销对 {} 的操作", job.dest.display()) } else { format!("正在{v} {n} 个项目到 {}", job.dest.display()) };
    let _ = SetWindowTextW(hwnd, &HSTRING::from(format!("正在{v}")));
    let st = w!("STATIC");
    let mut u = UI.with(|u| std::mem::take(&mut *u.borrow_mut()));
    u.font = font;
    u.title = child(hwnd, st, &title, SS_PATHELLIPSIS, 0);
    u.percent = child(hwnd, st, "正在计算…", 0, 0);
    u.bar = child(hwnd, PROGRESS_CLASSW, "", 0, 0);
    u.name = child(hwnd, st, "", SS_ENDELLIPSIS, 0);
    u.eta = child(hwnd, st, "", 0, 0);
    u.detail = child(hwnd, st, "", SS_ENDELLIPSIS, 0);
    u.pause = child(hwnd, w!("BUTTON"), "暂停", WS_TABSTOP.0, ID_PAUSE);
    u.cancel = child(hwnd, w!("BUTTON"), "取消", WS_TABSTOP.0, ID_CANCEL);
    SendMessageW(u.bar, PBM_SETRANGE32, Some(WPARAM(0)), Some(LPARAM(1000)));

    let s = |v| scale(v, dpi);
    let (w, m, line) = (s(480), s(16), s(22));
    let rows = [(u.title, s(14)), (u.percent, s(42)), (u.name, s(96)), (u.eta, s(118)), (u.detail, s(140))];
    for (h, y) in rows {
        let _ = MoveWindow(h, m, y, w - 2 * m, line, false);
    }
    let _ = MoveWindow(u.bar, m, s(68), w - 2 * m, s(18), false);
    let (bw, bh, by) = (s(88), s(28), s(174));
    let _ = MoveWindow(u.cancel, w - m - bw, by, bw, bh, false);
    let _ = MoveWindow(u.pause, w - m - 2 * bw - s(8), by, bw, bh, false);
    for h in [u.title, u.percent, u.name, u.eta, u.detail, u.pause, u.cancel] {
        SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    }
    // 客户区 480×216 逻辑像素，换算成窗口外框大小
    let mut rc = RECT { left: 0, top: 0, right: w, bottom: s(216) };
    let _ = AdjustWindowRectExForDpi(&mut rc, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX, false, WINDOW_EX_STYLE::default(), dpi);
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    let sx = (GetSystemMetrics(SM_CXSCREEN) - ww) / 2;
    let sy = (GetSystemMetrics(SM_CYSCREEN) - wh) / 3;
    let _ = SetWindowPos(hwnd, None, sx, sy, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
    UI.with(|cell| *cell.borrow_mut() = u);
}

unsafe fn set_text(h: HWND, s: &str) {
    let _ = SetWindowTextW(h, &HSTRING::from(s));
}

unsafe fn tick(hwnd: HWND) {
    let session = SESSION.get().unwrap();
    let snap = session.snapshot();
    if snap.phase == Phase::NeedDecision {
        if UI.with(|u| std::mem::replace(&mut u.borrow_mut().asking, true)) {
            return;
        }
        match ask_conflicts(hwnd, session) {
            Some(d) => session.decide(d),
            None => session.cancel(),
        }
        UI.with(|u| {
            let mut u = u.borrow_mut();
            u.asking = false;
            u.answered = Some(std::time::Instant::now());
        });
        return;
    }
    if snap.phase == Phase::Done {
        let _ = KillTimer(Some(hwnd), 1);
        finish(hwnd);
        return;
    }
    UI.with(|cell| {
        let mut u = cell.borrow_mut();
        let since = u.answered.map_or(session.started.elapsed(), |t| t.elapsed());
        if !u.shown && since >= SHOW_DELAY {
            u.shown = true;
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
            u.foreground = GetForegroundWindow() == hwnd;
            // 复制时磁盘很忙，写日志可能卡上一秒，所以只记时间，结束时一起写
            u.shown_ms = now_ms();
        }
        if !u.shown {
            return;
        }
        if snap.total_bytes > 0 && snap.phase == Phase::Copying {
            let frac = snap.done_bytes as f64 / snap.total_bytes as f64;
            // 主题进度条往前走有动画会落后于真实进度，往回设是立即生效的，所以先多设 1 再退回
            let pos = (frac * 1000.0) as usize;
            SendMessageW(u.bar, PBM_SETPOS, Some(WPARAM((pos + 1).min(1000))), None);
            SendMessageW(u.bar, PBM_SETPOS, Some(WPARAM(pos)), None);
            let state = if snap.paused { PBST_PAUSED } else { 1 }; // PBST_NORMAL
            SendMessageW(u.bar, PBM_SETSTATE, Some(WPARAM(state as usize)), None);
            set_text(u.percent, &format!("已完成 {:.0}%{}", frac * 100.0, if snap.paused { "（已暂停）" } else { "" }));
            set_text(u.name, &format!("名称：{}", snap.current.as_deref().unwrap_or("")));
            let eta = match (snap.paused, snap.eta) {
                (true, _) => "剩余时间：已暂停".to_string(),
                (false, Some(d)) => format!("剩余时间：{}", fmt_eta(d)),
                (false, None) => "剩余时间：正在计算…".to_string(),
            };
            set_text(u.eta, &eta);
            set_text(
                u.detail,
                &format!(
                    "{} / {} · {} / {} 个文件 · {}/s",
                    fmt_bytes(snap.done_bytes as f64),
                    fmt_bytes(snap.total_bytes as f64),
                    snap.done_files,
                    snap.total_files,
                    fmt_bytes(snap.bytes_per_sec)
                ),
            );
        }
    });
}

unsafe fn finish(hwnd: HWND) {
    let session = SESSION.get().unwrap();
    let v = verb(&session.job, session.undo.is_some());
    let mut lines = Vec::new();
    if let Some(e) = session.error.get() {
        lines.push(format!("无法{v}：{e}"));
    }
    let timing = UI.with(|u| {
        let u = u.borrow();
        let since = |t: u64| if t == 0 { "-".to_string() } else { format!("{}ms", t.saturating_sub(u.key_ms)) };
        format!("距按键：进程启动 {} 窗口就绪 {} 窗口显示 {} 拿到前台 {}", since(u.start_ms), since(u.ready_ms), since(u.shown_ms), u.foreground)
    });
    log(&format!("win32: {timing} 峰值内存 {}", qc_engine::ticket::peak_memory()));
    if let Some(r) = session.report.get() {
        log(&format!(
            "win32: {v}完成 {} 个文件 {:.1}MB 用时 {:.2}s 跳过 {} 失败 {} 备份 {} 取消 {}",
            r.copied_files,
            r.copied_bytes as f64 / 1048576.0,
            r.elapsed.as_secs_f64(),
            r.skipped.len(),
            r.failures.len(),
            r.backups.len(),
            r.cancelled
        ));
        if !r.failures.is_empty() {
            let names: Vec<String> = r.failures.iter().take(5).map(|(p, e)| format!("{}：{e}", p.display())).collect();
            let more = if r.failures.len() > 5 { "\n…" } else { "" };
            lines.push(format!("{} 项没能{v}：\n{}{more}", r.failures.len(), names.join("\n")));
        }
        if session.job.mode == Mode::Move && session.undo.is_none() && !r.moved.is_empty() {
            clear_clipboard(UI.with(|u| u.borrow().clip_seq));
        }
    }
    if !lines.is_empty() {
        let _ = ShowWindow(hwnd, SW_HIDE);
        MessageBoxW(None, &HSTRING::from(lines.join("\n\n")), w!("QuickCopy"), MB_ICONWARNING | MB_SETFOREGROUND);
    }
    let _ = DestroyWindow(hwnd);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            tick(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let session = SESSION.get().unwrap();
            match wp.0 & 0xffff {
                ID_PAUSE => {
                    let paused = !session.prog.paused.load(std::sync::atomic::Ordering::Relaxed);
                    session.set_paused(paused);
                    UI.with(|u| set_text(u.borrow().pause, if paused { "继续" } else { "暂停" }));
                }
                ID_CANCEL => session.cancel(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            SESSION.get().unwrap().cancel();
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            // 静态文字用窗口底色，避免灰底
            let hdc = HDC(wp.0 as _);
            SetBkColor(hdc, COLORREF(GetSysColor(COLOR_WINDOW)));
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 剪切粘贴做完后清空剪贴板（剪贴板在这期间被别的程序改过就不动）。
unsafe fn clear_clipboard(seq: u32) {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard};
    if seq == 0 || GetClipboardSequenceNumber() != seq {
        return;
    }
    if OpenClipboard(None).is_ok() {
        let _ = EmptyClipboard();
        let _ = CloseClipboard();
    }
}

/// 逐个问冲突怎么处理。返回 None 表示用户取消整个操作。
unsafe fn ask_conflicts(hwnd: HWND, session: &Session) -> Option<Decision> {
    let plan = session.plan.get()?;
    let idx: Vec<usize> = plan.files.iter().enumerate().filter(|(_, f)| f.conflict).map(|(i, _)| i).collect();
    let mut per_file = HashMap::new();
    for (k, &i) in idx.iter().enumerate() {
        let (res, all) = conflict_dialog(hwnd, &plan.files[i], idx.len() - k - 1)?;
        if all {
            return Some(Decision { default: res, per_file });
        }
        per_file.insert(i, res);
    }
    Some(Decision { default: Resolution::Skip, per_file })
}

/// 文件的大小和修改时间，形如 `12.3 KB，修改于 2026/10/19 14:05`。
fn describe(p: &Path) -> (String, u64) {
    let Ok(m) = std::fs::metadata(p) else { return ("读不到".into(), 0) };
    let ft = m.last_write_time();
    let mut when = String::new();
    unsafe {
        use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
        let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
        if FileTimeToSystemTime(&f, &mut utc).is_ok() && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok() {
            when = format!("，修改于 {}/{}/{} {:02}:{:02}", local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute);
        }
    }
    (format!("{}{when}", fmt_bytes(m.len() as f64)), ft)
}

const ID_REPLACE: i32 = 1001;
const ID_SKIP: i32 = 1002;
const ID_KEEP: i32 = 1003;

unsafe extern "system" fn dialog_proc(h: HWND, msg: TASKDIALOG_NOTIFICATIONS, _: WPARAM, _: LPARAM, _: isize) -> windows::core::HRESULT {
    if msg == TDN_CREATED {
        let _ = SetForegroundWindow(h);
    }
    windows::core::HRESULT(0)
}

/// 一个冲突的对话框：替换 / 跳过 / 都保留，外加“对剩下的都这样”。
unsafe fn conflict_dialog(owner: HWND, f: &FileItem, remaining: usize) -> Option<(Resolution, bool)> {
    let name = f.dst.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (src_info, src_t) = describe(&f.src);
    let (dst_info, dst_t) = describe(&f.dst);
    let newer = |a: u64, b: u64| if a > b { "（较新）" } else { "" };
    let parent = |p: &Path| p.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let keep_name = (2..).map(|n| numbered_name(&f.dst, n)).find(|p| !p.exists()).unwrap();
    let keep_name = keep_name.file_name().unwrap_or_default().to_string_lossy().into_owned();

    let title = HSTRING::from("替换或跳过文件");
    let main = HSTRING::from(format!("目标位置已有名为“{name}”的文件"));
    let content = HSTRING::from(format!(
        "来源：{}\n    {src_info}{}\n目标：{}\n    {dst_info}{}",
        parent(&f.src),
        newer(src_t, dst_t),
        parent(&f.dst),
        newer(dst_t, src_t)
    ));
    let b_replace = HSTRING::from("替换目标中的文件\n旧文件先留作备份，按 Ctrl+Z 可以撤销");
    let b_skip = HSTRING::from("跳过此文件\n目标中的文件保持不变");
    let b_keep = HSTRING::from(format!("保留两个文件\n来源文件改名为“{keep_name}”"));
    let verify = HSTRING::from(format!("对剩下的 {remaining} 个冲突都这样处理"));
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: ID_REPLACE, pszButtonText: PCWSTR(b_replace.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: ID_SKIP, pszButtonText: PCWSTR(b_skip.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: ID_KEEP, pszButtonText: PCWSTR(b_keep.as_ptr()) },
    ];
    let cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: if IsWindowVisible(owner).as_bool() { owner } else { HWND::default() },
        dwFlags: TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(main.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: ID_SKIP,
        pszVerificationText: if remaining > 0 { PCWSTR(verify.as_ptr()) } else { PCWSTR::null() },
        pfCallback: Some(dialog_proc),
        ..Default::default()
    };
    let (mut button, mut all) = (0i32, windows::core::BOOL(0));
    if TaskDialogIndirect(&cfg, Some(&mut button), None, Some(&mut all)).is_err() {
        log("win32: 冲突对话框打不开，按跳过处理");
        return Some((Resolution::Skip, true));
    }
    let res = match button {
        ID_REPLACE => Resolution::Replace,
        ID_SKIP => Resolution::Skip,
        ID_KEEP => Resolution::KeepBoth,
        _ => return None,
    };
    Some((res, all.as_bool()))
}
