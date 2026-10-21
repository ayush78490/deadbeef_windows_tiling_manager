//! The live preview: a scaled-down desktop drawn from the model. Tiles come
//! from the real layout code (`layout::with_gaps` + dwindle / master) run on
//! the real monitor size, so what you see is what Apply produces.

use std::time::Instant;

use super::model::Model;
use crate::config::types::{LayoutKind, Rgba, Tristate};
use crate::layout::{self, dwindle, master, Rect};
use crate::ui::{self, Align, Canvas, RectF};

/// Fake apps shown in the preview tiles: (title, glyph).
const APPS: &[(&str, char)] = &[
    ("Terminal", '\u{E756}'),
    ("Browser", '\u{E774}'),
    ("Files", '\u{E8B7}'),
    ("Code", '\u{E943}'),
    ("Music", '\u{E8D6}'),
    ("Notes", '\u{E70B}'),
    ("Mail", '\u{E715}'),
    ("Photos", '\u{E91B}'),
];
pub const MAX_WINDOWS: usize = APPS.len();

const ANIM_MS: f32 = 220.0;

pub struct Preview {
    pub windows: usize,
    pub focus: usize,
    /// Real monitor size in pixels and whether this is Windows 11.
    pub monitor: (i32, i32),
    win11: bool,
    from: Vec<RectF>,
    to: Vec<RectF>,
    t0: Instant,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let f = |x: u8, y: u8| lerp(x as f32, y as f32, t).round() as u8;
    Rgba { r: f(a.r, b.r), g: f(a.g, b.g), b: f(a.b, b.b), a: f(a.a, b.a) }
}

fn opaque(c: Rgba) -> Rgba {
    Rgba { a: 0xff, ..c }
}

fn is_light(c: Rgba) -> bool {
    (c.r as u32 * 299 + c.g as u32 * 587 + c.b as u32 * 114) / 1000 > 150
}

impl Preview {
    pub fn new() -> Self {
        let mons = crate::win::monitors();
        let m = mons.iter().find(|m| m.primary).or(mons.first());
        let monitor = m.map(|m| (m.rect.w.max(640), m.rect.h.max(480))).unwrap_or((1920, 1080));
        Preview { windows: 3, focus: 2, monitor, win11: crate::win::is_windows11(), from: Vec::new(), to: Vec::new(), t0: Instant::now() }
    }

    pub fn animating(&self) -> bool {
        self.t0.elapsed().as_secs_f32() * 1000.0 < ANIM_MS
    }

    /// Height reserved by the Windows taskbar when it is visible.
    fn taskbar_h(&self) -> i32 {
        if self.win11 {
            48
        } else {
            40
        }
    }

    /// Tile rects in real monitor pixels, index 0 = oldest window.
    fn layout(&self, m: &Model) -> Vec<Rect> {
        let (mw, mh) = self.monitor;
        let mut work = Rect::new(0, 0, mw, mh);
        let taskbar = !(m.bar && m.hide_taskbar);
        if taskbar {
            work.h -= self.taskbar_h();
        }
        if m.bar {
            work = work.inset(m.bar_height, 0, 0, 0);
        }
        let n = self.windows;
        let tiles = layout::with_gaps(work, m.gaps_in, m.gaps_out, |a| match m.layout {
            LayoutKind::Dwindle => dwindle::layout(a, n, m.split_ratio, m.spiral, false),
            LayoutKind::Master => master::layout(a, n, m.mfact, 1, m.orientation),
        });
        // Master: the master tile goes to the newest window with
        // new_status = master, otherwise to the oldest.
        if m.layout == LayoutKind::Master && m.new_master {
            tiles.into_iter().rev().collect()
        } else {
            tiles
        }
    }

    fn animated(&mut self, target: Vec<RectF>, animate: bool) -> Vec<RectF> {
        let t = (self.t0.elapsed().as_secs_f32() * 1000.0 / ANIM_MS).min(1.0);
        let ease = 1.0 - (1.0 - t).powi(3);
        let current: Vec<RectF> = self
            .to
            .iter()
            .enumerate()
            .map(|(i, to)| match self.from.get(i) {
                Some(f) => RectF::new(lerp(f.x, to.x, ease), lerp(f.y, to.y, ease), lerp(f.w, to.w, ease), lerp(f.h, to.h, ease)),
                None => *to,
            })
            .collect();
        if target != self.to {
            // New windows grow out of the centre of their own tile.
            self.from = target
                .iter()
                .enumerate()
                .map(|(i, r)| current.get(i).copied().unwrap_or(RectF::new(r.x + r.w / 2.0, r.y + r.h / 2.0, 0.0, 0.0)))
                .collect();
            self.to = target;
            self.t0 = Instant::now();
            if !animate {
                self.from.clear();
            }
            return if animate { self.from.clone() } else { self.to.clone() };
        }
        current
    }

    /// Draws the preview into the top of `area`; returns hit rects for each
    /// tile and the y just below the monitor stand.
    pub fn draw(&mut self, c: &mut Canvas, area: RectF, m: &Model, s: f32) -> (Vec<(RectF, usize)>, f32) {
        let (mw, mh) = self.monitor;
        // Fit the monitor into the area, leaving room for the stand.
        let bezel = 10.0 * s;
        let avail_h = area.h - 26.0 * s;
        let k = ((area.w - 2.0 * bezel) / mw as f32).min((avail_h - 2.0 * bezel) / mh as f32);
        let sw = mw as f32 * k;
        let sh = mh as f32 * k;
        let frame = RectF::new(area.x + (area.w - sw) / 2.0 - bezel, area.y, sw + 2.0 * bezel, sh + 2.0 * bezel);
        let screen = frame.inset(bezel, bezel);
        let px = |r: Rect| RectF::new(screen.x + r.x as f32 * k, screen.y + r.y as f32 * k, r.w as f32 * k, r.h as f32 * k);

        // Monitor and stand.
        let stand = Rgba { r: 0x16, g: 0x19, b: 0x1f, a: 0xff };
        c.fill_rrect(RectF::new(frame.x + frame.w / 2.0 - 30.0 * s, frame.bottom(), 60.0 * s, 14.0 * s), 2.0 * s, stand);
        c.fill_rrect(RectF::new(frame.x + frame.w / 2.0 - 70.0 * s, frame.bottom() + 12.0 * s, 140.0 * s, 6.0 * s), 3.0 * s, stand);
        c.fill_rrect(frame, 12.0 * s, stand);
        c.stroke_rrect(frame, 12.0 * s, 1.0 * s, Rgba { r: 0xff, g: 0xff, b: 0xff, a: 0x18 });

        // Wallpaper from the theme colours.
        let bg = opaque(m.bar_background);
        let accent = opaque(m.bar_accent);
        let second = m.active_border.colors.get(1).copied().map(opaque).unwrap_or(accent);
        c.push_clip(screen);
        c.fill_rrect_gradient(screen, 0.0, &[mix(bg, accent, 0.45), bg, mix(bg, second, 0.35)], -35.0);
        c.fill_circle(screen.x + screen.w * 0.78, screen.y + screen.h * 0.3, screen.h * 0.32, ui::with_alpha(accent, 0x30));
        c.fill_circle(screen.x + screen.w * 0.18, screen.y + screen.h * 0.85, screen.h * 0.4, ui::with_alpha(second, 0x24));

        let target: Vec<RectF> = self.layout(m).into_iter().map(px).collect();
        let rects = self.animated(target, m.animations);
        self.focus = self.focus.min(self.windows.saturating_sub(1));

        // Windows: inactive first so the focused border sits on top.
        let overlay = !self.win11 || m.border_overlay == Tristate::On;
        let radius = match m.rounding {
            0 => 0.0,
            1..=4 => 4.0,
            r if overlay => r as f32,
            _ => 8.0,
        } * k;
        let mut order: Vec<usize> = (0..rects.len()).filter(|&i| i != self.focus).collect();
        order.push(self.focus);
        let mut hits = Vec::new();
        for &i in &order {
            let Some(&r) = rects.get(i) else { continue };
            if r.w < 2.0 || r.h < 2.0 {
                continue;
            }
            let focused = i == self.focus;
            self.draw_window(c, r, i, focused, m, radius, k, s, overlay);
            hits.push((r, i));
        }

        if m.bar {
            self.draw_bar(c, px(Rect::new(0, 0, mw, m.bar_height)), m, k, s);
        }
        if !(m.bar && m.hide_taskbar) {
            let th = self.taskbar_h();
            self.draw_taskbar(c, px(Rect::new(0, mh - th, mw, th)), s);
        }
        c.pop_clip();
        (hits, frame.bottom() + 18.0 * s)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_window(&self, c: &mut Canvas, r: RectF, i: usize, focused: bool, m: &Model, radius: f32, k: f32, s: f32, overlay: bool) {
        let opacity = if focused { m.active_opacity } else { m.inactive_opacity };
        let a = (opacity * 255.0).round() as u8;
        let light = is_light(m.bar_background);
        let body = if light { Rgba { r: 0xfb, g: 0xfb, b: 0xfb, a } } else { Rgba { r: 0x1f, g: 0x23, b: 0x2b, a } };
        let chrome = if light { Rgba { r: 0xee, g: 0xee, b: 0xf0, a } } else { Rgba { r: 0x29, g: 0x2e, b: 0x38, a } };
        let fg = ui::with_alpha(opaque(m.bar_foreground), (a as f32 * 0.85) as u8);
        let dim = ui::with_alpha(opaque(m.bar_dim), a);
        let accent = ui::with_alpha(opaque(m.bar_accent), a);

        c.push_clip(r);
        c.fill_rrect(r, radius, body);
        // Title bar.
        let tb = (30.0 * k).max(7.0 * s).min(r.h * 0.3);
        c.fill_rect(RectF::new(r.x, r.y, r.w, tb), chrome);
        let (title, glyph) = APPS[i % APPS.len()];
        let fs = (tb * 0.5).max(6.0 * s);
        if r.w > 60.0 * s {
            c.text(&glyph.to_string(), RectF::new(r.x + 6.0 * s, r.y, fs * 1.6, tb), ui::icon_font(), fs, false, Align::Left, accent);
            c.text(title, RectF::new(r.x + 8.0 * s + fs * 1.6, r.y, r.w * 0.6, tb), ui::text_font(), fs, false, Align::Left, fg);
        }
        // Skeleton content lines.
        let lh = (fs * 0.55).max(3.0 * s);
        let mut y = r.y + tb + lh * 1.4;
        let mut n = 0;
        while y + lh < r.bottom() - lh && n < 14 {
            let w = r.w * [0.62, 0.4, 0.75, 0.3, 0.55, 0.68, 0.45][(n + i) % 7] - 12.0 * s;
            if w > 4.0 {
                c.fill_rrect(RectF::new(r.x + 8.0 * s, y, w, lh), lh / 2.0, if n % 3 == 0 { accent } else { dim });
            }
            y += lh * 2.1;
            n += 1;
        }
        c.pop_clip();

        // Border.
        if m.border_size > 0 {
            let width = if overlay { (m.border_size as f32 * k).max(1.0 * s) } else { 1.0 * s };
            if focused {
                let colors: Vec<Rgba> = if overlay {
                    m.active_border.colors.clone()
                } else {
                    vec![m.active_border.primary()]
                };
                if colors.len() > 1 {
                    c.stroke_rrect_gradient(r, radius, width, &colors, m.active_border.angle_deg);
                } else {
                    c.stroke_rrect(r, radius, width, colors[0]);
                }
            } else if m.inactive_border.a > 0 {
                c.stroke_rrect(r, radius, width, m.inactive_border);
            }
        }
    }

    fn draw_bar(&self, c: &mut Canvas, r: RectF, m: &Model, k: f32, s: f32) {
        c.fill_rect(r, m.bar_background);
        let fg = opaque(m.bar_foreground);
        let accent = opaque(m.bar_accent);
        let dim = opaque(m.bar_dim);
        let fs = (m.bar_font_size * k * 1.33).max(6.0 * s).min(r.h * 0.7);
        let pad = 10.0 * k + 3.0 * s;
        let icon = |c: &mut Canvas, x: f32, g: char, col: Rgba| {
            c.text(&g.to_string(), RectF::new(x, r.y, fs * 1.4, r.h), ui::icon_font(), fs, false, Align::Center, col);
            fs * 1.6
        };

        // Left section.
        let mut x = r.x + pad;
        for module in &m.modules_left {
            match module.as_str() {
                "launcher" => x += icon(c, x, ui::glyph::SEARCH, accent),
                "workspaces" => {
                    for ws in 0..4 {
                        let d = fs * 0.5;
                        let col = if ws == 0 { accent } else { dim };
                        let w = if ws == 0 { d * 2.2 } else { d };
                        c.fill_rrect(RectF::new(x, r.y + (r.h - d) / 2.0, w, d), d / 2.0, col);
                        x += w + d * 0.7;
                    }
                    x += fs * 0.5;
                }
                "apps" => {
                    for (i, app) in APPS.iter().take(self.windows.min(5)).enumerate() {
                        let col = if i == self.focus { fg } else { dim };
                        x += icon(c, x, app.1, col);
                    }
                }
                _ => x += icon(c, x, '\u{E8A1}', dim),
            }
        }

        // Centre: the clock in the chosen format.
        if !m.modules_center.is_empty() {
            let t = crate::win::local_time();
            let label: Vec<String> = m
                .modules_center
                .iter()
                .map(|mo| if mo == "clock" { crate::bar::clock::format(&t, &m.clock_format) } else { "deadbeef".into() })
                .collect();
            c.text(&label.join("   "), RectF::new(r.x + r.w * 0.25, r.y, r.w * 0.5, r.h), ui::text_font(), fs * 0.85, true, Align::Center, fg);
        }

        // Right section, laid out from the edge.
        let mut x = r.right() - pad;
        for module in m.modules_right.iter().rev() {
            let g = match module.as_str() {
                "cpu" => ui::glyph::CPU,
                "memory" => ui::glyph::RAM,
                "temperature" => ui::glyph::TEMP,
                "volume" => ui::glyph::VOLUME,
                "wifi" => ui::glyph::WIFI,
                "bluetooth" => ui::glyph::BLUETOOTH,
                "battery" => ui::glyph::battery(80, false),
                "wallpaper" => ui::glyph::IMAGE,
                "power" => ui::glyph::POWER,
                "clock" => ui::glyph::CALENDAR,
                _ => '\u{E8A1}',
            };
            x -= fs * 1.6;
            if x < r.x + r.w * 0.72 {
                break;
            }
            icon(c, x, g, if module == "power" { accent } else { fg });
        }
    }

    fn draw_taskbar(&self, c: &mut Canvas, r: RectF, s: f32) {
        c.fill_rect(r, Rgba { r: 0x1c, g: 0x1c, b: 0x1c, a: 0xf0 });
        c.line(r.x, r.y, r.right(), r.y, 1.0, Rgba { r: 0xff, g: 0xff, b: 0xff, a: 0x14 });
        let d = r.h * 0.55;
        let n = 6 + self.windows.min(4);
        let total = n as f32 * d * 1.3;
        let mut x = r.x + (r.w - total) / 2.0;
        for i in 0..n {
            let col = if i == 0 { Rgba { r: 0x4c, g: 0xc2, b: 0xff, a: 0xff } } else { Rgba { r: 0xff, g: 0xff, b: 0xff, a: 0x40 } };
            c.fill_rrect(RectF::new(x, r.y + (r.h - d) / 2.0, d, d), 2.0 * s, col);
            x += d * 1.3;
        }
    }
}
