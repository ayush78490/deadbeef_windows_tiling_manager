//! What the settings app does to the system: install the binaries, start
//! with Windows, start / stop the window manager, and undo all of it.
//!
//! ```text
//! %LOCALAPPDATA%\Programs\deadbeef\     installed deadbeef.exe (+ dbctl.exe)
//! HKCU\…\CurrentVersion\Run  deadbeef   "…\deadbeef.exe" --wm
//! HKCU\…\Uninstall\deadbeef             Apps & features entry
//! Start menu\Programs\deadbeef.lnk      opens the settings app
//! %APPDATA%\deadbeef\disabled           present after "Reset everything"
//! %APPDATA%\deadbeef\autostart.off      the user switched start-up off
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
    RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

use crate::config::profile::{self, Paths};
use crate::win::wide;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
/// Task Manager's Startup tab records disabled entries here.
const APPROVED_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\deadbeef";
const VALUE: PCWSTR = w!("deadbeef");
const DISABLED_MARKER: &str = "disabled";
const AUTOSTART_OFF_MARKER: &str = "autostart.off";
const EXE: &str = "deadbeef.exe";
const CLI: &str = "dbctl.exe";

pub fn install_dir() -> PathBuf {
    crate::win::local_appdata().join("Programs").join("deadbeef")
}

pub fn installed_exe() -> PathBuf {
    install_dir().join(EXE)
}

fn shortcut_path() -> PathBuf {
    crate::win::start_menu_programs().join("deadbeef.lnk")
}

fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from(EXE))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// Same size and modification time (`fs::copy` preserves the latter).
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && a.modified().ok() == b.modified().ok(),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// Running the installed copy, or the installed copy is identical.
    Current,
    /// Installed, but this exe is a different build.
    Outdated,
    Missing,
}

pub fn install_state() -> Install {
    let (cur, inst) = (current_exe(), installed_exe());
    if !inst.exists() {
        Install::Missing
    } else if same_path(&cur, &inst) || same_file(&cur, &inst) {
        Install::Current
    } else {
        Install::Outdated
    }
}

/// The exe autostart and "Start tiling" should launch.
fn wm_exe() -> PathBuf {
    let inst = installed_exe();
    if inst.exists() {
        inst
    } else {
        current_exe()
    }
}

// ─────────────────────────────── running ───────────────────────────────

pub fn wm_running() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\deadbeef-wm-instance")) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

fn wait_for(running: bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if wm_running() == running {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    wm_running() == running
}

pub fn start_wm() -> Result<(), String> {
    if wm_running() {
        return Ok(());
    }
    let exe = wm_exe();
    std::process::Command::new(&exe).arg("--wm").spawn().map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    if wait_for(true, Duration::from_secs(5)) {
        Ok(())
    } else {
        Err("deadbeef did not start — see the log".into())
    }
}

/// Quits the window manager, which puts every window and the taskbar back.
pub fn stop_wm() -> Result<(), String> {
    if !wm_running() {
        return Ok(());
    }
    crate::ipc::client::send(&["dispatch", "exit"])?;
    if wait_for(false, Duration::from_secs(5)) {
        Ok(())
    } else {
        Err("deadbeef did not stop in time".into())
    }
}

/// Reloads a running window manager; returns its report (config errors).
pub fn reload_wm() -> Result<String, String> {
    crate::ipc::client::send(&["reload"])
}

// ────────────────────────────── autostart ──────────────────────────────

fn read_reg(key: PCWSTR, value: PCWSTR, flags: windows::Win32::System::Registry::REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    unsafe {
        let mut len = 0u32;
        if RegGetValueW(HKEY_CURRENT_USER, key, value, flags, None, None, Some(&mut len)) != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if RegGetValueW(HKEY_CURRENT_USER, key, value, flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut len))
            != ERROR_SUCCESS
        {
            return None;
        }
        buf.truncate(len as usize);
        Some(buf)
    }
}

fn set_string(key: PCWSTR, value: PCWSTR, data: &str) -> Result<(), String> {
    let w = wide(data);
    let r = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, key, value, REG_SZ.0, Some(w.as_ptr().cast()), (w.len() * 2) as u32)
    };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry write failed: {}", windows::core::Error::from(r.to_hresult())))
    }
}

/// Registered to run at sign-in, and not disabled in Task Manager.
pub fn autostart_enabled() -> bool {
    if read_reg(RUN_KEY, VALUE, RRF_RT_REG_SZ).is_none() {
        return false;
    }
    // StartupApproved: first byte even = enabled, odd = disabled.
    read_reg(APPROVED_KEY, VALUE, RRF_RT_REG_BINARY).is_none_or(|b| b.first().is_none_or(|x| x & 1 == 0))
}

pub fn set_autostart(paths: &Paths, on: bool) -> Result<(), String> {
    let off_marker = paths.root.join(AUTOSTART_OFF_MARKER);
    if on {
        set_string(RUN_KEY, VALUE, &format!("\"{}\" --wm", wm_exe().display()))?;
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APPROVED_KEY, VALUE);
        }
        let _ = fs::remove_file(off_marker);
    } else {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
        }
        let _ = fs::write(off_marker, "");
    }
    Ok(())
}

/// Apply turns start-up on unless the user switched it off themselves.
pub fn autostart_wanted(paths: &Paths) -> bool {
    !paths.root.join(AUTOSTART_OFF_MARKER).exists()
}

/// Set by "Reset everything": a plain `deadbeef.exe` launch then opens the
/// settings app instead of tiling.
pub fn is_disabled(paths: &Paths) -> bool {
    paths.root.join(DISABLED_MARKER).exists()
}

pub fn set_disabled(paths: &Paths, disabled: bool) {
    let marker = paths.root.join(DISABLED_MARKER);
    if disabled {
        let _ = fs::create_dir_all(&paths.root);
        let _ = fs::write(marker, "");
    } else {
        let _ = fs::remove_file(marker);
    }
}

// ─────────────────────────────── install ───────────────────────────────

/// Copies this exe (and dbctl next to it) to the install folder, creates the
/// Start menu shortcut and the Apps & features entry. If the window manager
/// is running from the installed copy it is stopped for the copy; the caller
/// starts it again. Returns whether it was stopped.
pub fn install() -> Result<bool, String> {
    let dir = install_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let cur = current_exe();
    let inst = installed_exe();
    let mut stopped = false;
    if !same_path(&cur, &inst) {
        if let Err(e) = fs::copy(&cur, &inst) {
            // Most likely the installed exe is in use by the running WM.
            if !wm_running() {
                return Err(format!("copy to {}: {e}", inst.display()));
            }
            stop_wm()?;
            stopped = true;
            fs::copy(&cur, &inst).map_err(|e| format!("copy to {}: {e}", inst.display()))?;
        }
        if let Some(cli) = cur.parent().map(|p| p.join(CLI)).filter(|p| p.exists()) {
            let _ = fs::copy(cli, dir.join(CLI));
        }
    }
    if let Err(e) = create_shortcut(&inst, &shortcut_path()) {
        crate::log_warn!("start menu shortcut: {e}");
    }
    register_uninstall(&inst)?;
    crate::log_info!("installed to {}", dir.display());
    Ok(stopped)
}

fn create_shortcut(target: &Path, lnk: &Path) -> Result<(), String> {
    if let Some(dir) = lnk.parent() {
        let _ = fs::create_dir_all(dir);
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let t = wide(&target.display().to_string());
        let dir = wide(&target.parent().unwrap_or(target).display().to_string());
        link.SetPath(PCWSTR(t.as_ptr())).map_err(|e| e.to_string())?;
        link.SetArguments(w!("--settings")).map_err(|e| e.to_string())?;
        link.SetWorkingDirectory(PCWSTR(dir.as_ptr())).map_err(|e| e.to_string())?;
        link.SetDescription(w!("deadbeef tiling window manager — settings")).map_err(|e| e.to_string())?;
        link.SetIconLocation(PCWSTR(t.as_ptr()), 0).map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let p = wide(&lnk.display().to_string());
        file.Save(PCWSTR(p.as_ptr()), true).map_err(|e| e.to_string())
    }
}

fn register_uninstall(exe: &Path) -> Result<(), String> {
    let key = wide(UNINSTALL_KEY);
    let key = PCWSTR(key.as_ptr());
    let exe_s = exe.display().to_string();
    let dir = install_dir().display().to_string();
    for (name, value) in [
        (w!("DisplayName"), "deadbeef"),
        (w!("DisplayVersion"), env!("CARGO_PKG_VERSION")),
        (w!("Publisher"), "deadbeef"),
        (w!("DisplayIcon"), exe_s.as_str()),
        (w!("InstallLocation"), dir.as_str()),
        (w!("UninstallString"), &format!("\"{exe_s}\" --uninstall")),
    ] {
        set_string(key, name, value)?;
    }
    for name in [w!("NoModify"), w!("NoRepair")] {
        let one = 1u32;
        unsafe {
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_DWORD.0, Some((&one as *const u32).cast()), 4);
        }
    }
    Ok(())
}

// ──────────────────────────────── reset ────────────────────────────────

/// Back to plain Windows: stop tiling (windows and taskbar are restored),
/// no start-up entry, and the profile reset to the template (the old one
/// is kept in backups\).
pub fn reset_everything(paths: &Paths) -> Result<String, String> {
    let stop = stop_wm();
    // A crashed WM may have left the taskbar hidden; this restores it.
    drop(crate::shell::Taskbar::new(&paths.root));
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
    }
    let _ = fs::remove_file(paths.root.join(AUTOSTART_OFF_MARKER));
    set_disabled(paths, true);
    let msg = if paths.active_conf().exists() {
        profile::apply(paths, profile::DEFAULT_TEMPLATE)?
    } else {
        String::new()
    };
    stop?;
    crate::log_info!("reset everything: {msg}");
    Ok(msg)
}

/// `deadbeef.exe --uninstall` (Apps & features): reset, remove the shortcut,
/// registry entries and install folder. Settings in %APPDATA% are kept.
pub fn uninstall(paths: &Paths) -> Result<(), String> {
    let r = reset_everything(paths);
    let _ = fs::remove_file(shortcut_path());
    let key = wide(UNINSTALL_KEY);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()));
    }
    let dir = install_dir();
    if dir.exists() {
        // This exe may be the one being deleted: let cmd remove the folder
        // a moment after we exit.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd.exe")
            .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    r.map(|_| ())
}
