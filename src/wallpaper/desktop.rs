//! Setting the wallpaper through the documented `IDesktopWallpaper` COM API.

use std::path::Path;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper, DWPOS_FILL};

/// Sets `path` (Fill) on every monitor. COM must be initialised.
pub fn set(path: &Path) -> Result<(), String> {
    let path = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let p = path.to_string_lossy();
    let p = p.strip_prefix(r"\\?\").unwrap_or(&p).to_string();
    unsafe {
        let dw: IDesktopWallpaper =
            CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL).map_err(|e| format!("IDesktopWallpaper: {e}"))?;
        dw.SetPosition(DWPOS_FILL).map_err(|e| format!("SetPosition: {e}"))?;
        dw.SetWallpaper(PCWSTR::null(), &HSTRING::from(p.as_str())).map_err(|e| format!("SetWallpaper: {e}"))?;
    }
    crate::log_info!("wallpaper set: {p}");
    Ok(())
}
