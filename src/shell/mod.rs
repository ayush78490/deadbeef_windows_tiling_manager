//! Replacing the Windows taskbar with the deadbeef bar.
//!
//! The taskbar is set to auto-hide (so it stops reserving screen space) via
//! `SHAppBarMessage(ABM_SETSTATE)` and its windows are hidden with
//! `ShowWindow(SW_HIDE)`. Explorer re-shows it from time to time (Start
//! menu, hover, explorer restart); the WM re-hides it whenever a show or
//! move event for a taskbar window arrives.
//!
//! The original auto-hide state is written to `taskbar.restore` before
//! anything changes and restored on exit — or on the next start if deadbeef
//! did not exit cleanly — so the taskbar can't be lost.

use std::mem::size_of;
use std::path::PathBuf;

use windows::core::w;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::UI::Shell::{SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, ShowWindow, SW_HIDE, SW_SHOWNA};

use crate::win;

const TASKBAR_CLASSES: &[&str] = &["Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

pub struct Taskbar {
    restore_file: PathBuf,
    hidden: bool,
}

pub fn is_taskbar(h: isize) -> bool {
    TASKBAR_CLASSES.contains(&win::class_name(h).as_str())
}

fn taskbars() -> Vec<isize> {
    win::top_level_windows().into_iter().filter(|&h| is_taskbar(h)).collect()
}

fn get_autohide() -> bool {
    let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, ..Default::default() };
    let state = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) } as u32;
    state & ABS_AUTOHIDE != 0
}

fn set_autohide(on: bool) {
    let tray = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.unwrap_or_default();
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: tray,
        lParam: LPARAM(if on { ABS_AUTOHIDE as isize } else { 0 }),
        ..Default::default()
    };
    unsafe { SHAppBarMessage(ABM_SETSTATE, &mut abd) };
}

impl Taskbar {
    pub fn new(root: &std::path::Path) -> Self {
        let t = Taskbar { restore_file: root.join("taskbar.restore"), hidden: false };
        // A previous run did not exit cleanly: put things back first.
        if t.restore_file.exists() {
            crate::log_warn!("restoring taskbar state left by an unclean exit");
            t.restore_now();
        }
        t
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    pub fn hide(&mut self) {
        if !self.hidden {
            let original = get_autohide();
            if let Err(e) = std::fs::write(&self.restore_file, if original { "autohide" } else { "normal" }) {
                crate::log_error!("not hiding the taskbar: cannot write {}: {e}", self.restore_file.display());
                return;
            }
            if !original {
                set_autohide(true);
            }
            self.hidden = true;
            crate::log_info!("taskbar hidden (original state: {})", if original { "autohide" } else { "normal" });
        }
        self.enforce();
    }

    /// Re-hides every taskbar window (Explorer may have shown one again).
    pub fn enforce(&self) {
        if !self.hidden {
            return;
        }
        for h in taskbars() {
            if win::is_visible(h) {
                unsafe {
                    let _ = ShowWindow(win::hwnd(h), SW_HIDE);
                }
            }
        }
    }

    pub fn show(&mut self) {
        if self.hidden {
            self.restore_now();
            self.hidden = false;
            crate::log_info!("taskbar restored");
        }
    }

    fn restore_now(&self) {
        let original = std::fs::read_to_string(&self.restore_file).unwrap_or_default();
        for h in taskbars() {
            unsafe {
                let _ = ShowWindow(win::hwnd(h), SW_SHOWNA);
            }
        }
        if original.trim() == "normal" {
            set_autohide(false);
        }
        let _ = std::fs::remove_file(&self.restore_file);
    }
}

impl Drop for Taskbar {
    fn drop(&mut self) {
        self.show();
    }
}
