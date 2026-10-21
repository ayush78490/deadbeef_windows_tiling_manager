//! Animated wallpaper change.
//!
//! The new image is decoded per monitor with the same centre-crop "Fill"
//! framing Windows uses, drawn into a layered window that sits directly above
//! the desktop (below every application window), and revealed with a
//! bezier-eased `grow` (circle from the cursor), `wipe` or `fade`. When the
//! animation finishes, the wallpaper is committed with `IDesktopWallpaper`
//! and the overlay removed — the desktop underneath now shows the same image.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use windows::core::w;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindow, SetWindowPos, GW_HWNDPREV, HWND_TOP, SWP_NOACTIVATE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT,
};

use super::desktop;
use crate::animation::CubicBezier;
use crate::config::types::WallpaperConfig;
use crate::layout::Rect;
use crate::ui::{self, image, Canvas, RectF};
use crate::win;

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Upper bound on waiting for explorer to apply the wallpaper.
const MAX_HOLD: Duration = Duration::from_secs(8);
const FADE_OUT: Duration = Duration::from_millis(400);

/// Explorer writes `%APPDATA%\Microsoft\Windows\Themes\TranscodedWallpaper`
/// once it has processed a new wallpaper; wait for that (plus a repaint).
fn wait_for_explorer(since: SystemTime) {
    let themes = crate::win::roaming_appdata().join(r"Microsoft\Windows\Themes");
    // Newer builds keep a per-image TranscodedWallpaperCache: an image that
    // was processed before only gets its `.meta` touched.
    let touched = || {
        let fresh = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t >= since);
        fresh(&themes.join("TranscodedWallpaper"))
            || std::fs::read_dir(themes.join("TranscodedWallpaperCache"))
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| fresh(&e.path()))
    };
    let start = Instant::now();
    while start.elapsed() < MAX_HOLD {
        let updated = touched();
        if updated {
            std::thread::sleep(Duration::from_millis(350));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    crate::log_warn!("explorer did not confirm the wallpaper within {MAX_HOLD:?}");
}

/// Top-left of the virtual screen (the overlay spans all monitors).
fn virtual_origin() -> (i32, i32) {
    let monitors = win::monitors();
    (
        monitors.iter().map(|m| m.rect.x).min().unwrap_or(0),
        monitors.iter().map(|m| m.rect.y).min().unwrap_or(0),
    )
}

unsafe extern "system" fn proc_(
    h: windows::Win32::Foundation::HWND,
    m: u32,
    w: windows::Win32::Foundation::WPARAM,
    l: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(h, m, w, l)
}

/// The window just above the desktop layer (Progman / WorkerW), so the
/// overlay can be inserted beneath every application window.
fn insert_after() -> Option<isize> {
    let progman = unsafe { FindWindowW(w!("Progman"), None) }.ok()?;
    let mut top = win::raw(progman);
    loop {
        let prev = unsafe { GetWindow(win::hwnd(top), GW_HWNDPREV) }.ok().filter(|p| !p.is_invalid());
        match prev {
            Some(p) if matches!(win::class_name(win::raw(p)).as_str(), "WorkerW" | "Progman") => top = win::raw(p),
            Some(p) => return Some(win::raw(p)),
            None => return None,
        }
    }
}

/// Applies `path` with the configured transition. Blocks until done.
/// Must be called on a COM-initialised thread.
pub fn run(path: &Path, cfg: &WallpaperConfig, curve: [f32; 4]) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("a wallpaper transition is already running".into());
    }
    let overlay = animate(path, cfg, curve);
    // Commit even if the animation failed — the user asked for this wallpaper.
    let committed_at = SystemTime::now();
    let set = desktop::set(path);
    match overlay {
        Ok(Some(mut canvas)) => {
            // SetWallpaper returns before explorer has decoded and painted the
            // image (1 s for a JPEG, 5 s+ for a large PNG). The overlay is below
            // every app window and click-through, so holding it is free: keep
            // the finished frame up until explorer is done, then fade it out.
            wait_for_explorer(committed_at);
            let (ox, oy) = virtual_origin();
            let start = Instant::now();
            loop {
                let t = (start.elapsed().as_secs_f32() / FADE_OUT.as_secs_f32()).min(1.0);
                let _ = canvas.present(ox, oy, ((1.0 - t) * 255.0) as u8);
                if t >= 1.0 {
                    break;
                }
                if !win::dwm_flush() {
                    std::thread::sleep(Duration::from_millis(16));
                }
            }
            drop(canvas);
        }
        Ok(None) => {}
        Err(e) => crate::log_warn!("wallpaper transition skipped: {e}"),
    }
    RUNNING.store(false, Ordering::SeqCst);
    set
}

fn animate(path: &Path, cfg: &WallpaperConfig, curve: [f32; 4]) -> Result<Option<Canvas>, String> {
    if cfg.transition == "none" || cfg.duration <= 0.0 {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let monitors = win::monitors();
    if monitors.is_empty() {
        return Err("no monitors".into());
    }
    let left = monitors.iter().map(|m| m.rect.x).min().unwrap();
    let top = monitors.iter().map(|m| m.rect.y).min().unwrap();
    let right = monitors.iter().map(|m| m.rect.right()).max().unwrap();
    let bottom = monitors.iter().map(|m| m.rect.bottom()).max().unwrap();
    let virt = Rect::new(left, top, right - left, bottom - top);

    ui::register_class(w!("deadbeef_wallfx"), Some(proc_));
    let mut canvas = Canvas::new(w!("deadbeef_wallfx"), "deadbeef wallpaper", WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT, false)?;
    let mut layers = Vec::new();
    for m in &monitors {
        let px = image::decode_fill(&bytes, m.rect.w as u32, m.rect.h as u32)?;
        let bmp = canvas.bitmap(px.w, px.h, &px.data).ok_or("could not upload wallpaper bitmap")?;
        layers.push((m.rect, bmp));
    }

    // Slot the overlay in directly above the desktop before it is shown.
    let after = insert_after().map(win::hwnd).unwrap_or(HWND_TOP);
    unsafe {
        let _ = SetWindowPos(win::hwnd(canvas.hwnd), after, virt.x, virt.y, virt.w, virt.h, SWP_NOACTIVATE);
    }

    let ease = CubicBezier::from_points(curve);
    let (cx, cy) = win::cursor_pos();
    let duration = Duration::from_secs_f32(cfg.duration);
    let start = Instant::now();
    let mut faded_once = false;
    loop {
        let t = (start.elapsed().as_secs_f32() / duration.as_secs_f32()).min(1.0);
        let e = ease.ease(t);
        if cfg.transition == "fade" {
            // One render; each frame only changes the window's constant alpha.
            if !faded_once {
                canvas.begin(virt.w, virt.h)?;
                for (r, b) in &layers {
                    canvas.draw_bitmap(b, RectF::new((r.x - virt.x) as f32, (r.y - virt.y) as f32, r.w as f32, r.h as f32), 1.0);
                }
                canvas.end(virt.x, virt.y, 0)?;
                faded_once = true;
            }
            canvas.present(virt.x, virt.y, (e.clamp(0.0, 1.0) * 255.0) as u8)?;
        } else {
            canvas.begin(virt.w, virt.h)?;
            for (r, b) in &layers {
                let (ox, oy) = ((r.x - virt.x) as f32, (r.y - virt.y) as f32);
                if cfg.transition == "wipe" {
                    canvas.push_clip(RectF::new(ox, oy, r.w as f32 * e.clamp(0.0, 1.0), r.h as f32));
                    canvas.draw_bitmap(b, RectF::new(ox, oy, r.w as f32, r.h as f32), 1.0);
                    canvas.pop_clip();
                } else {
                    // grow: a circle from the cursor (or the monitor centre).
                    let (gx, gy) = if r.contains(cx, cy) { (cx, cy) } else { r.center() };
                    let (lx, ly) = ((gx - virt.x) as f32, (gy - virt.y) as f32);
                    let far = [(ox, oy), (ox + r.w as f32, oy), (ox, oy + r.h as f32), (ox + r.w as f32, oy + r.h as f32)]
                        .iter()
                        .map(|(x, y)| ((x - lx).powi(2) + (y - ly).powi(2)).sqrt())
                        .fold(0.0f32, f32::max);
                    canvas.push_clip(RectF::new(ox, oy, r.w as f32, r.h as f32));
                    canvas.fill_ellipse_with_bitmap(b, ox, oy, lx, ly, far * e.max(0.0));
                    canvas.pop_clip();
                }
            }
            canvas.end(virt.x, virt.y, 255)?;
        }
        if t >= 1.0 {
            break;
        }
        if !win::dwm_flush() {
            std::thread::sleep(Duration::from_millis(16));
        }
    }
    Ok(Some(canvas))
}
