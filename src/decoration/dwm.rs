//! Windows 11 decorations through `DwmSetWindowAttribute`.

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_AUTO, DWMSBT_MAINWINDOW, DWMSBT_TABBEDWINDOW, DWMSBT_TRANSIENTWINDOW,
    DWMWA_BORDER_COLOR, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DEFAULT,
    DWMWCP_DONOTROUND, DWMWCP_ROUND, DWMWCP_ROUNDSMALL, DWMWINDOWATTRIBUTE,
};

use crate::config::types::Backdrop;
use crate::win::hwnd;

/// DWMWA_COLOR_DEFAULT: let the system pick the border colour again.
const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
/// DWMWA_COLOR_NONE: no border at all.
const COLOR_NONE: u32 = 0xFFFF_FFFE;

fn set<T>(h: isize, attr: DWMWINDOWATTRIBUTE, value: &T) -> bool {
    unsafe { DwmSetWindowAttribute(hwnd(h), attr, value as *const T as *const c_void, size_of::<T>() as u32).is_ok() }
}

/// Sets the 1px DWM border to a COLORREF (0x00BBGGRR).
pub fn set_border_color(h: isize, colorref: u32) -> bool {
    set(h, DWMWA_BORDER_COLOR, &colorref)
}

pub fn hide_border(h: isize) -> bool {
    set(h, DWMWA_BORDER_COLOR, &COLOR_NONE)
}

pub fn reset_border(h: isize) {
    set(h, DWMWA_BORDER_COLOR, &COLOR_DEFAULT);
}

/// Maps Hyprland's pixel `rounding` onto the DWM corner preferences.
pub fn set_rounding(h: isize, rounding: i32) {
    let pref = match rounding {
        0 => DWMWCP_DONOTROUND,
        1..=4 => DWMWCP_ROUNDSMALL,
        _ => DWMWCP_ROUND,
    };
    set(h, DWMWA_WINDOW_CORNER_PREFERENCE, &pref);
}

pub fn reset_rounding(h: isize) {
    set(h, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_DEFAULT);
}

/// System backdrop material (Windows 11 22H2, build 22621+). Only visible
/// where the app leaves its client area transparent.
pub fn set_backdrop(h: isize, backdrop: Option<Backdrop>) {
    let v = match backdrop {
        None => DWMSBT_AUTO,
        Some(Backdrop::Mica) => DWMSBT_MAINWINDOW,
        Some(Backdrop::Acrylic) => DWMSBT_TRANSIENTWINDOW,
        Some(Backdrop::Tabbed) => DWMSBT_TABBEDWINDOW,
    };
    set(h, DWMWA_SYSTEMBACKDROP_TYPE, &v);
}

pub fn supports_backdrop() -> bool {
    crate::win::os_build() >= 22621
}
