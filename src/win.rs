//! Thin, safe-ish wrappers over the Win32 calls deadbeef makes.
//!
//! Window handles cross thread boundaries as `isize` (HWND is `!Send`).

use std::ffi::c_void;
use std::mem::size_of;
use std::path::PathBuf;

use serde::Serialize;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, POINT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmFlush, DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW,
    ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_MENU,
};
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Programs, FOLDERID_RoamingAppData, SHGetKnownFolderPath, ShellExecuteW,
    KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::layout::Rect;

/// `dwExtraInfo` tag on input we synthesise, so our own keyboard hook ignores it.
pub const INJECT_MARK: usize = 0xDEAD_BEEF;

#[inline]
pub fn hwnd(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

#[inline]
pub fn raw(h: HWND) -> isize {
    h.0 as isize
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn class_name(h: isize) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd(h), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn title(h: isize) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd(h), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn pid(h: isize) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd(h), Some(&mut pid)) };
    pid
}

pub fn own_pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

/// Full image path of a process.
pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn process_exe(pid: u32) -> Option<String> {
    let path = process_path(pid)?;
    Some(path.rsplit('\\').next().unwrap_or(&path).to_string())
}

/// Process that actually owns the app in the window. UWP apps are hosted by
/// ApplicationFrameHost.exe; for those, the hosted app's process is returned.
pub fn app_pid(h: isize) -> u32 {
    let own = pid(h);
    if class_name(h) == "ApplicationFrameWindow" {
        let mut found: (u32, u32) = (own, 0);
        unsafe extern "system" fn child(c: HWND, lp: LPARAM) -> BOOL {
            let st = &mut *(lp.0 as *mut (u32, u32));
            let p = pid(raw(c));
            if p != st.0 {
                st.1 = p;
                return BOOL(0);
            }
            BOOL(1)
        }
        unsafe {
            let _ = EnumChildWindows(hwnd(h), Some(child), LPARAM(&mut found as *mut _ as isize));
        }
        if found.1 != 0 {
            return found.1;
        }
    }
    own
}

/// Executable name of the app in the window (see [`app_pid`]).
pub fn exe_name(h: isize) -> String {
    process_exe(app_pid(h)).or_else(|| process_exe(pid(h))).unwrap_or_default()
}

pub fn style(h: isize) -> u32 {
    unsafe { GetWindowLongPtrW(hwnd(h), GWL_STYLE) as u32 }
}

pub fn ex_style(h: isize) -> u32 {
    unsafe { GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE) as u32 }
}

pub fn is_window(h: isize) -> bool {
    unsafe { IsWindow(hwnd(h)).as_bool() }
}

pub fn is_visible(h: isize) -> bool {
    unsafe { IsWindowVisible(hwnd(h)).as_bool() }
}

pub fn is_iconic(h: isize) -> bool {
    unsafe { IsIconic(hwnd(h)).as_bool() }
}

pub fn is_zoomed(h: isize) -> bool {
    unsafe { IsZoomed(hwnd(h)).as_bool() }
}

pub fn is_top_level(h: isize) -> bool {
    unsafe { GetAncestor(hwnd(h), GA_ROOT) == hwnd(h) }
}

pub fn owner(h: isize) -> Option<isize> {
    unsafe { GetWindow(hwnd(h), GW_OWNER).ok().filter(|o| !o.is_invalid()).map(raw) }
}

pub fn is_cloaked(h: isize) -> bool {
    let mut cloaked: u32 = 0;
    unsafe {
        DwmGetWindowAttribute(hwnd(h), DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut c_void, size_of::<u32>() as u32)
            .is_ok()
            && cloaked != 0
    }
}

pub fn foreground() -> isize {
    unsafe { raw(GetForegroundWindow()) }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

fn rect_from(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// Outer window rect, including the invisible resize borders.
pub fn window_rect(h: isize) -> Option<Rect> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd(h), &mut r).ok()? };
    Some(rect_from(r))
}

/// What the user actually sees (DWM extended frame bounds).
pub fn visual_rect(h: isize) -> Option<Rect> {
    let mut r = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd(h),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut c_void,
            size_of::<RECT>() as u32,
        )
    };
    match ok {
        Ok(()) => Some(rect_from(r)),
        Err(_) => window_rect(h),
    }
}

/// Converts a desired *visual* rect into the outer rect to pass to
/// SetWindowPos, compensating for the invisible DWM resize borders.
pub fn outer_for_visual(h: isize, visual: Rect) -> Rect {
    match (window_rect(h), visual_rect(h)) {
        (Some(w), Some(v)) => {
            let (l, t) = (v.x - w.x, v.y - w.y);
            let (r, b) = (w.right() - v.right(), w.bottom() - v.bottom());
            // Guard against bogus bounds (e.g. a window mid-minimise).
            if [l, t, r, b].iter().all(|d| (0..=32).contains(d)) {
                return Rect::new(visual.x - l, visual.y - t, visual.w + l + r, visual.h + t + b);
            }
            visual
        }
        _ => visual,
    }
}

/// SetWindowPos with an outer rect; never activates, never touches z-order,
/// and is asynchronous so a hung app cannot block the caller.
pub fn move_window_raw(h: isize, r: Rect) {
    unsafe {
        let _ = SetWindowPos(
            hwnd(h),
            None,
            r.x,
            r.y,
            r.w,
            r.h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
        );
    }
}

pub fn restore(h: isize) {
    unsafe {
        let _ = ShowWindow(hwnd(h), SW_RESTORE);
    }
}

pub fn maximize(h: isize) {
    unsafe {
        let _ = ShowWindow(hwnd(h), SW_MAXIMIZE);
    }
}

pub fn bring_to_top(h: isize) {
    unsafe {
        let _ = SetWindowPos(hwnd(h), HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
    }
}

pub fn close(h: isize) {
    unsafe {
        let _ = PostMessageW(hwnd(h), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

fn tap_key(vk: u16) {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: INJECT_MARK },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Sends an unassigned virtual key (0xE8). Used to keep the Start menu from
/// opening after a swallowed Win+key combo and to satisfy the foreground lock.
pub fn tap_mask_key() {
    tap_key(0xE8);
}

/// SetForegroundWindow, with the documented foreground-lock rules handled:
/// a background process may only take focus right after it received input,
/// so on failure we inject a harmless key and retry (then Alt, as komorebi does).
pub fn focus(h: isize) {
    unsafe {
        if is_iconic(h) {
            restore(h);
        }
        if SetForegroundWindow(hwnd(h)).as_bool() && foreground() == h {
            return;
        }
        tap_mask_key();
        if SetForegroundWindow(hwnd(h)).as_bool() && foreground() == h {
            return;
        }
        tap_key(VK_MENU.0);
        let _ = SetForegroundWindow(hwnd(h));
    }
}

/// Launches a command line through the shell, so PATH, App Paths
/// (msedge, …), app execution aliases (wt) and URIs (ms-settings:) resolve.
pub fn shell_exec(cmdline: &str) -> Result<(), String> {
    let cmd = crate::config::parser::expand_env(cmdline.trim());
    if cmd.is_empty() {
        return Err("empty command".into());
    }
    let (file, params) = split_command(&cmd);
    shell_exec_verb(&file, &params, "open")
}

/// ShellExecute with an explicit verb ("open", "runas" for administrator).
pub fn shell_exec_verb(file: &str, params: &str, verb: &str) -> Result<(), String> {
    let file_w = wide(file);
    let params_w = wide(params);
    let verb_w = wide(verb);
    let r = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(file_w.as_ptr()),
            if params.is_empty() { PCWSTR::null() } else { PCWSTR(params_w.as_ptr()) },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value > 32 on success.
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute('{file}') failed with code {}", r.0 as isize))
    }
}

/// Splits `"C:\path with spaces\app.exe" args` / `app args` into (file, params).
pub fn split_command(cmd: &str) -> (String, String) {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
    }
    match cmd.split_once(char::is_whitespace) {
        Some((f, p)) => (f.to_string(), p.trim().to_string()),
        None => (cmd.to_string(), String::new()),
    }
}

/// A shell known folder, falling back to an environment variable.
fn known_folder(id: &windows::core::GUID, env: &str) -> PathBuf {
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const c_void));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    PathBuf::from(std::env::var(env).unwrap_or_else(|_| ".".into()))
}

pub fn roaming_appdata() -> PathBuf {
    known_folder(&FOLDERID_RoamingAppData, "APPDATA")
}

pub fn local_appdata() -> PathBuf {
    known_folder(&FOLDERID_LocalAppData, "LOCALAPPDATA")
}

/// The user's Start menu "Programs" folder.
pub fn start_menu_programs() -> PathBuf {
    let p = known_folder(&FOLDERID_Programs, "APPDATA");
    if p.ends_with("Programs") {
        p
    } else {
        p.join(r"Microsoft\Windows\Start Menu\Programs")
    }
}

pub fn local_time() -> SYSTEMTIME {
    unsafe { windows::Win32::System::SystemInformation::GetLocalTime() }
}

/// Real OS build number (RtlGetVersion is not subject to manifest shims).
pub fn os_build() -> u32 {
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut info = OSVERSIONINFOW { dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    unsafe {
        let _ = windows::Wdk::System::SystemServices::RtlGetVersion(&mut info);
    }
    info.dwBuildNumber
}

pub fn is_windows11() -> bool {
    os_build() >= 22000
}

/// Blocks until the next DWM composition pass. Returns false if unavailable.
pub fn dwm_flush() -> bool {
    unsafe { DwmFlush().is_ok() }
}

#[derive(Debug, Clone, Serialize)]
pub struct Monitor {
    pub index: usize,
    #[serde(skip)]
    pub handle: isize,
    pub name: String,
    pub primary: bool,
    pub rect: Rect,
    pub work: Rect,
    pub dpi: u32,
    pub scale: f32,
    pub refresh_hz: u32,
}

/// Enumerates monitors: primary is index 0, the rest left-to-right, top-to-bottom.
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let v = &mut *(lp.0 as *mut Vec<isize>);
        v.push(m.0 as isize);
        BOOL(1)
    }
    let mut handles: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut handles as *mut _ as isize));
    }
    let mut out: Vec<Monitor> = handles.into_iter().filter_map(monitor_info).collect();
    out.sort_by_key(|m| (!m.primary, m.rect.x, m.rect.y));
    for (i, m) in out.iter_mut().enumerate() {
        m.index = i;
    }
    out
}

fn monitor_info(handle: isize) -> Option<Monitor> {
    let hmon = HMONITOR(handle as *mut c_void);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    unsafe {
        if !GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            return None;
        }
    }
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    }
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    let refresh = unsafe {
        if EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
            dm.dmDisplayFrequency
        } else {
            60
        }
    };
    Some(Monitor {
        index: 0,
        handle,
        name: from_wide(&info.szDevice),
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        rect: rect_from(info.monitorInfo.rcMonitor),
        work: rect_from(info.monitorInfo.rcWork),
        dpi: dx,
        scale: dx as f32 / 96.0,
        refresh_hz: if refresh <= 1 { 60 } else { refresh },
    })
}

/// Applies an explicit `monitor = N, WxH@HZ, XxY, …` rule with
/// ChangeDisplaySettingsExW. `preferred`/`auto` never touch the display, and
/// nothing is changed when the monitor already matches.
pub fn apply_display_mode(m: &Monitor, rule: &crate::config::types::MonitorRule) -> Result<(), String> {
    use crate::config::types::{MonitorMode, MonitorPos};
    use windows::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, CDS_TYPE, CDS_NORESET, CDS_UPDATEREGISTRY, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY,
        DM_PELSHEIGHT, DM_PELSWIDTH, DM_POSITION,
    };
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    let device = wide(&m.name);
    unsafe {
        if !EnumDisplaySettingsW(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
            return Err(format!("EnumDisplaySettingsW({}) failed", m.name));
        }
    }
    let mut fields = 0u32;
    if let MonitorMode::Mode { width, height, refresh } = rule.mode {
        if dm.dmPelsWidth != width || dm.dmPelsHeight != height {
            dm.dmPelsWidth = width;
            dm.dmPelsHeight = height;
            fields |= DM_PELSWIDTH.0 | DM_PELSHEIGHT.0;
        }
        if let Some(hz) = refresh {
            if dm.dmDisplayFrequency != hz {
                dm.dmDisplayFrequency = hz;
                fields |= DM_DISPLAYFREQUENCY.0;
            }
        }
    }
    // The primary monitor defines the origin, so its position is fixed.
    if let MonitorPos::At(x, y) = rule.position {
        if !m.primary && (m.rect.x != x || m.rect.y != y) {
            dm.Anonymous1.Anonymous2.dmPosition.x = x;
            dm.Anonymous1.Anonymous2.dmPosition.y = y;
            fields |= DM_POSITION.0;
        }
    }
    if fields == 0 {
        return Ok(());
    }
    dm.dmFields = windows::Win32::Graphics::Gdi::DEVMODE_FIELD_FLAGS(fields);
    crate::log_info!("monitor {}: applying display mode change (fields {fields:#x})", m.index);
    unsafe {
        let r = ChangeDisplaySettingsExW(
            PCWSTR(device.as_ptr()),
            Some(&dm),
            None,
            CDS_UPDATEREGISTRY | CDS_NORESET,
            None,
        );
        if r != DISP_CHANGE_SUCCESSFUL {
            return Err(format!("ChangeDisplaySettingsExW returned {}", r.0));
        }
        let r = ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None);
        if r != DISP_CHANGE_SUCCESSFUL {
            return Err(format!("ChangeDisplaySettingsExW(apply) returned {}", r.0));
        }
    }
    Ok(())
}

pub fn monitor_of_window(h: isize) -> isize {
    unsafe { MonitorFromWindow(hwnd(h), MONITOR_DEFAULTTONEAREST).0 as isize }
}

/// Toggles per-window alpha via WS_EX_LAYERED + SetLayeredWindowAttributes.
/// Returns true if this call added WS_EX_LAYERED (so it can be removed later).
pub fn set_opacity(h: isize, alpha: f32) -> bool {
    let ex = ex_style(h);
    let had = ex & WS_EX_LAYERED.0 != 0;
    unsafe {
        if !had {
            SetWindowLongPtrW(hwnd(h), GWL_EXSTYLE, (ex | WS_EX_LAYERED.0) as isize);
        }
        let _ = SetLayeredWindowAttributes(
            hwnd(h),
            windows::Win32::Foundation::COLORREF(0),
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
            LWA_ALPHA,
        );
    }
    !had
}

pub fn clear_layered(h: isize) {
    let ex = ex_style(h);
    unsafe {
        SetWindowLongPtrW(hwnd(h), GWL_EXSTYLE, (ex & !WS_EX_LAYERED.0) as isize);
    }
}

/// All top-level windows in z-order.
pub fn top_level_windows() -> Vec<isize> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let v = &mut *(lp.0 as *mut Vec<isize>);
        v.push(raw(h));
        BOOL(1)
    }
    let mut v: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_commands() {
        assert_eq!(split_command("wt"), ("wt".into(), "".into()));
        assert_eq!(split_command("explorer.exe ms-screenclip:"), ("explorer.exe".into(), "ms-screenclip:".into()));
        assert_eq!(
            split_command("\"C:\\Program Files\\x.exe\" --a b"),
            ("C:\\Program Files\\x.exe".into(), "--a b".into())
        );
    }
}
