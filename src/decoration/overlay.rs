//! Gradient border overlay (Windows 10, or `general:border_overlay = on`).
//!
//! A click-through `WS_EX_LAYERED | WS_EX_TRANSPARENT` popup sits directly
//! above the focused window in z-order. Direct2D renders the rounded
//! gradient stroke into a 32bpp premultiplied DIB via an
//! `ID2D1DCRenderTarget`, and `UpdateLayeredWindow(ULW_ALPHA)` composites it
//! with per-pixel alpha.

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindow, RegisterClassW, SetWindowPos, ShowWindow, GW_HWNDPREV,
    HWND_TOP, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, UpdateLayeredWindow,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::config::types::Gradient;
use crate::layout::Rect;
use crate::win::{hwnd, raw};

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

#[derive(PartialEq, Clone)]
struct Painted {
    w: i32,
    h: i32,
    gradient: Gradient,
    thickness: i32,
    radius: i32,
}

pub struct Overlay {
    hwnd: isize,
    _factory: ID2D1Factory,
    target: ID2D1DCRenderTarget,
    surface: Option<Surface>,
    painted: Option<Painted>,
    visible: bool,
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(h, msg, wp, lp)
}

impl Overlay {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = w!("deadbeef_border");
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            let h = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("deadbeef border"),
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
            .map_err(|e| format!("CreateWindowExW(border): {e}"))?;

            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).map_err(|e| format!("D2D1CreateFactory: {e}"))?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&props).map_err(|e| format!("CreateDCRenderTarget: {e}"))?;
            Ok(Self { hwnd: raw(h), _factory: factory, target, surface: None, painted: None, visible: false })
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(hwnd(self.hwnd), SW_HIDE);
            }
            self.visible = false;
        }
    }

    /// Draws the border around `visual` (the window's visible bounds) and
    /// stacks the overlay directly above `owner`.
    pub fn show(&mut self, owner: isize, visual: Rect, gradient: &Gradient, thickness: i32, radius: i32, topmost: bool) {
        if thickness <= 0 {
            self.hide();
            return;
        }
        let outer = Rect::new(visual.x - thickness, visual.y - thickness, visual.w + 2 * thickness, visual.h + 2 * thickness);
        let want = Painted { w: outer.w, h: outer.h, gradient: gradient.clone(), thickness, radius };
        if self.painted.as_ref() != Some(&want) {
            if let Err(e) = self.paint(&want) {
                crate::log_warn!("border overlay paint failed: {e}");
                self.hide();
                return;
            }
            self.painted = Some(want);
        }
        let Some(surface) = &self.surface else { return };
        unsafe {
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let pos = POINT { x: outer.x, y: outer.y };
            let size = SIZE { cx: outer.w, cy: outer.h };
            let src = POINT { x: 0, y: 0 };
            let _ = UpdateLayeredWindow(
                hwnd(self.hwnd),
                None,
                Some(&pos),
                Some(&size),
                surface.dc,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );

            // Stack directly above the owner: insert after whatever is above it.
            let after = if topmost {
                Some(HWND_TOPMOST)
            } else {
                match GetWindow(hwnd(owner), GW_HWNDPREV).ok().filter(|a| !a.is_invalid()) {
                    Some(a) if raw(a) == self.hwnd => None, // already in place
                    Some(a) => Some(a),
                    None => Some(HWND_TOP),
                }
            };
            if let Some(after) = after {
                let _ = SetWindowPos(hwnd(self.hwnd), after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            if !self.visible {
                let _ = ShowWindow(hwnd(self.hwnd), SW_SHOWNOACTIVATE);
                self.visible = true;
            }
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
                    biHeight: -h, // top-down
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

    fn paint(&mut self, p: &Painted) -> Result<(), String> {
        self.ensure_surface(p.w, p.h)?;
        let surface = self.surface.as_ref().unwrap();
        unsafe {
            let rc = RECT { left: 0, top: 0, right: p.w, bottom: p.h };
            self.target.BindDC(surface.dc, &rc).map_err(|e| format!("BindDC: {e}"))?;
            self.target.BeginDraw();
            self.target.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));

            let n = p.gradient.colors.len();
            let stops: Vec<D2D1_GRADIENT_STOP> = p
                .gradient
                .colors
                .iter()
                .enumerate()
                .map(|(i, c)| D2D1_GRADIENT_STOP {
                    position: if n == 1 { 0.0 } else { i as f32 / (n - 1) as f32 },
                    color: D2D1_COLOR_F {
                        r: c.r as f32 / 255.0,
                        g: c.g as f32 / 255.0,
                        b: c.b as f32 / 255.0,
                        a: c.a as f32 / 255.0,
                    },
                })
                .collect();
            let collection = self
                .target
                .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
                .map_err(|e| format!("CreateGradientStopCollection: {e}"))?;

            // Gradient axis through the centre at the configured angle
            // (0deg = left→right, counter-clockwise, y up as in Hyprland).
            let (cx, cy) = (p.w as f32 / 2.0, p.h as f32 / 2.0);
            let a = p.gradient.angle_deg.to_radians();
            let half = (cx * cx + cy * cy).sqrt();
            let (dx, dy) = (a.cos() * half, -a.sin() * half);
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: D2D_POINT_2F { x: cx - dx, y: cy - dy },
                endPoint: D2D_POINT_2F { x: cx + dx, y: cy + dy },
            };
            let brush = self
                .target
                .CreateLinearGradientBrush(&props, None, &collection)
                .map_err(|e| format!("CreateLinearGradientBrush: {e}"))?;

            let t = p.thickness as f32;
            let r = (p.radius as f32 + t / 2.0).max(0.0);
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: t / 2.0, top: t / 2.0, right: p.w as f32 - t / 2.0, bottom: p.h as f32 - t / 2.0 },
                radiusX: r,
                radiusY: r,
            };
            self.target.DrawRoundedRectangle(&rr, &brush, t, None);
            self.target.EndDraw(None, None).map_err(|e| format!("EndDraw: {e}"))?;
        }
        Ok(())
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.surface = None;
        unsafe {
            let _ = DestroyWindow(hwnd(self.hwnd));
        }
    }
}
