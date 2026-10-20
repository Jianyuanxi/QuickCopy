//! 外观：速度曲线（仿系统复制窗口的绿色图）、窗口图标、打开时切到前台。

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::*;

/// 曲线数据：横轴是完成比例，纵轴是速度。
#[derive(Default)]
pub struct Graph {
    pub samples: Vec<(f32, f32)>,
    pub frac: f32,
    pub label: String,
}

impl Graph {
    pub fn push(&mut self, frac: f32, speed: f32, label: String) {
        self.frac = frac.clamp(0.0, 1.0);
        self.label = label;
        // 速度要攒一会儿才算得出来，算出来之前不画；第一点从 0% 画起，和系统一样
        if self.samples.is_empty() {
            if speed <= 0.0 {
                return;
            }
            self.samples.push((0.0, speed));
        }
        // 同一个横坐标只留最新的一点，免得暂停时堆一大串
        if let Some(last) = self.samples.last_mut() {
            if (self.frac - last.0) * 1000.0 < 1.0 {
                *last = (self.frac.max(last.0), speed);
                return;
            }
        }
        self.samples.push((self.frac, speed));
    }
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// 画到 `hdc` 的 `rc` 区域里。先画到内存位图再一次贴上去，不闪。
pub unsafe fn paint_graph(hdc: HDC, rc: RECT, g: &Graph, font: HFONT) {
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if w <= 2 || h <= 2 {
        return;
    }
    let mem = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem, bmp.into());
    let fill = |r: RECT, c: COLORREF| {
        let b = CreateSolidBrush(c);
        FillRect(mem, &r, b);
        let _ = DeleteObject(b.into());
    };
    fill(RECT { left: 0, top: 0, right: w, bottom: h }, rgb(255, 255, 255));
    // 已完成的部分用浅绿底
    let done_x = (g.frac * (w - 2) as f32) as i32 + 1;
    fill(RECT { left: 1, top: 1, right: done_x, bottom: h - 1 }, rgb(188, 230, 188));
    // 网格
    let grid = CreatePen(PS_SOLID, 1, rgb(214, 234, 214));
    let old_pen = SelectObject(mem, grid.into());
    for i in 1..10 {
        let x = w * i / 10;
        let _ = MoveToEx(mem, x, 1, None);
        let _ = LineTo(mem, x, h - 1);
    }
    for i in 1..4 {
        let y = h * i / 4;
        let _ = MoveToEx(mem, 1, y, None);
        let _ = LineTo(mem, w - 1, y);
    }
    // 速度曲线下方填深绿
    let max = g.samples.iter().map(|s| s.1).fold(0.0f32, f32::max) * 1.25;
    let y_of = |v: f32| if max > 0.0 { h - 1 - (v / max * (h - 2) as f32) as i32 } else { h - 1 };
    if g.samples.len() >= 2 && max > 0.0 {
        let mut pts: Vec<POINT> = g.samples.iter().map(|&(f, v)| POINT { x: (f * (w - 2) as f32) as i32 + 1, y: y_of(v) }).collect();
        let (first, last) = (pts[0].x, pts[pts.len() - 1].x);
        pts.push(POINT { x: last, y: h - 1 });
        pts.push(POINT { x: first, y: h - 1 });
        let brush = CreateSolidBrush(rgb(6, 176, 37));
        let old_brush = SelectObject(mem, brush.into());
        let _ = SelectObject(mem, GetStockObject(NULL_PEN));
        let _ = Polygon(mem, &pts);
        SelectObject(mem, old_brush);
        let _ = DeleteObject(brush.into());
    }
    // 当前速度的横线和文字
    if let Some(&(_, v)) = g.samples.last() {
        let y = y_of(v).clamp(1, h - 2);
        let line = CreatePen(PS_SOLID, 1, rgb(0, 0, 0));
        let _ = SelectObject(mem, line.into());
        let _ = MoveToEx(mem, 1, y, None);
        let _ = LineTo(mem, w - 1, y);
        let _ = SelectObject(mem, grid.into());
        let _ = DeleteObject(line.into());
        let old_font = SelectObject(mem, font.into());
        SetBkMode(mem, TRANSPARENT);
        SetTextColor(mem, rgb(0, 0, 0));
        let mut text: Vec<u16> = g.label.encode_utf16().collect();
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(mem, &mut tm);
        let th = tm.tmHeight;
        // 线太靠上时字写在线下面
        let top = if y - th - 2 < 1 { y + 2 } else { y - th - 2 };
        let mut r = RECT { left: 4, top, right: w - 6, bottom: top + th };
        DrawTextW(mem, &mut text, &mut r, DT_RIGHT | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(mem, old_font);
    }
    // 边框
    let border = CreatePen(PS_SOLID, 1, rgb(188, 188, 188));
    let _ = SelectObject(mem, border.into());
    let _ = SelectObject(mem, GetStockObject(NULL_BRUSH));
    let _ = Rectangle(mem, 0, 0, w, h);
    SelectObject(mem, old_pen);
    let _ = DeleteObject(grid.into());
    let _ = DeleteObject(border.into());
    let _ = BitBlt(hdc, rc.left, rc.top, w, h, Some(mem), 0, 0, SRCCOPY);
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(bmp.into());
    let _ = DeleteDC(mem);
}

/// 窗口图标：exe 里嵌的资源 1（assets/app.ico），按要的尺寸取最接近的一张。
pub unsafe fn make_icon(size: i32) -> HICON {
    let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok().map(|h| HINSTANCE(h.0));
    LoadImageW(hinst, windows::core::PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
        .map(|h| HICON(h.0))
        .unwrap_or_default()
}

/// 打开时切到最前面并拿到焦点。界面进程是常驻进程拉起来的，系统默认不让它抢前台，
/// 所以先挂到当前前台窗口的输入队列上再切；之后用户点别的窗口它就正常退到后面，不常驻置顶。
pub unsafe fn bring_to_front(hwnd: HWND) {
    let fg = GetForegroundWindow();
    let fg_tid = GetWindowThreadProcessId(fg, None);
    let me = GetCurrentThreadId();
    let attached = fg_tid != 0 && fg_tid != me && AttachThreadInput(me, fg_tid, true).as_bool();
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW;
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
    let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
    let _ = BringWindowToTop(hwnd);
    let _ = SetForegroundWindow(hwnd);
    if attached {
        let _ = AttachThreadInput(me, fg_tid, false);
    }
}
