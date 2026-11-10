//! The system's dialogs the app opens for the page, and the installer it
//! hands an update to.

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

/// The download folder, picked in the system's folder dialog from `start`.
pub(crate) fn pick_folder(app: &AppHandle, start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Download folder")
        .set_directory(start);
    if let Some(window) = app.get_webview_window(crate::MAIN) {
        dialog = dialog.set_parent(&window);
    }
    dialog.blocking_pick_folder()?.into_path().ok()
}

/// Where to save file `name`, picked in the system's save dialog from `start`.
pub(crate) fn pick_save(
    app: &AppHandle,
    start: &std::path::Path,
    name: &str,
) -> Option<std::path::PathBuf> {
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Save report")
        .set_directory(start)
        .set_file_name(name);
    if let Some(ext) = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
    {
        dialog = dialog.add_filter(ext.to_ascii_uppercase(), &[ext]);
    }
    if let Some(window) = app.get_webview_window(crate::MAIN) {
        dialog = dialog.set_parent(&window);
    }
    dialog.blocking_save_file()?.into_path().ok()
}

/// Starts setup program `installer` with `args` the way Explorer does: one
/// that asks for administrator rights (as Haul's does on an administrator's
/// account) gets Windows' permission prompt, where starting it as a plain
/// process fails with "The requested operation requires elevation" (error
/// 740). Returns once it has started, or once the person declined.
#[cfg(windows)]
pub(crate) fn run_installer(installer: &std::path::Path, args: &str) -> Result<(), String> {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

    use windows_sys::Win32::UI::{
        Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    };

    /// ERROR_CANCELLED: the person said no to the permission prompt.
    const DECLINED: i32 = 1223;
    let wide = |s: &OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let (verb, file, params) = (
        wide(OsStr::new("open")),
        wide(installer.as_os_str()),
        wide(OsStr::new(args)),
    );
    // SAFETY: a plain C struct, all zeroes being its documented empty value;
    // the strings it points at outlive the call.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    // SAFETY: `info` is filled in as the call expects.
    if unsafe { ShellExecuteExW(&mut info) } != 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(DECLINED) {
        return Err("the update was not installed: Windows' permission prompt was declined".into());
    }
    Err(format!("{}: {e}", installer.display()))
}

#[cfg(not(windows))]
pub(crate) fn run_installer(_installer: &std::path::Path, _args: &str) -> Result<(), String> {
    Err("only the Windows app installs updates itself".into())
}
