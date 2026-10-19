use std::os::windows::ffi::OsStrExt;
use std::path::Path;

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
