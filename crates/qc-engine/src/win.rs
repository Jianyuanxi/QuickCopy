use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesW, GetVolumePathNameW, MoveFileExW, SetFileAttributesW, INVALID_FILE_ATTRIBUTES,
    MOVEFILE_COPY_ALLOWED, MOVEFILE_WRITE_THROUGH,
};

/// 转成以 0 结尾的 UTF-16；长路径加 `\\?\` 前缀绕过 MAX_PATH。
pub fn wide(p: &Path) -> Vec<u16> {
    let s = p.as_os_str();
    let mut v: Vec<u16> = Vec::with_capacity(s.len() + 8);
    if s.len() >= 240 && !s.to_string_lossy().starts_with(r"\\") {
        v.extend(r"\\?\".encode_utf16());
    }
    v.extend(s.encode_wide());
    v.push(0);
    v
}

fn volume_of(p: &Path) -> Option<String> {
    let mut buf = [0u16; 1024];
    let ok = unsafe { GetVolumePathNameW(wide(p).as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..n]).trim_start_matches(r"\\?\").to_lowercase())
}

/// 两个路径在不在同一个卷上（同卷才能直接改名移动）。
pub fn same_volume(a: &Path, b: &Path) -> bool {
    match (volume_of(a), volume_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 所在的盘有没有回收站：只有本地固定磁盘有，U 盘、存储卡、网络盘上“送回收站”实际是彻底删除。
pub fn has_recycle_bin(p: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_FIXED: u32 = 3;
    match volume_of(p) {
        Some(root) => {
            let w: Vec<u16> = root.encode_utf16().chain([0]).collect();
            unsafe { GetDriveTypeW(w.as_ptr()) == DRIVE_FIXED }
        }
        None => false,
    }
}

/// 改名/移动，目标已存在就失败，绝不覆盖。`cross` 允许跨卷（只对文件有效，系统会先复制再删）。
pub fn move_path(src: &Path, dst: &Path, cross: bool) -> Result<(), u32> {
    let flags = if cross { MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH } else { 0 };
    let ok = unsafe { MoveFileExW(wide(src).as_ptr(), wide(dst).as_ptr(), flags) };
    if ok != 0 { Ok(()) } else { Err(unsafe { GetLastError() }) }
}

pub fn attrs(p: &Path) -> Option<u32> {
    let a = unsafe { GetFileAttributesW(wide(p).as_ptr()) };
    (a != INVALID_FILE_ATTRIBUTES).then_some(a)
}

pub fn set_attrs(p: &Path, a: u32) {
    unsafe { SetFileAttributesW(wide(p).as_ptr(), a) };
}

/// 送进回收站（不弹确认；回收站放不下、要彻底删除时系统会先问）。返回没删掉的。
pub fn recycle(paths: &[PathBuf]) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE,
        SHFILEOPSTRUCTW,
    };
    if paths.is_empty() {
        return Ok(());
    }
    // 多个路径用 \0 隔开，最后再多一个 \0；这个接口不认 \\?\ 前缀
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = list.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING) as u16;
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        Err(format!("删除失败（代码 {rc:#x}）"))
    } else if op.fAnyOperationsAborted != 0 {
        Err("删除被取消".into())
    } else {
        Ok(())
    }
}
