//! Shared drawing layer for deadbeef's own windows (bar, flyouts, wallpaper
//! picker, wallpaper transition).
//!
//! A [`Canvas`] is a `WS_EX_LAYERED` popup. Each frame is rendered by Direct2D
//! into a 32bpp premultiplied DIB through an `ID2D1DCRenderTarget`, text comes
//! from DirectWrite, and `UpdateLayeredWindow(ULW_ALPHA)` presents it with
//! per-pixel alpha (fully transparent pixels are click-through).

pub mod cheatsheet;
pub mod image;

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{w, PCWSTR};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, RegisterClassW, ShowWindow, UpdateLayeredWindow, SW_HIDE, SW_SHOWNOACTIVATE,
    ULW_ALPHA, WINDOW_EX_STYLE, WNDCLASSW, WNDPROC, WS_EX_LAYERED, WS_POPUP,
};

use crate::config::types::Rgba;
use crate::win::{hwnd, raw, wide};

/// Segoe Fluent Icons (Windows 11) / Segoe MDL2 Assets (Windows 10) glyphs.
pub mod glyph {
    pub const WIFI: char = '\u{E701}';
    pub const BLUETOOTH: char = '\u{E702}';
    pub const VOLUME: char = '\u{E767}';
    pub const MUTE: char = '\u{E74F}';
    pub const POWER: char = '\u{E7E8}';
    pub const LOCK: char = '\u{E72E}';
    pub const SLEEP: char = '\u{E708}';
    pub const RESTART: char = '\u{E777}';
    pub const CALENDAR: char = '\u{E787}';
    pub const CPU: char = '\u{E950}';
    pub const RAM: char = '\u{E964}';
    pub const TEMP: char = '\u{E9CA}';
    pub const HOME: char = '\u{E80F}';
    pub const SETTINGS: char = '\u{E713}';
    pub const IMAGE: char = '\u{E91B}';
    pub const DOWNLOAD: char = '\u{E896}';
    pub const SEARCH: char = '\u{E71E}';
    pub const FOLDER: char = '\u{E8B7}';

    /// Battery glyph for a 0-100 level.
    pub fn battery(pct: u8, charging: bool) -> char {
        let step = (pct.min(100) as u32 + 5) / 10; // 0..=10
        let cp = if charging {
            0xE85A + step.min(8)
        } else if step >= 10 {
            0xE83F
        } else {
            0xE850 + step
        };
        char::from_u32(cp).unwrap_or('?')
    }
}

pub fn icon_font() -> &'static str {
    if crate::win::is_windows11() {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

pub fn text_font() -> &'static str {
    if crate::win::is_windows11() {
        "Segoe UI Variable Text"
    } else {
        "Segoe UI"
    }
}

pub fn color(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

pub fn with_alpha(c: Rgba, a: u8) -> Rgba {
    Rgba { a, ..c }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    pub fn inset(&self, dx: f32, dy: f32) -> RectF {
        RectF::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Hash, PartialEq, Eq)]
struct FormatKey {
    family: String,
    size_x10: u32,
    bold: bool,
    align: Align,
    wrap: bool,
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
    }
}

pub struct Canvas {
    pub hwnd: isize,
    _factory: ID2D1Factory,
    pub rt: ID2D1DCRenderTarget,
    dwrite: IDWriteFactory,
    brush: ID2D1SolidColorBrush,
    formats: HashMap<FormatKey, IDWriteTextFormat>,
    surface: Option<Surface>,
    visible: bool,
    pub w: i32,
    pub h: i32,
}

/// Registers a window class once per name (re-registration fails harmlessly).
pub fn register_class(name: PCWSTR, proc_: WNDPROC) {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: proc_,
            hInstance: instance.into(),
            lpszClassName: name,
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            )
            .unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

impl Canvas {
    /// Creates a hidden layered popup of an already-registered class.
    pub fn new(class: PCWSTR, title: &str, ex_style: WINDOW_EX_STYLE, software: bool) -> Result<Canvas, String> {
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let t = wide(title);
            let h = CreateWindowExW(
                WS_EX_LAYERED | ex_style,
                class,
                PCWSTR(t.as_ptr()),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                instance,
                None,
            )
            .map_err(|e| format!("CreateWindowExW({title}): {e}"))?;
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).map_err(|e| format!("D2D1CreateFactory: {e}"))?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: if software { D2D1_RENDER_TARGET_TYPE_SOFTWARE } else { D2D1_RENDER_TARGET_TYPE_DEFAULT },
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = factory.CreateDCRenderTarget(&props).map_err(|e| format!("CreateDCRenderTarget: {e}"))?;
            // ClearType needs an opaque background; layered windows have alpha.
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let dwrite: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(|e| format!("DWriteCreateFactory: {e}"))?;
            let brush = rt
                .CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)
                .map_err(|e| format!("CreateSolidColorBrush: {e}"))?;
            Ok(Canvas {
                hwnd: raw(h),
                _factory: factory,
                rt,
                dwrite,
                brush,
                formats: HashMap::new(),
                surface: None,
                visible: false,
                w: 0,
                h: 0,
            })
        }
    }

    fn ensure_surface(&mut self, w: i32, h: i32) -> Result<(), String> {
        if matches!(&self.surface, Some(s) if s.w == w && s.h == h) {
            return Ok(());
        }
        self.surface = None;
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| {
                let _ = DeleteDC(dc);
                format!("CreateDIBSection: {e}")
            })?;
            let old = SelectObject(dc, bitmap);
            self.surface = Some(Surface { dc, bitmap, old, w, h });
        }
        Ok(())
    }

    /// Starts a frame of `w`×`h` pixels cleared to transparent.
    pub fn begin(&mut self, w: i32, h: i32) -> Result<(), String> {
        let (w, h) = (w.max(1), h.max(1));
        self.ensure_surface(w, h)?;
        self.w = w;
        self.h = h;
        let dc = self.surface.as_ref().unwrap().dc;
        unsafe {
            self.rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h }).map_err(|e| format!("BindDC: {e}"))?;
            self.rt.BeginDraw();
            self.rt.SetTransform(&Matrix3x2::identity());
            self.rt.Clear(Some(&D2D1_COLOR_F::default()));
        }
        Ok(())
    }

    /// Finishes the frame and presents it at screen position (x, y).
    pub fn end(&mut self, x: i32, y: i32, alpha: u8) -> Result<(), String> {
        unsafe { self.rt.EndDraw(None, None).map_err(|e| format!("EndDraw: {e}"))? };
        self.present(x, y, alpha)
    }

    /// Re-presents the last frame (e.g. a fade that only changes `alpha`).
    pub fn present(&mut self, x: i32, y: i32, alpha: u8) -> Result<(), String> {
        let Some(s) = &self.surface else { return Ok(()) };
        unsafe {
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: alpha,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(
                hwnd(self.hwnd),
                None,
                Some(&POINT { x, y }),
                Some(&SIZE { cx: s.w, cy: s.h }),
                s.dc,
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .map_err(|e| format!("UpdateLayeredWindow: {e}"))?;
            if !self.visible {
                let _ = ShowWindow(hwnd(self.hwnd), SW_SHOWNOACTIVATE);
                self.visible = true;
            }
        }
        Ok(())
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(hwnd(self.hwnd), SW_HIDE);
            }
            self.visible = false;
        }
    }

    fn set_color(&self, c: Rgba) {
        unsafe { self.brush.SetColor(&color(c)) };
    }

    pub fn fill_rect(&self, r: RectF, c: Rgba) {
        self.set_color(c);
        unsafe { self.rt.FillRectangle(&r.d2d(), &self.brush) };
    }

    pub fn fill_rrect(&self, r: RectF, radius: f32, c: Rgba) {
        self.set_color(c);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillRoundedRectangle(&rr, &self.brush) };
    }

    pub fn stroke_rrect(&self, r: RectF, radius: f32, width: f32, c: Rgba) {
        self.set_color(c);
        let r = r.inset(width / 2.0, width / 2.0);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.DrawRoundedRectangle(&rr, &self.brush, width, None) };
    }

    /// A linear gradient across `r` (0deg = left→right, counter-clockwise,
    /// y up — the same convention as the border overlay).
    fn gradient(&self, r: RectF, stops: &[Rgba], angle_deg: f32) -> Option<ID2D1LinearGradientBrush> {
        let n = stops.len();
        let stops: Vec<D2D1_GRADIENT_STOP> = stops
            .iter()
            .enumerate()
            .map(|(i, c)| D2D1_GRADIENT_STOP { position: if n <= 1 { 0.0 } else { i as f32 / (n - 1) as f32 }, color: color(*c) })
            .collect();
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let a = angle_deg.to_radians();
        let half = (r.w * r.w + r.h * r.h).sqrt() / 2.0;
        let (dx, dy) = (a.cos() * half, -a.sin() * half);
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: D2D_POINT_2F { x: cx - dx, y: cy - dy },
            endPoint: D2D_POINT_2F { x: cx + dx, y: cy + dy },
        };
        unsafe {
            let coll = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
            self.rt.CreateLinearGradientBrush(&props, None, &coll).ok()
        }
    }

    pub fn fill_rrect_gradient(&self, r: RectF, radius: f32, stops: &[Rgba], angle_deg: f32) {
        let Some(b) = self.gradient(r, stops, angle_deg) else { return };
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillRoundedRectangle(&rr, &b) };
    }

    pub fn stroke_rrect_gradient(&self, r: RectF, radius: f32, width: f32, stops: &[Rgba], angle_deg: f32) {
        let Some(b) = self.gradient(r, stops, angle_deg) else { return };
        let r = r.inset(width / 2.0, width / 2.0);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.DrawRoundedRectangle(&rr, &b, width, None) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
        self.set_color(c);
        unsafe {
            self.rt.DrawLine(D2D_POINT_2F { x: x0, y: y0 }, D2D_POINT_2F { x: x1, y: y1 }, &self.brush, width, None)
        };
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Rgba) {
        self.set_color(c);
        let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, &self.brush) };
    }

    fn format(&mut self, family: &str, size: f32, bold: bool, align: Align) -> Option<IDWriteTextFormat> {
        self.format_ex(family, size, bold, align, false)
    }

    fn format_ex(&mut self, family: &str, size: f32, bold: bool, align: Align, wrap: bool) -> Option<IDWriteTextFormat> {
        let key = FormatKey { family: family.to_string(), size_x10: (size * 10.0) as u32, bold, align, wrap };
        if let Some(f) = self.formats.get(&key) {
            return Some(f.clone());
        }
        unsafe {
            let fam = wide(family);
            let f = self
                .dwrite
                .CreateTextFormat(
                    PCWSTR(fam.as_ptr()),
                    None,
                    if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
                .ok()?;
            let _ = f.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            if wrap {
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            } else {
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let trimming =
                    DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&f) {
                    let _ = f.SetTrimming(&trimming, &sign);
                }
            }
            self.formats.insert(key, f.clone());
            Some(f)
        }
    }

    /// Draws single-line text vertically centred in `r`, trimmed with an ellipsis.
    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, s: &str, r: RectF, family: &str, size: f32, bold: bool, align: Align, c: Rgba) {
        let Some(f) = self.format(family, size, bold, align) else { return };
        let t: Vec<u16> = s.encode_utf16().collect();
        self.set_color(c);
        unsafe {
            self.rt.DrawText(&t, &f, &r.d2d(), &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    /// Draws word-wrapped text starting at (x, y), `w` wide; returns its height.
    #[allow(clippy::too_many_arguments)]
    pub fn paragraph(&mut self, s: &str, x: f32, y: f32, w: f32, family: &str, size: f32, c: Rgba) -> f32 {
        let Some(f) = self.format_ex(family, size, false, Align::Left, true) else { return 0.0 };
        let t: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&t, &f, w.max(1.0), 10_000.0) else { return 0.0 };
            self.set_color(c);
            self.rt.DrawTextLayout(D2D_POINT_2F { x, y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() {
                m.height
            } else {
                size * 1.4
            }
        }
    }

    /// Whether a font family is installed (DirectWrite system collection).
    pub fn has_font(&self, family: &str) -> bool {
        unsafe {
            let mut coll = None;
            if self.dwrite.GetSystemFontCollection(&mut coll, false).is_err() {
                return false;
            }
            let Some(coll) = coll else { return false };
            let name = wide(family);
            let (mut idx, mut exists) = (0u32, windows::Win32::Foundation::BOOL(0));
            coll.FindFamilyName(PCWSTR(name.as_ptr()), &mut idx, &mut exists).is_ok() && exists.as_bool()
        }
    }

    /// Width of single-line text in pixels.
    pub fn measure(&mut self, s: &str, family: &str, size: f32, bold: bool) -> f32 {
        let Some(f) = self.format(family, size, bold, Align::Left) else { return 0.0 };
        let t: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            match self.dwrite.CreateTextLayout(&t, &f, 10_000.0, 1_000.0) {
                Ok(layout) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    if layout.GetMetrics(&mut m).is_ok() {
                        m.widthIncludingTrailingWhitespace
                    } else {
                        0.0
                    }
                }
                Err(_) => 0.0,
            }
        }
    }

    /// Uploads BGRA (premultiplied) pixels as a bitmap for this canvas.
    pub fn bitmap(&self, w: u32, h: u32, bgra: &[u8]) -> Option<ID2D1Bitmap> {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            self.rt
                .CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(bgra.as_ptr() as *const c_void), w * 4, &props)
                .ok()
        }
    }

    pub fn draw_bitmap(&self, b: &ID2D1Bitmap, dest: RectF, opacity: f32) {
        unsafe {
            self.rt.DrawBitmap(b, Some(&dest.d2d()), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
        }
    }

    /// Fills an ellipse with a bitmap placed at (bx, by) — used by the
    /// wallpaper "grow" transition.
    pub fn fill_ellipse_with_bitmap(&self, b: &ID2D1Bitmap, bx: f32, by: f32, cx: f32, cy: f32, r: f32) {
        unsafe {
            if let Ok(brush) = self.rt.CreateBitmapBrush(b, None, None) {
                brush.SetTransform(&Matrix3x2::translation(bx, by));
                let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: r, radiusY: r };
                self.rt.FillEllipse(&e, &brush);
            }
        }
    }

    pub fn push_clip(&self, r: RectF) {
        unsafe { self.rt.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }

    pub fn pop_clip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() };
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        self.surface = None;
        unsafe {
            let _ = DestroyWindow(hwnd(self.hwnd));
        }
    }
}
