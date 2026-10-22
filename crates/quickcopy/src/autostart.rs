//! 开机启动：用"启动"文件夹里的快捷方式 `QuickCopy.lnk`，不用 HKCU\...\Run。
//! 本机（Win11 26300）开机时 Explorer 跳过了新写入的 Run 项（Shell-Core 日志 9707 里没有它，同次开机的
//! Span、Everything 都执行了；StartupApproved 写成启用也没用），快捷方式能正常启动（2026-10-20 重启验证，做法同 QuickView）。

use std::path::Path;

use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
use windows::Win32::UI::Shell::*;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const APPROVED_LNK: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";
const NAME: &str = "QuickCopy";
const LNK_NAME: &str = "QuickCopy.lnk";

fn del(key: &str, name: &str) -> bool {
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name)).is_ok() }
}

fn startup_lnk() -> Option<std::path::PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        Some(std::path::PathBuf::from(s?).join(LNK_NAME))
    }
}

fn write_lnk(exe: &Path, ui: Option<&str>) -> windows::core::Result<()> {
    let lnk = startup_lnk().ok_or(windows::core::Error::empty())?;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        sl.SetPath(&HSTRING::from(exe.as_os_str()))?;
        if let Some(d) = exe.parent() {
            sl.SetWorkingDirectory(&HSTRING::from(d.as_os_str()))?;
        }
        if let Some(ui) = ui {
            sl.SetArguments(&HSTRING::from(format!("--ui {ui}")))?;
        }
        sl.SetDescription(&HSTRING::from("QuickCopy 快速复制"))?;
        sl.cast::<IPersistFile>()?.Save(&HSTRING::from(lnk.as_os_str()), true)
    }
}

/// `--install` / `--uninstall`：建或删指向 `exe` 的快捷方式；打开时顺带清掉"设置 > 启动应用"里的禁用记录
pub fn set(on: bool, exe: &Path, ui: Option<&str>) -> windows::core::Result<()> {
    remove_old();
    del(APPROVED_LNK, LNK_NAME);
    if on {
        write_lnk(exe, ui)
    } else {
        if let Some(p) = startup_lnk() {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }
}

/// 删掉旧版写的 Run 项和它在 StartupApproved 里的记录；返回旧版是否开过自启
fn remove_old() -> bool {
    let had = del(RUN, NAME);
    del(APPROVED_RUN, NAME);
    had
}

/// 常驻进程启动时调用：旧版用 Run 开过自启的，迁成快捷方式（已有快捷方式就不动，免得覆盖用户在设置里的开关）
pub fn migrate() {
    if remove_old() && startup_lnk().is_some_and(|p| !p.exists()) {
        let _ = write_lnk(&std::env::current_exe().unwrap_or_default(), None);
    }
}
