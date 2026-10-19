//! QuickCopy 进度窗口（Tauri / WebView2 版）。参数：任务单 JSON 路径。
//! 和 Win32 版共用 qc-engine 的 Session，只换界面，用来比较内存和弹窗延迟。
#![windows_subsystem = "windows"]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use qc_engine::session::{fmt_bytes, fmt_eta, Decision, Phase, Session};
use qc_engine::ticket::{log, now_ms, take_ticket, tree_memory};
use qc_engine::Resolution;
use serde::Serialize;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// 操作在这么短时间内做完就不弹窗，和系统一样。
const SHOW_DELAY: Duration = Duration::from_millis(300);

static SESSION: OnceLock<Arc<Session>> = OnceLock::new();
/// 页面加载完、能画出内容了；没加载完就显示会是一块白板。
static PAGE_READY: AtomicBool = AtomicBool::new(false);
static PAGE_READY_MS: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
struct Snap {
    title: String,
    frac: f64,
    percent: String,
    name: String,
    eta: String,
    detail: String,
    paused: bool,
    copying: bool,
}

#[tauri::command]
fn snap() -> Snap {
    let s = SESSION.get().unwrap();
    let snap = s.snapshot();
    let n = s.job.sources.len();
    let title = format!("正在复制 {n} 个项目到 {}", s.job.dest.display());
    let copying = snap.total_bytes > 0 && snap.phase == Phase::Copying;
    let frac = if copying { snap.done_bytes as f64 / snap.total_bytes as f64 } else { 0.0 };
    let percent = if copying {
        format!("已完成 {:.0}%{}", frac * 100.0, if snap.paused { "（已暂停）" } else { "" })
    } else {
        "正在计算…".into()
    };
    let eta = match (snap.paused, snap.eta) {
        (true, _) => "剩余时间：已暂停".to_string(),
        (false, Some(d)) => format!("剩余时间：{}", fmt_eta(d)),
        (false, None) => "剩余时间：正在计算…".to_string(),
    };
    Snap {
        title,
        frac,
        percent,
        name: format!("名称：{}", snap.current.as_deref().unwrap_or("")),
        eta,
        detail: format!(
            "{} / {} · {} / {} 个文件 · {}/s",
            fmt_bytes(snap.done_bytes as f64),
            fmt_bytes(snap.total_bytes as f64),
            snap.done_files,
            snap.total_files,
            fmt_bytes(snap.bytes_per_sec)
        ),
        paused: snap.paused,
        copying,
    }
}

#[tauri::command]
fn page_ready() {
    PAGE_READY_MS.store(now_ms(), Relaxed);
    PAGE_READY.store(true, Relaxed);
}

#[tauri::command]
fn toggle_pause() -> bool {
    let s = SESSION.get().unwrap();
    let paused = !s.prog.paused.load(Relaxed);
    s.set_paused(paused);
    paused
}

#[tauri::command]
fn cancel() {
    SESSION.get().unwrap().cancel();
}

fn main() {
    let Some(path) = std::env::args().nth(1) else { return };
    let ticket = match take_ticket(path.as_ref()) {
        Ok(t) => t,
        Err(e) => {
            log(&format!("tauri: 读任务单失败 {e}"));
            return;
        }
    };
    let start_ms = now_ms();
    let key_ms = ticket.key_ms;
    SESSION.set(Session::start(ticket.job)).ok();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![snap, page_ready, toggle_pause, cancel])
        .setup(move |app| {
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("正在复制")
                .inner_size(480.0, 216.0)
                .resizable(false)
                .maximizable(false)
                .visible(false)
                .center()
                .build()?;
            let window_ms = now_ms();
            let handle = app.handle().clone();
            win.on_window_event(|e| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    SESSION.get().unwrap().cancel();
                }
            });
            std::thread::spawn(move || watch(handle, key_ms, start_ms, window_ms));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri 启动失败");
}

/// 后台盯着会话：到点且页面就绪就显示窗口，做完了记日志、报跳过/失败、退出。
fn watch(app: tauri::AppHandle, key_ms: u64, start_ms: u64, window_ms: u64) {
    let session = SESSION.get().unwrap();
    let mut shown_ms = 0u64;
    let mut foreground = false;
    loop {
        let phase = session.phase();
        if phase == Phase::NeedDecision {
            // 样张阶段：冲突一律跳过，结束时列出来
            session.decide(Decision { default: Resolution::Skip, per_file: Default::default() });
        }
        if phase == Phase::Done {
            break;
        }
        if shown_ms == 0 && session.started.elapsed() >= SHOW_DELAY && PAGE_READY.load(Relaxed) {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
                std::thread::sleep(Duration::from_millis(50));
                foreground = w.is_focused().unwrap_or(false);
            }
            shown_ms = now_ms();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let since = |t: u64| if t == 0 { "-".to_string() } else { format!("{}ms", t.saturating_sub(key_ms)) };
    // 复制时磁盘很忙，写日志可能卡上一秒，所以只记时间，结束时一起写
    log(&format!(
        "tauri: 距按键：进程启动 {} 窗口创建 {} 页面就绪 {} 窗口显示 {} 拿到前台 {foreground} 内存 {}",
        since(start_ms),
        since(window_ms),
        since(PAGE_READY_MS.load(Relaxed)),
        since(shown_ms),
        tree_memory()
    ));
    let mut lines = Vec::new();
    if let Some(e) = session.error.get() {
        lines.push(format!("无法复制：{e}"));
    }
    if let Some(r) = session.report.get() {
        log(&format!(
            "tauri: 完成 {} 个文件 {:.1}MB 用时 {:.2}s 跳过 {} 失败 {} 取消 {}",
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
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
        message_box(&lines.join("\n\n"));
    }
    app.exit(0);
}

fn message_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_SETFOREGROUND};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (t, c) = (wide(text), wide("QuickCopy"));
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_ICONWARNING | MB_SETFOREGROUND) };
}
