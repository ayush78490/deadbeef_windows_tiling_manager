//! Workspaces are real Windows virtual desktops.
//!
//! * `IVirtualDesktopManager` (documented COM API) answers "is this window on
//!   the current desktop?" for the tiler.
//! * Switching desktops and moving *other processes'* windows is not exposed by
//!   the documented interface, so those go through `winvd`, which drives the
//!   shell's IVirtualDesktopManagerInternal / IApplicationViewCollection.
//!
//! Workspace N maps to desktop index N-1; missing desktops are created.

use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

use crate::win::hwnd;

pub struct Workspaces {
    vdm: Option<IVirtualDesktopManager>,
}

impl Workspaces {
    /// Must be called on a COM-initialised thread.
    pub fn new() -> Self {
        let vdm = unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) }
            .map_err(|e| crate::log_warn!("IVirtualDesktopManager unavailable: {e}"))
            .ok();
        Self { vdm }
    }

    /// Whether `h` belongs to the desktop currently shown. Windows the
    /// manager cannot answer for (e.g. pinned or shell-owned) count as current.
    pub fn is_on_current(&self, h: isize) -> bool {
        match &self.vdm {
            Some(vdm) => unsafe { vdm.IsWindowOnCurrentVirtualDesktop(hwnd(h)) }.map(|b| b.as_bool()).unwrap_or(true),
            None => true,
        }
    }

    /// 1-based workspace number of the current desktop.
    pub fn current(&self) -> Option<u32> {
        winvd::get_current_desktop().and_then(|d| d.get_index()).ok().map(|i| i + 1)
    }

    pub fn count(&self) -> u32 {
        winvd::get_desktop_count().unwrap_or(1)
    }

    /// 1-based workspace of a window.
    pub fn of_window(&self, h: isize) -> Option<u32> {
        winvd::get_desktop_by_window(hwnd(h)).and_then(|d| d.get_index()).ok().map(|i| i + 1)
    }

    fn ensure(&self, n: u32) -> Result<(), String> {
        let mut count = self.count();
        while count < n {
            winvd::create_desktop().map_err(|e| format!("create desktop: {e:?}"))?;
            count += 1;
        }
        Ok(())
    }

    /// Resolves `N`, `+1`/`-1` (wraps over existing desktops) and
    /// `e+1`/`e-1` (relative, never wraps, creates as needed).
    pub fn resolve(&self, arg: &str) -> Result<u32, String> {
        let arg = arg.trim();
        let cur = self.current().unwrap_or(1) as i64;
        let count = self.count() as i64;
        let n = if let Some(rel) = arg.strip_prefix('e') {
            let d: i64 = rel.parse().map_err(|_| format!("invalid workspace '{arg}'"))?;
            (cur + d).max(1)
        } else if arg.starts_with('+') || arg.starts_with('-') {
            let d: i64 = arg.parse().map_err(|_| format!("invalid workspace '{arg}'"))?;
            (cur - 1 + d).rem_euclid(count.max(1)) + 1
        } else {
            arg.parse::<i64>().map_err(|_| format!("invalid workspace '{arg}'"))?
        };
        if !(1..=64).contains(&n) {
            return Err(format!("workspace {n} out of range (1-64)"));
        }
        Ok(n as u32)
    }

    pub fn switch(&self, n: u32) -> Result<(), String> {
        self.ensure(n)?;
        winvd::switch_desktop(n - 1).map_err(|e| format!("switch desktop: {e:?}"))
    }

    pub fn move_window(&self, h: isize, n: u32) -> Result<(), String> {
        self.ensure(n)?;
        winvd::move_window_to_desktop(n - 1, &hwnd(h)).map_err(|e| format!("move window: {e:?}"))
    }

    pub fn pin(&self, h: isize, on: bool) -> Result<(), String> {
        let r = if on { winvd::pin_window(hwnd(h)) } else { winvd::unpin_window(hwnd(h)) };
        r.map_err(|e| format!("pin window: {e:?}"))
    }

    pub fn is_pinned(&self, h: isize) -> bool {
        winvd::is_pinned_window(hwnd(h)).unwrap_or(false)
    }
}
