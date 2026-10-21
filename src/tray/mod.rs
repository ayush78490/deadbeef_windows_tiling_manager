//! Notification-area icon (`Shell_NotifyIconW`) and its context menu.

use std::mem::size_of;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, PostMessageW, SetForegroundWindow,
    TrackPopupMenu, HICON, IDI_APPLICATION, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::config::profile::ProfileList;
use crate::win::{hwnd, wide};

const TRAY_UID: u32 = 1;

pub enum MenuChoice {
    Settings,
    Reload,
    OpenFolder,
    OpenLog,
    Reset,
    Quit,
    Switch(String),
}

const ID_RELOAD: usize = 1;
const ID_FOLDER: usize = 2;
const ID_LOG: usize = 3;
const ID_RESET: usize = 4;
const ID_QUIT: usize = 5;
const ID_SETTINGS: usize = 6;
const ID_PROFILE_BASE: usize = 100;

fn copy_into(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

fn base(main: isize) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd(main),
        uID: TRAY_UID,
        ..Default::default()
    }
}

pub fn app_icon() -> HICON {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        // MAKEINTRESOURCE(1): the icon embedded by build.rs (winres).
        LoadIconW(instance, PCWSTR(std::ptr::without_provenance(1)))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default()
    }
}

pub fn add(main: isize, callback_msg: u32) {
    let mut nid = base(main);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = callback_msg;
    nid.hIcon = app_icon();
    copy_into(&mut nid.szTip, "deadbeef");
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            // Already present (e.g. after TaskbarCreated) — refresh instead.
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }
}

pub fn remove(main: isize) {
    let nid = base(main);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

pub fn set_tip(main: isize, tip: &str) {
    let mut nid = base(main);
    nid.uFlags = NIF_TIP;
    copy_into(&mut nid.szTip, tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

pub fn balloon(main: isize, title: &str, text: &str, warning: bool) {
    let mut nid = base(main);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
    copy_into(&mut nid.szInfoTitle, title);
    copy_into(&mut nid.szInfo, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// Shows the context menu at the cursor and returns the chosen item. Must be
/// called without holding the state borrow: TrackPopupMenu runs a modal loop.
pub fn show_menu(main: isize, profiles: &ProfileList) -> Option<MenuChoice> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let sub = CreatePopupMenu().ok()?;
        let mut names: Vec<String> = Vec::new();

        let header = |m, text: &str| {
            let t = wide(text);
            let _ = AppendMenuW(m, MF_STRING | MF_GRAYED, 0, PCWSTR(t.as_ptr()));
        };
        header(sub, "Templates");
        for t in &profiles.templates {
            names.push(t.clone());
            let checked = if profiles.active_origin.as_deref() == Some(t) { MF_CHECKED } else { MF_STRING };
            let label = wide(&format!("    {t}"));
            let _ = AppendMenuW(sub, MF_STRING | checked, ID_PROFILE_BASE + names.len() - 1, PCWSTR(label.as_ptr()));
        }
        let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null());
        header(sub, "Saved profiles");
        if profiles.profiles.is_empty() {
            header(sub, "    (none — dbctl profile save <name>)");
        }
        for p in &profiles.profiles {
            names.push(p.clone());
            let checked = if profiles.active_origin.as_deref() == Some(p) { MF_CHECKED } else { MF_STRING };
            let label = wide(&format!("    {p}"));
            let _ = AppendMenuW(sub, MF_STRING | checked, ID_PROFILE_BASE + names.len() - 1, PCWSTR(label.as_ptr()));
        }

        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("deadbeef"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RELOAD, w!("Reload Config"));
        let _ = AppendMenuW(menu, MF_STRING, ID_FOLDER, w!("Open Profile Folder"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LOG, w!("Open Log"));
        let _ = AppendMenuW(menu, MF_STRING | MF_POPUP, sub.0 as usize, w!("Switch Profile"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RESET, w!("Reset to nerdy-deadbeef"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit"));

        let mut pt = Default::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd(main));
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            0,
            hwnd(main),
            None,
        )
        .0 as usize;
        let _ = PostMessageW(hwnd(main), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu); // destroys the submenu too

        match cmd {
            ID_SETTINGS => Some(MenuChoice::Settings),
            ID_RELOAD => Some(MenuChoice::Reload),
            ID_FOLDER => Some(MenuChoice::OpenFolder),
            ID_LOG => Some(MenuChoice::OpenLog),
            ID_RESET => Some(MenuChoice::Reset),
            ID_QUIT => Some(MenuChoice::Quit),
            c if c >= ID_PROFILE_BASE => names.get(c - ID_PROFILE_BASE).cloned().map(MenuChoice::Switch),
            _ => None,
        }
    }
}
