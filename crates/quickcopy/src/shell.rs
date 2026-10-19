//! 和资源管理器打交道：读剪贴板里的文件列表，取窗口当前显示的文件夹。

use std::path::PathBuf;

use windows::core::{w, Interface};
use windows::Win32::Foundation::{HGLOBAL, HWND};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IServiceProvider, CLSCTX_ALL};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::*;

pub struct ClipFiles {
    pub files: Vec<PathBuf>,
    pub is_move: bool,
}

pub fn read_clipboard_files() -> Option<ClipFiles> {
    unsafe {
        // 别的程序正占着剪贴板时稍等重试
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return None;
        }
        let result = (|| {
            let h = GetClipboardData(CF_HDROP.0 as u32).ok()?;
            let hdrop = HDROP(h.0);
            let n = DragQueryFileW(hdrop, u32::MAX, None);
            let mut files = Vec::with_capacity(n as usize);
            for i in 0..n {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, Some(&mut buf));
                files.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
            }
            let fmt = RegisterClipboardFormatW(w!("Preferred DropEffect"));
            let mut is_move = false;
            if let Ok(h) = GetClipboardData(fmt) {
                let g = HGLOBAL(h.0);
                let p = GlobalLock(g) as *const u32;
                if !p.is_null() {
                    is_move = *p & 2 != 0; // DROPEFFECT_MOVE
                    let _ = GlobalUnlock(g);
                }
            }
            (!files.is_empty()).then_some(ClipFiles { files, is_move })
        })();
        let _ = CloseClipboard();
        result
    }
}

pub fn desktop_dir() -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

/// 找到前台资源管理器窗口里、拥有这个 SHELLDLL_DefView 的那个标签页，返回它的文件系统路径。
/// 库、手机、压缩包内部这类虚拟位置拿不到路径，返回 None。
pub fn explorer_folder(fg: HWND, defview: HWND) -> Option<PathBuf> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
        let count = windows.Count().ok()?;
        for i in 0..count {
            let Ok(disp) = windows.Item(&VARIANT::from(i)) else { continue };
            let Ok(wb) = disp.cast::<IWebBrowser2>() else { continue };
            if wb.HWND().ok().map(|h| h.0) != Some(fg.0 as isize) {
                continue;
            }
            let Ok(sp) = wb.cast::<IServiceProvider>() else { continue };
            let Ok(browser) = sp.QueryService::<IShellBrowser>(&SID_STopLevelBrowser) else { continue };
            let Ok(view) = browser.QueryActiveShellView() else { continue };
            if view.GetWindow().ok() != Some(defview) {
                continue; // 同一窗口的其他标签页
            }
            let fv: IFolderView = view.cast().ok()?;
            let pf: IPersistFolder2 = fv.GetFolder().ok()?;
            let pidl = pf.GetCurFolder().ok()?;
            let mut buf = [0u16; 32768];
            let ok = SHGetPathFromIDListEx(pidl, &mut buf, GPFIDL_DEFAULT).as_bool();
            CoTaskMemFree(Some(pidl as _));
            if !ok {
                return None;
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
            return Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
        }
        None
    }
}
