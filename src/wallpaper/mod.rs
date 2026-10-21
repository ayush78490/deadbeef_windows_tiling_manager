//! Wallpapers: wallhaven.cc browsing, downloads and animated application.

pub mod desktop;
pub mod http;
pub mod picker;
pub mod transition;
pub mod wallhaven;

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

use crate::config::types::{Rgba, WallpaperConfig};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Rgba,
    pub fg: Rgba,
    pub accent: Rgba,
    pub dim: Rgba,
    pub critical: Rgba,
}

/// Everything a wallpaper thread needs, cloned out of the WM config.
#[derive(Debug, Clone)]
pub struct Settings {
    pub cfg: WallpaperConfig,
    pub theme: Theme,
    pub curve: [f32; 4],
    pub dir: PathBuf,
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        f();
    });
}

/// Opens the picker (or focuses it if it is already open).
pub fn open_picker(s: Settings, query: Option<String>) {
    let open = picker::OPEN.load(Ordering::SeqCst);
    if open != 0 {
        crate::win::focus(open);
        return;
    }
    spawn("deadbeef-wallpicker", move || picker::run(s.cfg, s.theme, s.curve, s.dir, query));
}

/// Applies a local file with the configured transition (in the background).
pub fn apply(s: Settings, path: PathBuf) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    spawn("deadbeef-wallfx", move || {
        if let Err(e) = transition::run(&path, &s.cfg, s.curve) {
            crate::log_error!("wallpaper: {e}");
        }
    });
    Ok(())
}

/// Applies a random downloaded wallpaper.
pub fn random(s: Settings) -> Result<PathBuf, String> {
    let files = picker::list_local(&s.dir);
    if files.is_empty() {
        return Err(format!("no wallpapers in {} yet — download some with the picker", s.dir.display()));
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let path = files[nanos as usize % files.len()].clone();
    apply(s, path.clone())?;
    Ok(path)
}
