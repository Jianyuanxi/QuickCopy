//! QuickCopy 进度窗口（Win32 原生控件版）。参数：任务单 JSON 路径。
#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use qc_engine::session::{fmt_bytes, fmt_eta, Decision, Phase, Session};
use qc_engine::ticket::{log, now_ms, take_ticket};
use qc_engine::Resolution;
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
        UI.with(|u| u.borrow_mut().key_ms = ticket.key_ms);
        build(hwnd, &ticket.job);
        SESSION.set(Session::start(ticket.job.clone())).ok();
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
unsafe fn build(hwnd: HWND, job: &qc_engine::Job) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut ncm = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let _ = SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, ncm.cbSize, Some(&mut ncm as *mut _ as _), 0, dpi);
    let font = CreateFontIndirectW(&ncm.lfMessageFont);
    const SS_ENDELLIPSIS: u32 = 0x4000;
    const SS_PATHELLIPSIS: u32 = 0x8000;
    let n = job.sources.len();
    let title = format!("正在复制 {n} 个项目到 {}", job.dest.display());
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
        // 样张阶段：冲突一律跳过，结束时列出来
        session.decide(Decision { default: Resolution::Skip, per_file: Default::default() });
    }
    if snap.phase == Phase::Done {
        let _ = KillTimer(Some(hwnd), 1);
        finish(hwnd);
        return;
    }
    UI.with(|cell| {
        let mut u = cell.borrow_mut();
        if !u.shown && session.started.elapsed() >= SHOW_DELAY {
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
    let mut lines = Vec::new();
    if let Some(e) = session.error.get() {
        lines.push(format!("无法复制：{e}"));
    }
    let timing = UI.with(|u| {
        let u = u.borrow();
        let since = |t: u64| if t == 0 { "-".to_string() } else { format!("{}ms", t.saturating_sub(u.key_ms)) };
        format!("距按键：进程启动 {} 窗口就绪 {} 窗口显示 {} 拿到前台 {}", since(u.start_ms), since(u.ready_ms), since(u.shown_ms), u.foreground)
    });
    log(&format!("win32: {timing} 峰值内存 {}", qc_engine::ticket::peak_memory()));
    if let Some(r) = session.report.get() {
        log(&format!(
            "win32: 完成 {} 个文件 {:.1}MB 用时 {:.2}s 跳过 {} 失败 {} 取消 {}",
            r.copied_files,
            r.copied_bytes as f64 / 1048576.0,
            r.elapsed.as_secs_f64(),
            r.skipped.len(),
            r.failures.len(),
            r.cancelled
        ));
        if !r.skipped.is_empty() {
            let names: Vec<String> = r.skipped.iter().take(5).map(|p| p.display().to_string()).collect();
            lines.push(format!("目标位置已有同名文件，跳过了 {} 个（样张阶段一律不覆盖）：\n{}", r.skipped.len(), names.join("\n")));
        }
        if !r.failures.is_empty() {
            let names: Vec<String> = r.failures.iter().take(5).map(|(p, e)| format!("{}：{e}", p.display())).collect();
            lines.push(format!("{} 个文件复制失败：\n{}", r.failures.len(), names.join("\n")));
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
