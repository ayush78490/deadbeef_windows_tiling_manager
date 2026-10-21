//! deadbeef Settings (`deadbeef.exe --settings`, the tray's "Settings…",
//! or the Start menu shortcut): set everything up without editing files.
//!
//! The left column edits a [`Model`]; the right column previews it. Nothing
//! touches the system until **Apply**, which saves `settings.conf`, installs
//! deadbeef, registers it to start at sign-in and starts (or reloads) the
//! window manager. **Reset everything** undoes all of it.
//!
//! The window is drawn on a [`Canvas`] like the rest of deadbeef's UI; it is
//! a separate process from the window manager and talks to it over the pipe.

pub mod model;
mod preview;
pub mod system;

use std::cell::RefCell;
use std::mem::size_of;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::profile::{self, Paths};
use crate::config::types::{LayoutKind, Rgba, BAR_MODULES};
use crate::ui::{self, glyph, Align, Canvas, RectF};
use crate::win::{self, hwnd};
use model::{Flag, Model, Num, Pick, ACCENTS, INACTIVE, PRESETS};
use preview::Preview;
use system::Install;

const CLASS: PCWSTR = w!("deadbeef_settings");
const TIMER_STATUS: usize = 1;
const TIMER_ANIM: usize = 2;
const TITLE_H: f32 = 48.0;
const SIDEBAR_W: f32 = 214.0;
const FOOTER_H: f32 = 68.0;
const TOAST_SECS: u64 = 6;
const CONFIRM_SECS: u64 = 4;
/// Defined in Win32_UI_Controls, which deadbeef doesn't otherwise need.
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Layout,
    Look,
    Bar,
    Behaviour,
    System,
}

const PAGES: [(Page, &str, char); 6] = [
    (Page::Overview, "Overview", '\u{E80F}'),
    (Page::Layout, "Layout", '\u{E8A9}'),
    (Page::Look, "Look", '\u{E771}'),
    (Page::Bar, "Top bar", '\u{E7F4}'),
    (Page::Behaviour, "Behaviour", '\u{E765}'),
    (Page::System, "Startup & reset", '\u{E7E8}'),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Apply,
    Discard,
    Defaults,
    StartStop,
    Reload,
    Install,
    Autostart,
    ResetAll,
    Uninstall,
    OpenFolder,
    EditConf,
    OpenLog,
    ShowBinds,
    MoreWindows,
    FewerWindows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Active,
    Second,
    Inactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Nav(Page),
    Close,
    Minimize,
    Slider(Num),
    Toggle(Flag),
    Choice(Pick, usize),
    Color(Slot, u32),
    Preset(usize),
    Module(usize),
    Tile(usize),
    Do(Action),
}

#[derive(Clone, Copy)]
struct Colors {
    bg: Rgba,
    side: Rgba,
    card: Rgba,
    line: Rgba,
    fg: Rgba,
    dim: Rgba,
    accent: Rgba,
    on_accent: Rgba,
    ok: Rgba,
    warn: Rgba,
    danger: Rgba,
}

const fn hex(v: u32) -> Rgba {
    Rgba { r: (v >> 24) as u8, g: (v >> 16) as u8, b: (v >> 8) as u8, a: v as u8 }
}

fn is_light(c: Rgba) -> bool {
    (c.r as u32 * 299 + c.g as u32 * 587 + c.b as u32 * 114) / 1000 > 150
}

impl Colors {
    /// A fixed dark UI tinted with the theme's accent, so picking a theme
    /// recolours the app as well as the preview.
    fn new(accent: Rgba) -> Self {
        let accent = Rgba { a: 0xff, ..accent };
        Colors {
            bg: hex(0x1b1e24ff),
            side: hex(0x16191eff),
            card: hex(0x252a32ff),
            line: hex(0xffffff14),
            fg: hex(0xe6e9efff),
            dim: hex(0x8f97a6ff),
            accent,
            on_accent: if is_light(accent) { hex(0x12151aff) } else { hex(0xffffffff) },
            ok: hex(0xa3be8cff),
            warn: hex(0xebcb8bff),
            danger: hex(0xbf616aff),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Secondary,
    Danger,
}

/// Immediate-mode drawing of one column: a cursor (`y`) moving down,
/// registering clickable rects as it goes.
struct Ui<'a> {
    c: &'a mut Canvas,
    hits: &'a mut Vec<(RectF, Hit)>,
    hover: Option<Hit>,
    armed: Option<Hit>,
    clip: RectF,
    t: Colors,
    s: f32,
    x: f32,
    w: f32,
    y: f32,
}

fn font() -> &'static str {
    ui::text_font()
}

impl Ui<'_> {
    fn hit(&mut self, r: RectF, h: Hit) {
        let top = r.y.max(self.clip.y);
        let bottom = r.bottom().min(self.clip.bottom());
        if bottom > top {
            self.hits.push((RectF::new(r.x, top, r.w, bottom - top), h));
        }
    }

    fn hovered(&self, h: Hit) -> bool {
        self.hover == Some(h)
    }

    fn heading(&mut self, title: &str, sub: &str) {
        let s = self.s;
        self.c.text(title, RectF::new(self.x, self.y, self.w, 36.0 * s), font(), 24.0 * s, true, Align::Left, self.t.fg);
        self.y += 42.0 * s;
        self.y += self.c.paragraph(sub, self.x, self.y, self.w, font(), 13.0 * s, self.t.dim) + 14.0 * s;
    }

    fn section(&mut self, title: &str) {
        let s = self.s;
        self.y += 14.0 * s;
        self.c.text(title, RectF::new(self.x, self.y, self.w, 22.0 * s), font(), 14.0 * s, true, Align::Left, self.t.fg);
        self.y += 30.0 * s;
    }

    fn note(&mut self, text: &str) {
        let s = self.s;
        self.y += self.c.paragraph(text, self.x + 4.0 * s, self.y, self.w - 8.0 * s, font(), 12.0 * s, self.t.dim) + 8.0 * s;
    }

    /// A setting card; hovering it highlights it when it is clickable.
    fn card(&mut self, h: f32, hit: Option<Hit>) -> RectF {
        let r = RectF::new(self.x, self.y, self.w, h);
        let hot = hit.is_some_and(|h| self.hovered(h));
        self.c.fill_rrect(r, 8.0 * self.s, if hot { ui::with_alpha(self.t.fg, 0x1a) } else { self.t.card });
        if let Some(h) = hit {
            self.hit(r, h);
        }
        self.y += h + 4.0 * self.s;
        r
    }

    fn header_h(&self, desc: &str) -> f32 {
        if desc.is_empty() {
            46.0 * self.s
        } else {
            60.0 * self.s
        }
    }

    /// Label and description in the left part of a card.
    fn labels(&mut self, r: RectF, label: &str, desc: &str, right: f32) {
        let s = self.s;
        let w = r.w - 32.0 * s - right;
        if desc.is_empty() {
            self.c.text(label, RectF::new(r.x + 16.0 * s, r.y, w, 46.0 * s), font(), 13.5 * s, false, Align::Left, self.t.fg);
        } else {
            self.c.text(label, RectF::new(r.x + 16.0 * s, r.y + 9.0 * s, w, 22.0 * s), font(), 13.5 * s, false, Align::Left, self.t.fg);
            self.c.text(desc, RectF::new(r.x + 16.0 * s, r.y + 30.0 * s, w, 20.0 * s), font(), 12.0 * s, false, Align::Left, self.t.dim);
        }
    }

    fn switch(&mut self, r: RectF, on: bool, hot: bool) {
        let s = self.s;
        let sw = RectF::new(r.right() - 60.0 * s, r.y + (r.h - 22.0 * s) / 2.0, 44.0 * s, 22.0 * s);
        let knob_x = if on { sw.right() - 11.0 * s } else { sw.x + 11.0 * s };
        if on {
            self.c.fill_rrect(sw, 11.0 * s, self.t.accent);
            self.c.fill_circle(knob_x, sw.y + 11.0 * s, if hot { 7.0 } else { 6.0 } * s, self.t.on_accent);
        } else {
            self.c.stroke_rrect(sw, 11.0 * s, 1.5 * s, self.t.dim);
            self.c.fill_circle(knob_x, sw.y + 11.0 * s, if hot { 6.0 } else { 5.0 } * s, self.t.dim);
        }
        let label = if on { "On" } else { "Off" };
        self.c.text(label, RectF::new(sw.x - 50.0 * s, sw.y, 40.0 * s, sw.h), font(), 12.5 * s, false, Align::Right, self.t.dim);
    }

    fn toggle(&mut self, label: &str, desc: &str, on: bool, hit: Hit) {
        let r = self.card(self.header_h(desc), Some(hit));
        self.labels(r, label, desc, 110.0 * self.s);
        let hot = self.hovered(hit);
        self.switch(r, on, hot);
    }

    fn slider(&mut self, label: &str, desc: &str, n: Num, m: &Model) {
        let s = self.s;
        let r = self.card(self.header_h(desc), None);
        let track_w = (r.w * 0.34).min(240.0 * s);
        self.labels(r, label, desc, track_w + 90.0 * s);
        let v = m.num(n);
        let (lo, hi, _) = n.range();
        let f = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        let track = RectF::new(r.right() - 84.0 * s - track_w, r.y + r.h / 2.0 - 2.0 * s, track_w, 4.0 * s);
        self.c.fill_rrect(track, 2.0 * s, ui::with_alpha(self.t.fg, 0x30));
        self.c.fill_rrect(RectF::new(track.x, track.y, track.w * f, track.h), 2.0 * s, self.t.accent);
        let hot = self.hovered(Hit::Slider(n));
        let kx = track.x + track.w * f;
        self.c.fill_circle(kx, track.y + 2.0 * s, 9.0 * s, self.t.card);
        self.c.fill_circle(kx, track.y + 2.0 * s, if hot { 7.0 } else { 6.0 } * s, self.t.accent);
        self.c.text(&n.format(v), RectF::new(r.right() - 76.0 * s, r.y, 62.0 * s, r.h), font(), 13.0 * s, true, Align::Right, self.t.fg);
        self.hit(RectF::new(track.x - 6.0 * s, r.y, track.w + 12.0 * s, r.h), Hit::Slider(n));
    }

    fn choice(&mut self, label: &str, desc: &str, p: Pick, m: &Model) {
        let s = self.s;
        let r = self.card(self.header_h(desc) + 42.0 * s, None);
        self.labels(RectF { h: self.header_h(desc), ..r }, label, desc, 0.0);
        let opts = p.options();
        let strip = RectF::new(r.x + 16.0 * s, r.bottom() - 46.0 * s, r.w - 32.0 * s, 32.0 * s);
        self.c.fill_rrect(strip, 8.0 * s, ui::with_alpha(self.t.fg, 0x0c));
        let seg = strip.w / opts.len() as f32;
        let sel = m.pick(p);
        for (i, o) in opts.iter().enumerate() {
            let b = RectF::new(strip.x + seg * i as f32, strip.y, seg, strip.h).inset(2.0 * s, 2.0 * s);
            let h = Hit::Choice(p, i);
            let col = if sel == Some(i) {
                self.c.fill_rrect(b, 6.0 * s, self.t.accent);
                self.t.on_accent
            } else {
                if self.hovered(h) {
                    self.c.fill_rrect(b, 6.0 * s, ui::with_alpha(self.t.fg, 0x18));
                }
                self.t.fg
            };
            self.c.text(o, b, font(), 12.5 * s, sel == Some(i), Align::Center, col);
            self.hit(b, h);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn swatches(&mut self, label: &str, desc: &str, slot: Slot, palette: &[u32], selected: Option<Rgba>, none: bool) {
        let s = self.s;
        let d = 26.0 * s;
        let step = d + 10.0 * s;
        let count = palette.len() + none as usize;
        let per_row = (((self.w - 32.0 * s) / step).floor() as usize).max(1);
        let rows = count.div_ceil(per_row);
        let head = self.header_h(desc);
        let r = self.card(head + rows as f32 * step + 6.0 * s, None);
        self.labels(RectF { h: head, ..r }, label, desc, 0.0);
        let items = palette.iter().map(|&v| Some(v)).chain(none.then_some(None));
        for (i, v) in items.enumerate() {
            let cx = r.x + 16.0 * s + d / 2.0 + (i % per_row) as f32 * step;
            let cy = r.y + head + d / 2.0 + (i / per_row) as f32 * step;
            let value = v.unwrap_or(0);
            let col = model::rgba(value);
            let h = Hit::Color(slot, value);
            let is_sel = match (v, selected) {
                (Some(_), Some(sel)) => sel == col,
                (None, None) => true,
                _ => false,
            };
            if is_sel {
                self.c.fill_circle(cx, cy, d / 2.0 + 4.0 * s, self.t.fg);
                self.c.fill_circle(cx, cy, d / 2.0 + 2.0 * s, self.t.card);
            } else if self.hovered(h) {
                self.c.fill_circle(cx, cy, d / 2.0 + 3.0 * s, ui::with_alpha(self.t.fg, 0x40));
            }
            if v.is_none() || col.a == 0 {
                self.c.fill_circle(cx, cy, d / 2.0, hex(0x3a404bff));
                let o = d * 0.3;
                self.c.line(cx - o, cy + o, cx + o, cy - o, 2.0 * s, self.t.danger);
            } else {
                self.c.fill_circle(cx, cy, d / 2.0, Rgba { a: 0xff, ..col });
            }
            self.hit(RectF::new(cx - d / 2.0 - 3.0 * s, cy - d / 2.0 - 3.0 * s, d + 6.0 * s, d + 6.0 * s), h);
        }
    }

    fn button(&mut self, r: RectF, label: &str, icon: Option<char>, kind: Kind, hit: Hit) {
        let s = self.s;
        let hot = self.hovered(hit);
        let armed = self.armed == Some(hit);
        let (bg, fg) = match kind {
            Kind::Primary => (self.t.accent, self.t.on_accent),
            Kind::Secondary => (ui::with_alpha(self.t.fg, 0x16), self.t.fg),
            Kind::Danger if armed => (self.t.danger, hex(0xffffffff)),
            Kind::Danger => (ui::with_alpha(self.t.danger, 0x30), hex(0xf2c4c8ff)),
        };
        self.c.fill_rrect(r, 7.0 * s, bg);
        if hot {
            self.c.fill_rrect(r, 7.0 * s, ui::with_alpha(hex(0xffffffff), 0x1c));
        }
        let label = if armed { "Click again to confirm" } else { label };
        let tw = self.c.measure(label, font(), 13.0 * s, true);
        let iw = if icon.is_some() { 22.0 * s } else { 0.0 };
        let x0 = r.x + ((r.w - tw - iw) / 2.0).max(8.0 * s);
        if let Some(g) = icon {
            self.c.text(&g.to_string(), RectF::new(x0, r.y, 18.0 * s, r.h), ui::icon_font(), 13.0 * s, false, Align::Left, fg);
        }
        self.c.text(label, RectF::new(x0 + iw, r.y, r.right() - x0 - iw - 4.0 * s, r.h), font(), 13.0 * s, true, Align::Left, fg);
        self.hit(r, hit);
    }

    /// A row of buttons inside one card, right below the cursor.
    fn button_row(&mut self, buttons: &[(&str, char, Kind, Action)]) {
        let s = self.s;
        let r = self.card(56.0 * s, None);
        let mut x = r.x + 12.0 * s;
        for (label, icon, kind, action) in buttons {
            let w = self.c.measure(label, font(), 13.0 * s, true) + 52.0 * s;
            self.button(RectF::new(x, r.y + 10.0 * s, w, 36.0 * s), label, Some(*icon), *kind, Hit::Do(*action));
            x += w + 8.0 * s;
        }
    }

    fn presets(&mut self, m: &Model) {
        let s = self.s;
        let cols = if self.w > 560.0 * s { 4 } else { 2 };
        let gap = 10.0 * s;
        let tw = (self.w - gap * (cols - 1) as f32) / cols as f32;
        let th = 92.0 * s;
        let sel = m.preset();
        for (i, p) in PRESETS.iter().enumerate() {
            let r = RectF::new(self.x + (i % cols) as f32 * (tw + gap), self.y + (i / cols) as f32 * (th + gap), tw, th);
            let h = Hit::Preset(i);
            let bg = Rgba { a: 0xff, ..model::rgba(p.bg) };
            self.c.fill_rrect(r, 10.0 * s, bg);
            // A tiny tiled desktop in the preset's colours.
            let inner = RectF::new(r.x + 10.0 * s, r.y + 10.0 * s, r.w - 20.0 * s, r.h - 42.0 * s);
            let half = (inner.w - 6.0 * s) / 2.0;
            let a = RectF::new(inner.x, inner.y, half, inner.h);
            let b = RectF::new(inner.x + half + 6.0 * s, inner.y, half, inner.h);
            let win_bg = ui::with_alpha(model::rgba(p.fg), 0x14);
            self.c.fill_rrect(a, 4.0 * s, win_bg);
            self.c.fill_rrect(b, 4.0 * s, win_bg);
            self.c.stroke_rrect_gradient(a, 4.0 * s, 2.0 * s, &[model::rgba(p.active[0]), model::rgba(p.active[1])], 45.0);
            self.c.stroke_rrect(b, 4.0 * s, 1.5 * s, model::rgba(p.inactive));
            self.c.text(p.name, RectF::new(r.x + 12.0 * s, r.bottom() - 30.0 * s, r.w - 24.0 * s, 24.0 * s), font(), 12.5 * s, true, Align::Left, model::rgba(p.fg));
            if sel == Some(i) {
                self.c.stroke_rrect(r, 10.0 * s, 2.5 * s, self.t.fg);
                self.c.text("\u{E73E}", RectF::new(r.right() - 30.0 * s, r.bottom() - 30.0 * s, 20.0 * s, 24.0 * s), ui::icon_font(), 12.0 * s, false, Align::Center, model::rgba(p.accent));
            } else if self.hovered(h) {
                self.c.stroke_rrect(r, 10.0 * s, 2.0 * s, ui::with_alpha(self.t.fg, 0x80));
            }
            self.hit(r, h);
        }
        let rows = PRESETS.len().div_ceil(cols);
        self.y += rows as f32 * (th + gap) + 2.0 * s;
    }

    fn modules(&mut self, m: &Model) {
        let s = self.s;
        let ch = 30.0 * s;
        // Lay chips out first to know the card height.
        let mut pos = Vec::new();
        let (mut x, mut y) = (16.0 * s, 0.0);
        for name in BAR_MODULES {
            let w = self.c.measure(name, font(), 12.5 * s, false) + 30.0 * s;
            if x + w > self.w - 16.0 * s {
                x = 16.0 * s;
                y += ch + 8.0 * s;
            }
            pos.push((x, y, w));
            x += w + 8.0 * s;
        }
        let head = self.header_h("Click to show or hide each part of the bar");
        let r = self.card(head + y + ch + 14.0 * s, None);
        self.labels(RectF { h: head, ..r }, "Modules", "Click to show or hide each part of the bar", 0.0);
        for (i, (name, (x, y, w))) in BAR_MODULES.iter().zip(pos).enumerate() {
            let b = RectF::new(r.x + x, r.y + head + y, w, ch);
            let on = m.has_module(name);
            let h = Hit::Module(i);
            if on {
                self.c.fill_rrect(b, ch / 2.0, self.t.accent);
            } else {
                self.c.stroke_rrect(b, ch / 2.0, 1.2 * s, if self.hovered(h) { self.t.fg } else { ui::with_alpha(self.t.fg, 0x40) });
            }
            self.c.text(name, b, font(), 12.5 * s, false, Align::Center, if on { self.t.on_accent } else { self.t.fg });
            self.hit(b, h);
        }
    }
}

/// System state shown in the UI, refreshed by a timer.
#[derive(Clone, Copy, PartialEq)]
struct Status {
    running: bool,
    autostart: bool,
    install: Install,
}

impl Status {
    fn read() -> Self {
        Status { running: system::wm_running(), autostart: system::autostart_enabled(), install: system::install_state() }
    }
}

struct App {
    canvas: Canvas,
    paths: Paths,
    s: f32,
    x: i32,
    y: i32,
    w: f32,
    h: f32,
    page: Page,
    model: Model,
    saved: Model,
    defaults: Model,
    preview: Preview,
    status: Status,
    scroll: f32,
    content_h: f32,
    hits: Vec<(RectF, Hit)>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    drag: Option<(Num, RectF)>,
    toast: Option<(String, bool, Instant)>,
    armed: Option<(Hit, Instant)>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| {
        let mut g = a.try_borrow_mut().ok()?;
        Some(f(g.as_mut()?))
    })
}

/// Runs the settings window until it is closed (single instance).
pub fn run(paths: Paths) {
    let _instance = unsafe { CreateMutexW(None, true, w!("Local\\deadbeef-settings")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if let Ok(h) = unsafe { FindWindowW(CLASS, None) } {
            unsafe {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            win::focus(win::raw(h));
        }
        return;
    }
    if let Err(e) = profile::bootstrap(&paths) {
        crate::log_error!("settings: profile bootstrap failed: {e}");
    }

    ui::register_class(CLASS, Some(proc_));
    let canvas = match Canvas::new(CLASS, "deadbeef Settings", WS_EX_APPWINDOW, true) {
        Ok(c) => c,
        Err(e) => {
            crate::log_error!("settings window: {e}");
            return;
        }
    };
    let me = canvas.hwnd;
    unsafe {
        // A minimise box and system menu let the taskbar button minimise/restore it.
        SetWindowLongPtrW(hwnd(me), GWL_STYLE, (WS_POPUP | WS_MINIMIZEBOX | WS_SYSMENU).0 as isize);
        let icon = crate::tray::app_icon();
        SendMessageW(hwnd(me), WM_SETICON, WPARAM(ICON_BIG as usize), LPARAM(icon.0 as isize));
        SendMessageW(hwnd(me), WM_SETICON, WPARAM(ICON_SMALL as usize), LPARAM(icon.0 as isize));
    }

    let monitors = win::monitors();
    let Some(m) = monitors.iter().find(|m| m.primary).or(monitors.first()).cloned() else { return };
    let s = m.scale;
    let w = (1320.0 * s).min(m.work.w as f32 * 0.94).round();
    let h = (860.0 * s).min(m.work.h as f32 * 0.92).round();

    let model = Model::load(&paths);
    let app = App {
        canvas,
        s,
        x: m.work.x + ((m.work.w as f32 - w) / 2.0) as i32,
        y: m.work.y + ((m.work.h as f32 - h) / 2.0) as i32,
        w,
        h,
        page: Page::Overview,
        saved: model.clone(),
        defaults: Model::defaults(&paths),
        model,
        paths,
        preview: Preview::new(),
        status: Status::read(),
        scroll: 0.0,
        content_h: 0.0,
        hits: Vec::new(),
        hover: None,
        pressed: None,
        drag: None,
        toast: None,
        armed: None,
    };
    APP.with(|a| *a.borrow_mut() = Some(app));
    with_app(|a| a.draw());
    win::focus(me);
    unsafe {
        SetTimer(hwnd(me), TIMER_STATUS, 1000, None);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    APP.with(|a| a.borrow_mut().take());
}

impl App {
    fn dirty(&self) -> bool {
        self.model != self.saved
    }

    fn content_rect(&self) -> RectF {
        let s = self.s;
        let x = SIDEBAR_W * s;
        let w = ((self.w - x) * 0.5).max(420.0 * s);
        RectF::new(x, TITLE_H * s, w, self.h - (TITLE_H + FOOTER_H) * s)
    }

    fn max_scroll(&self) -> f32 {
        (self.content_h - self.content_rect().h + 24.0 * self.s).max(0.0)
    }

    fn say(&mut self, msg: impl Into<String>, error: bool) {
        let msg = msg.into();
        if error {
            crate::log_warn!("settings: {msg}");
        }
        self.toast = Some((msg, error, Instant::now()));
    }

    fn draw(&mut self) {
        let (s, w, h) = (self.s, self.w, self.h);
        if self.canvas.begin(w as i32, h as i32).is_err() {
            return;
        }
        let t = Colors::new(self.model.bar_accent);
        let mut hits = Vec::new();
        let hover = self.hover;
        let armed = self.armed.map(|a| a.0);
        let dirty = self.dirty();
        let content = self.content_rect();

        let c = &mut self.canvas;
        let full = RectF::new(0.0, 0.0, w, h);
        c.fill_rrect(full, 10.0 * s, t.bg);

        // Sidebar.
        let side = RectF::new(0.0, 0.0, SIDEBAR_W * s, h);
        c.push_clip(side);
        c.fill_rrect(RectF { w: side.w + 20.0 * s, ..side }, 10.0 * s, t.side);
        c.pop_clip();
        let logo = RectF::new(18.0 * s, 14.0 * s, 26.0 * s, 26.0 * s);
        c.fill_rrect_gradient(logo, 7.0 * s, &[t.accent, Rgba { a: 0xff, ..self.model.active_border.colors.last().copied().unwrap_or(t.accent) }], 45.0);
        c.text("\u{E8A9}", logo, ui::icon_font(), 13.0 * s, false, Align::Center, t.on_accent);
        c.text("deadbeef", RectF::new(52.0 * s, 12.0 * s, 150.0 * s, 30.0 * s), font(), 16.0 * s, true, Align::Left, t.fg);

        let mut ny = TITLE_H * s + 14.0 * s;
        for (page, label, icon) in PAGES {
            let r = RectF::new(10.0 * s, ny, side.w - 20.0 * s, 40.0 * s);
            let hit = Hit::Nav(page);
            if page == self.page {
                c.fill_rrect(r, 7.0 * s, ui::with_alpha(t.fg, 0x14));
                c.fill_rrect(RectF::new(r.x, r.y + 11.0 * s, 3.0 * s, 18.0 * s), 1.5 * s, t.accent);
            } else if hover == Some(hit) {
                c.fill_rrect(r, 7.0 * s, ui::with_alpha(t.fg, 0x0c));
            }
            c.text(&icon.to_string(), RectF::new(r.x + 16.0 * s, r.y, 20.0 * s, r.h), ui::icon_font(), 15.0 * s, false, Align::Left, if page == self.page { t.accent } else { t.fg });
            c.text(label, RectF::new(r.x + 46.0 * s, r.y, r.w - 50.0 * s, r.h), font(), 13.5 * s, page == self.page, Align::Left, t.fg);
            hits.push((r, hit));
            ny += 44.0 * s;
        }
        // Status at the bottom of the sidebar.
        let st = self.status;
        let (dot, label) = if st.running { (t.ok, "Tiling is on") } else { (t.dim, "Tiling is off") };
        c.fill_circle(26.0 * s, h - 46.0 * s, 5.0 * s, dot);
        c.text(label, RectF::new(40.0 * s, h - 58.0 * s, side.w - 48.0 * s, 24.0 * s), font(), 12.5 * s, true, Align::Left, t.fg);
        let sub = if st.autostart { "Starts with Windows" } else { "Doesn't start with Windows" };
        c.text(sub, RectF::new(40.0 * s, h - 36.0 * s, side.w - 48.0 * s, 20.0 * s), font(), 11.5 * s, false, Align::Left, t.dim);

        // Title bar buttons.
        for (hit, glyph_, x) in [(Hit::Close, '\u{E711}', w - 52.0 * s), (Hit::Minimize, '\u{E921}', w - 100.0 * s)] {
            let r = RectF::new(x, 0.0, 46.0 * s, 36.0 * s);
            if hover == Some(hit) {
                let col = if hit == Hit::Close { t.danger } else { ui::with_alpha(t.fg, 0x18) };
                c.fill_rrect(r, 6.0 * s, col);
            }
            c.text(&glyph_.to_string(), r, ui::icon_font(), 10.0 * s, false, Align::Center, t.fg);
            hits.push((r, hit));
        }
        let title = if armed == Some(Hit::Close) { "Unapplied changes — click × again to close without applying" } else { "deadbeef Settings" };
        c.text(title, RectF::new(content.x + 28.0 * s, 0.0, w - content.x - 160.0 * s, TITLE_H * s), font(), 12.5 * s, armed == Some(Hit::Close), Align::Left, if armed == Some(Hit::Close) { t.warn } else { t.dim });

        // Page content (scrolls).
        c.push_clip(content);
        let mut ui = Ui {
            c,
            hits: &mut hits,
            hover,
            armed,
            clip: content,
            t,
            s,
            x: content.x + 28.0 * s,
            w: content.w - 40.0 * s,
            y: content.y + 8.0 * s - self.scroll,
        };
        let y0 = ui.y;
        draw_page(&mut ui, self.page, &self.model, st);
        let content_h = ui.y - y0;
        // `ui` ends here; re-borrow the canvas so `hits` is free again.
        let c = &mut self.canvas;
        c.pop_clip();

        // Preview.
        let pv = RectF::new(content.right(), TITLE_H * s, w - content.right(), h - (TITLE_H + FOOTER_H) * s).inset(24.0 * s, 0.0);
        c.text("Preview", RectF::new(pv.x, pv.y + 8.0 * s, 200.0 * s, 30.0 * s), font(), 17.0 * s, true, Align::Left, t.fg);
        let (mw, mh) = self.preview.monitor;
        let info = format!("{mw} × {mh} · {} · nothing changes until you press Apply", self.model.layout.name());
        c.text(&info, RectF::new(pv.x, pv.y + 36.0 * s, pv.w, 20.0 * s), font(), 12.0 * s, false, Align::Left, t.dim);
        let screen_area = RectF::new(pv.x, pv.y + 70.0 * s, pv.w, (pv.h - 70.0 * s) * 0.66);
        let (tiles, below) = self.preview.draw(c, screen_area, &self.model, s);
        for (r, i) in tiles {
            hits.push((r, Hit::Tile(i)));
        }
        // What Apply will change.
        let changes: Vec<(&str, String, String)> = self
            .saved
            .entries()
            .into_iter()
            .zip(self.model.entries())
            .filter(|(a, b)| a.1 != b.1)
            .map(|((k, old), (_, new))| (k, old, new))
            .collect();
        let list_y = below + 50.0 * s;
        if !changes.is_empty() {
            let head = format!("{} pending change{}", changes.len(), if changes.len() == 1 { "" } else { "s" });
            c.text(&head, RectF::new(pv.x, list_y, pv.w, 22.0 * s), font(), 13.0 * s, true, Align::Left, t.warn);
            let room = ((h - FOOTER_H * s - 12.0 * s - (list_y + 26.0 * s)) / (20.0 * s)).max(0.0) as usize;
            for (i, (k, old, new)) in changes.iter().take(room).enumerate() {
                let ry = list_y + 26.0 * s + i as f32 * 20.0 * s;
                let more = changes.len() > room && i + 1 == room;
                let line = if more { format!("… and {} more", changes.len() - room + 1) } else { format!("{old}  →  {new}") };
                if !more {
                    c.text(k, RectF::new(pv.x, ry, pv.w * 0.42, 20.0 * s), font(), 12.0 * s, false, Align::Left, t.dim);
                }
                let lx = if more { pv.x } else { pv.x + pv.w * 0.42 };
                // Colours read better as dots than as rgba() text.
                let colors = |v: &str| crate::config::types::parse_gradient(v).ok().map(|g| g.colors);
                if let (false, Some(a), Some(b)) = (more, colors(old), colors(new)) {
                    let dots = |c: &mut Canvas, mut x: f32, list: &[Rgba]| {
                        for col in list {
                            c.fill_circle(x, ry + 10.0 * s, 7.0 * s, ui::with_alpha(t.fg, 0x50));
                            c.fill_circle(x, ry + 10.0 * s, 6.0 * s, Rgba { a: 0xff, ..*col });
                            x += 17.0 * s;
                        }
                        x
                    };
                    let x = dots(c, lx + 7.0 * s, &a);
                    c.text("→", RectF::new(x - 4.0 * s, ry, 22.0 * s, 20.0 * s), font(), 12.0 * s, false, Align::Center, t.fg);
                    dots(c, x + 24.0 * s, &b);
                    continue;
                }
                c.text(&line, RectF::new(lx, ry, pv.right() - lx, 20.0 * s), font(), 12.0 * s, false, Align::Left, t.fg);
            }
        }
        // Window count stepper, right under the monitor.
        let sy = below;
        let mut ui = Ui { c, hits: &mut hits, hover, armed, clip: full, t, s, x: pv.x, w: pv.w, y: sy };
        ui.c.text("Windows", RectF::new(pv.x, sy, 80.0 * s, 34.0 * s), font(), 13.0 * s, false, Align::Left, t.fg);
        ui.button(RectF::new(pv.x + 80.0 * s, sy, 34.0 * s, 34.0 * s), "", Some('\u{E738}'), Kind::Secondary, Hit::Do(Action::FewerWindows));
        ui.c.text(&self.preview.windows.to_string(), RectF::new(pv.x + 116.0 * s, sy, 30.0 * s, 34.0 * s), font(), 14.0 * s, true, Align::Center, t.fg);
        ui.button(RectF::new(pv.x + 148.0 * s, sy, 34.0 * s, 34.0 * s), "", Some('\u{E710}'), Kind::Secondary, Hit::Do(Action::MoreWindows));
        ui.c.text("Click a window to focus it", RectF::new(pv.x + 200.0 * s, sy, pv.w - 200.0 * s, 34.0 * s), font(), 12.0 * s, false, Align::Right, t.dim);

        // Footer.
        let fy = h - FOOTER_H * s;
        ui.c.line(content.x, fy, w, fy, 1.0, t.line);
        let (dot, msg) = if dirty {
            (t.warn, "You have changes that aren't applied yet")
        } else if !st.running {
            (t.dim, "Tiling is off — Apply starts it")
        } else {
            (t.ok, "Everything is applied")
        };
        ui.c.fill_circle(content.x + 32.0 * s, fy + FOOTER_H * s / 2.0, 4.5 * s, dot);
        ui.c.text(msg, RectF::new(content.x + 46.0 * s, fy, 420.0 * s, FOOTER_H * s), font(), 13.0 * s, false, Align::Left, t.fg);
        let by = fy + (FOOTER_H * s - 38.0 * s) / 2.0;
        let apply_label = if st.running { "Apply" } else { "Apply & start tiling" };
        let aw = ui.c.measure(apply_label, font(), 13.0 * s, true) + 60.0 * s;
        let ax = w - 24.0 * s - aw;
        ui.button(RectF::new(ax, by, aw, 38.0 * s), apply_label, Some('\u{E73E}'), if dirty || !st.running { Kind::Primary } else { Kind::Secondary }, Hit::Do(Action::Apply));
        if dirty {
            ui.button(RectF::new(ax - 118.0 * s, by, 110.0 * s, 38.0 * s), "Discard", None, Kind::Secondary, Hit::Do(Action::Discard));
        }
        if self.model != self.defaults {
            let dx = ax - if dirty { 118.0 } else { 0.0 } * s - 160.0 * s;
            ui.button(RectF::new(dx, by, 152.0 * s, 38.0 * s), "Restore defaults", None, Kind::Secondary, Hit::Do(Action::Defaults));
        }

        // Toast.
        if let Some((msg, error, _)) = &self.toast {
            let tw = (ui.c.measure(msg, font(), 13.0 * s, false) + 60.0 * s).min(w - content.x - 40.0 * s);
            let r = RectF::new(content.x + (w - content.x - tw) / 2.0, fy - 56.0 * s, tw, 42.0 * s);
            ui.c.fill_rrect(r, 21.0 * s, hex(0x2f3541f8));
            ui.c.stroke_rrect(r, 21.0 * s, 1.0 * s, if *error { t.danger } else { ui::with_alpha(t.accent, 0x90) });
            let g = if *error { '\u{E7BA}' } else { '\u{E946}' };
            ui.c.text(&g.to_string(), RectF::new(r.x + 16.0 * s, r.y, 20.0 * s, r.h), ui::icon_font(), 14.0 * s, false, Align::Left, if *error { t.danger } else { t.accent });
            ui.c.text(msg, RectF::new(r.x + 42.0 * s, r.y, r.w - 56.0 * s, r.h), font(), 13.0 * s, false, Align::Left, t.fg);
        }

        ui.c.stroke_rrect(full, 10.0 * s, 1.0 * s, ui::with_alpha(t.fg, 0x22));
        self.hits = hits;
        self.content_h = content_h;
        let _ = self.canvas.end(self.x, self.y, 255);

        if self.preview.animating() {
            unsafe {
                SetTimer(hwnd(self.canvas.hwnd), TIMER_ANIM, 15, None);
            }
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        // Later hits are drawn on top.
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    fn drag_to(&mut self, x: f32) {
        if let Some((n, r)) = self.drag {
            let (lo, hi, _) = n.range();
            let inner_x = r.x + 6.0 * self.s;
            let inner_w = r.w - 12.0 * self.s;
            let f = ((x - inner_x) / inner_w).clamp(0.0, 1.0);
            self.model.set_num(n, lo + f * (hi - lo));
        }
    }

    fn mouse_down(&mut self, x: f32, y: f32) {
        let hit = self.hit_at(x, y);
        self.pressed = hit;
        if let Some(Hit::Slider(n)) = hit {
            let r = self.hits.iter().rev().find(|(r, h)| *h == Hit::Slider(n) && r.contains(x, y)).map(|(r, _)| *r);
            if let Some(r) = r {
                self.drag = Some((n, r));
                unsafe {
                    SetCapture(hwnd(self.canvas.hwnd));
                }
                self.drag_to(x);
                self.draw();
            }
        }
    }

    fn mouse_up(&mut self, x: f32, y: f32) {
        if self.drag.take().is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            self.draw();
            return;
        }
        if let Some(hit) = self.hit_at(x, y).filter(|h| self.pressed == Some(*h)) {
            self.activate(hit);
        }
        self.pressed = None;
    }

    /// Two-step confirmation for destructive actions; true when confirmed.
    fn confirm(&mut self, hit: Hit) -> bool {
        if self.armed.is_some_and(|(h, t)| h == hit && t.elapsed() < Duration::from_secs(CONFIRM_SECS)) {
            self.armed = None;
            true
        } else {
            self.armed = Some((hit, Instant::now()));
            false
        }
    }

    fn activate(&mut self, hit: Hit) {
        if self.armed.is_some_and(|(h, _)| h != hit) {
            self.armed = None;
        }
        match hit {
            Hit::Nav(p) => {
                self.page = p;
                self.scroll = 0.0;
            }
            Hit::Close => {
                if !self.dirty() || self.confirm(hit) {
                    unsafe { PostQuitMessage(0) };
                    return;
                }
            }
            Hit::Minimize => unsafe {
                let _ = ShowWindow(hwnd(self.canvas.hwnd), SW_MINIMIZE);
            },
            Hit::Slider(_) => {}
            Hit::Toggle(f) => self.model.toggle(f),
            Hit::Choice(p, i) => self.model.set_pick(p, i),
            Hit::Color(slot, v) => {
                let c = model::rgba(v);
                match slot {
                    Slot::Active => self.model.active_border.colors[0] = c,
                    Slot::Second if v == 0 => self.model.active_border.colors.truncate(1),
                    Slot::Second => {
                        self.model.active_border.colors.truncate(1);
                        self.model.active_border.colors.push(c);
                    }
                    Slot::Inactive => self.model.inactive_border = c,
                }
            }
            Hit::Preset(i) => self.model.apply_preset(&PRESETS[i]),
            Hit::Module(i) => self.model.toggle_module(BAR_MODULES[i]),
            Hit::Tile(i) => self.preview.focus = i,
            Hit::Do(a) => self.action(a, hit),
        }
        self.draw();
    }

    /// Shows a "working" toast before a blocking system operation.
    fn busy(&mut self, msg: &str) {
        self.say(msg, false);
        self.draw();
    }

    fn action(&mut self, a: Action, hit: Hit) {
        let paths = self.paths.clone();
        match a {
            Action::Apply => {
                self.busy("Applying…");
                match self.apply() {
                    Ok(m) => self.say(m, false),
                    Err(e) => self.say(e, true),
                }
            }
            Action::Discard => {
                self.model = self.saved.clone();
                self.say("Changes discarded", false);
            }
            Action::Defaults => {
                self.model = self.defaults.clone();
                self.say("Default settings loaded — press Apply to use them", false);
            }
            Action::StartStop => {
                if self.status.running {
                    self.busy("Stopping tiling…");
                    match system::stop_wm() {
                        Ok(()) => self.say("Tiling stopped — your windows and taskbar are back to normal", false),
                        Err(e) => self.say(e, true),
                    }
                } else {
                    self.busy("Starting tiling…");
                    let r = (|| {
                        if system::install_state() == Install::Missing {
                            system::install()?;
                        }
                        system::set_disabled(&paths, false);
                        system::start_wm()
                    })();
                    match r {
                        Ok(()) => self.say("Tiling is on", false),
                        Err(e) => self.say(e, true),
                    }
                }
            }
            Action::Reload => match system::reload_wm() {
                Ok(out) if out == "ok" => self.say("Configuration reloaded", false),
                Ok(out) => self.say(out.lines().next().unwrap_or_default().to_string(), true),
                Err(e) => self.say(e, true),
            },
            Action::Install => {
                self.busy("Installing…");
                match system::install() {
                    Ok(stopped) => {
                        let restart = if stopped { system::start_wm() } else { Ok(()) };
                        // Point start-up at the installed copy.
                        if self.status.autostart {
                            let _ = system::set_autostart(&paths, true);
                        }
                        match restart {
                            Ok(()) => self.say(format!("Installed to {}", system::install_dir().display()), false),
                            Err(e) => self.say(e, true),
                        }
                    }
                    Err(e) => self.say(e, true),
                }
            }
            Action::Autostart => {
                let on = !self.status.autostart;
                let r = (|| {
                    if on && system::install_state() == Install::Missing {
                        system::install()?;
                    }
                    system::set_autostart(&paths, on)
                })();
                match r {
                    Ok(()) if on => self.say("deadbeef will start when you sign in", false),
                    Ok(()) => self.say("deadbeef won't start with Windows any more", false),
                    Err(e) => self.say(e, true),
                }
            }
            Action::ResetAll => {
                if self.confirm(hit) {
                    self.busy("Restoring Windows…");
                    match system::reset_everything(&paths) {
                        Ok(_) => {
                            self.model = Model::load(&paths);
                            self.saved = self.model.clone();
                            self.say("Back to normal Windows. Your old settings were saved in backups.", false);
                        }
                        Err(e) => self.say(e, true),
                    }
                }
            }
            Action::Uninstall => {
                if self.confirm(hit) {
                    self.busy("Uninstalling…");
                    match system::uninstall(&paths) {
                        Ok(()) => {
                            crate::message_box(
                                &format!(
                                    "deadbeef was uninstalled and Windows is back to normal.\n\nYour settings are kept in {}.",
                                    paths.root.display()
                                ),
                                false,
                            );
                            unsafe { PostQuitMessage(0) };
                        }
                        Err(e) => self.say(e, true),
                    }
                }
            }
            Action::OpenFolder => {
                if let Err(e) = win::shell_exec(&format!("explorer.exe \"{}\"", paths.active().display())) {
                    self.say(e, true);
                }
            }
            Action::EditConf => {
                if let Err(e) = win::shell_exec(&format!("notepad.exe \"{}\"", paths.active_conf().display())) {
                    self.say(e, true);
                }
            }
            Action::OpenLog => {
                if let Err(e) = win::shell_exec(&format!("notepad.exe \"{}\"", paths.log_file().display())) {
                    self.say(e, true);
                }
            }
            Action::ShowBinds => {
                if !self.status.running {
                    self.say("Start tiling first — the shortcuts belong to the running window manager", true);
                } else if let Err(e) = crate::ipc::client::send(&["dispatch", "binds"]) {
                    self.say(e, true);
                }
            }
            Action::MoreWindows => self.preview.windows = (self.preview.windows + 1).min(preview::MAX_WINDOWS),
            Action::FewerWindows => self.preview.windows = self.preview.windows.saturating_sub(1).max(1),
        }
        self.status = Status::read();
    }

    /// Saves the model and makes the system match it.
    fn apply(&mut self) -> Result<String, String> {
        let paths = self.paths.clone();
        profile::bootstrap(&paths).map_err(|e| format!("could not prepare {}: {e}", paths.root.display()))?;
        self.model.save(&paths).map_err(|e| format!("could not save settings: {e}"))?;
        self.saved = self.model.clone();
        if system::install_state() != Install::Current {
            system::install()?;
        }
        system::set_disabled(&paths, false);
        if system::autostart_wanted(&paths) {
            system::set_autostart(&paths, true)?;
        }
        if system::wm_running() {
            let out = system::reload_wm()?;
            if out != "ok" {
                return Err(format!(
                    "Applied, but the profile has problems: {} (System › Open log)",
                    out.lines().next().unwrap_or_default()
                ));
            }
            Ok("Applied — your windows use the new settings".into())
        } else {
            system::start_wm()?;
            Ok("Applied — tiling is on and will start again after a restart".into())
        }
    }

    fn tick(&mut self) {
        let mut changed = false;
        let st = Status::read();
        if st != self.status {
            self.status = st;
            changed = true;
        }
        if self.toast.as_ref().is_some_and(|t| t.2.elapsed() > Duration::from_secs(TOAST_SECS)) {
            self.toast = None;
            changed = true;
        }
        if self.armed.is_some_and(|a| a.1.elapsed() > Duration::from_secs(CONFIRM_SECS)) {
            self.armed = None;
            changed = true;
        }
        if changed {
            self.draw();
        }
    }
}

fn draw_page(ui: &mut Ui, page: Page, m: &Model, st: Status) {
    match page {
        Page::Overview => {
            if st.running {
                ui.heading("Overview", "deadbeef is tiling your windows. Change anything below or in the other pages, check the preview, then press Apply.");
            } else {
                ui.heading(
                    "Welcome to deadbeef",
                    "deadbeef arranges your windows side by side automatically — no overlapping, no dragging. \
                     Pick a look, check the preview on the right, then press Apply. You can undo everything later from Startup & reset.",
                );
            }
            status_card(ui, st);
            ui.section("Theme");
            ui.presets(m);
            ui.section("Arrangement");
            layout_choice(ui, m);
            ui.choice("Spacing", "Space between windows and around the screen", Pick::Spacing, m);
            ui.note("Tip: once tiling is on, press Win + F1 to see every keyboard shortcut.");
        }
        Page::Layout => {
            ui.heading("Layout", "How windows are arranged when they open and close.");
            ui.section("Arrangement");
            layout_choice(ui, m);
            if m.layout == LayoutKind::Dwindle {
                ui.slider("Split", "How a window shares its space with the next one", Num::SplitRatio, m);
                ui.toggle("Spiral", "Windows wind around the screen like a fibonacci spiral", m.spiral, Hit::Toggle(Flag::Spiral));
            } else {
                ui.slider("Master size", "Share of the screen for the main window", Num::Mfact, m);
                ui.choice("Master side", "Where the main window sits", Pick::Orientation, m);
                ui.toggle("New windows become master", "Otherwise they join the stack", m.new_master, Hit::Toggle(Flag::NewMaster));
            }
            ui.section("Spacing");
            ui.slider("Between windows", "Gap between neighbouring windows", Num::GapsIn, m);
            ui.slider("Screen edge", "Gap between windows and the edge of the screen", Num::GapsOut, m);
        }
        Page::Look => {
            let win11 = win::is_windows11();
            ui.heading("Look", "Colours, borders, corners and transparency.");
            ui.section("Theme");
            ui.presets(m);
            ui.section("Borders");
            let thin = win11 && !m.flag(Flag::Overlay);
            let desc = if thin { "Windows 11 draws 1 px borders — turn on thick borders below" } else { "Outline around each window" };
            ui.slider("Border width", desc, Num::BorderSize, m);
            if win11 {
                ui.toggle("Thick & gradient borders", "deadbeef draws the border itself instead of Windows", m.flag(Flag::Overlay), Hit::Toggle(Flag::Overlay));
            }
            ui.swatches("Focused window", "Border of the window you are using", Slot::Active, ACCENTS, Some(m.active_border.primary()), false);
            ui.swatches("Gradient", "Blend the focused border into a second colour", Slot::Second, ACCENTS, m.active_border.colors.get(1).copied(), true);
            ui.swatches("Other windows", "Border of windows in the background", Slot::Inactive, INACTIVE, Some(m.inactive_border), false);
            ui.section("Windows");
            ui.choice("Corners", if win11 { "" } else { "Rounded corners need Windows 11" }, Pick::Corners, m);
            ui.slider("Focused window opacity", "", Num::ActiveOpacity, m);
            ui.slider("Other windows opacity", "Below 100% background windows turn see-through", Num::InactiveOpacity, m);
            ui.toggle("Backdrop effect", "Mica / acrylic behind apps that support it (Windows 11)", m.blur, Hit::Toggle(Flag::Blur));
            if m.blur {
                ui.choice("Backdrop", "", Pick::Backdrop, m);
            }
            ui.toggle("Animations", "Windows glide into place", m.animations, Hit::Toggle(Flag::Animations));
        }
        Page::Bar => {
            ui.heading("Top bar", "deadbeef can draw its own bar at the top of the screen with workspaces, apps, clock and system stats.");
            ui.toggle("Show the top bar", "", m.bar, Hit::Toggle(Flag::Bar));
            ui.toggle("Hide the Windows taskbar", "It comes back automatically when tiling stops", m.hide_taskbar, Hit::Toggle(Flag::HideTaskbar));
            ui.section("Bar");
            ui.choice("Show on", "", Pick::BarMonitors, m);
            ui.slider("Height", "", Num::BarHeight, m);
            ui.slider("Text size", "", Num::BarFontSize, m);
            ui.choice("Clock", "", Pick::Clock, m);
            ui.modules(m);
            ui.note("The bar uses the colours of the theme chosen on the Look page.");
        }
        Page::Behaviour => {
            ui.heading("Behaviour", "How focus, keyboard shortcuts and special windows work.");
            ui.section("Focus");
            ui.toggle("Focus follows the mouse", "Point at a window to make it active, no click needed", m.follow_mouse, Hit::Toggle(Flag::FollowMouse));
            ui.section("Windows");
            ui.toggle("Float dialogs", "Pop-ups and dialogs stay floating instead of tiling", m.float_dialogs, Hit::Toggle(Flag::FloatDialogs));
            ui.toggle("Float fixed-size windows", "Windows that can't be resized stay floating", m.float_unresizable, Hit::Toggle(Flag::FloatUnresizable));
            ui.section("Keyboard");
            ui.toggle("Use Win + key shortcuts", "Take over shortcuts Windows reserves (Win+E, Win+1…)", m.hotkey_fallback, Hit::Toggle(Flag::HotkeyFallback));
            ui.toggle("Report problems", "Show a notification when the configuration has errors", m.notify_errors, Hit::Toggle(Flag::NotifyErrors));
            ui.button_row(&[
                ("Show shortcuts", '\u{E765}', Kind::Secondary, Action::ShowBinds),
                ("Edit shortcuts", '\u{E70F}', Kind::Secondary, Action::EditConf),
            ]);
            ui.note("Shortcuts are written in deadbeef.conf as `bind = SUPER, KEY, action`. Saving the file applies them immediately.");
        }
        Page::System => {
            ui.heading("Startup & reset", "Control when deadbeef runs, and put Windows back to normal at any time.");
            status_card(ui, st);
            ui.section("Startup");
            ui.toggle(
                "Start tiling when I sign in",
                "Your layout and settings come back after every restart",
                st.autostart,
                Hit::Do(Action::Autostart),
            );
            ui.section("Installation");
            let (label, desc) = match st.install {
                Install::Current => ("Installed", system::install_dir().display().to_string()),
                Install::Outdated => ("A different version is installed", "Install to replace it with this one".to_string()),
                Install::Missing => ("Not installed yet", "Apply installs it for you".to_string()),
            };
            let s = ui.s;
            let r = ui.card(60.0 * s, None);
            ui.labels(r, label, &desc, if st.install == Install::Current { 0.0 } else { 140.0 * s });
            if st.install != Install::Current {
                let b = RectF::new(r.right() - 140.0 * s, r.y + 12.0 * s, 124.0 * s, 36.0 * s);
                ui.button(b, "Install", Some(glyph::DOWNLOAD), Kind::Primary, Hit::Do(Action::Install));
            }
            ui.section("Advanced");
            ui.button_row(&[
                ("Reload", '\u{E72C}', Kind::Secondary, Action::Reload),
                ("Config folder", glyph::FOLDER, Kind::Secondary, Action::OpenFolder),
                ("Edit config", '\u{E70F}', Kind::Secondary, Action::EditConf),
                ("Log", '\u{E7C3}', Kind::Secondary, Action::OpenLog),
            ]);
            ui.section("Reset");
            let r = ui.card(150.0 * s, None);
            ui.c.text("Back to normal Windows", RectF::new(r.x + 16.0 * s, r.y + 10.0 * s, r.w - 32.0 * s, 24.0 * s), font(), 13.5 * s, true, Align::Left, ui.t.fg);
            ui.c.paragraph(
                "Stops tiling, puts your windows and the Windows taskbar back as they were, turns off start-up and restores the default settings. \
                 A backup of your current settings is kept.",
                r.x + 16.0 * s,
                r.y + 36.0 * s,
                r.w - 32.0 * s,
                font(),
                12.0 * s,
                ui.t.dim,
            );
            let by = r.bottom() - 50.0 * s;
            ui.button(RectF::new(r.x + 16.0 * s, by, 220.0 * s, 36.0 * s), "Reset everything", Some('\u{E777}'), Kind::Danger, Hit::Do(Action::ResetAll));
            if st.install != Install::Missing {
                ui.button(RectF::new(r.x + 246.0 * s, by, 220.0 * s, 36.0 * s), "Uninstall", Some('\u{E74D}'), Kind::Danger, Hit::Do(Action::Uninstall));
            }
        }
    }
}

fn layout_choice(ui: &mut Ui, m: &Model) {
    let desc = match m.layout {
        LayoutKind::Dwindle => "Dwindle: each new window splits the focused one in half",
        LayoutKind::Master => "Master: one big window, the others stacked beside it",
    };
    ui.choice("Layout", desc, Pick::Layout, m);
}

fn status_card(ui: &mut Ui, st: Status) {
    let s = ui.s;
    let r = ui.card(76.0 * s, None);
    let (dot, title) = if st.running { (ui.t.ok, "Tiling is on") } else { (ui.t.dim, "Tiling is off") };
    ui.c.fill_circle(r.x + 26.0 * s, r.y + 27.0 * s, 6.0 * s, dot);
    ui.c.text(title, RectF::new(r.x + 42.0 * s, r.y + 14.0 * s, r.w - 220.0 * s, 26.0 * s), font(), 15.0 * s, true, Align::Left, ui.t.fg);
    let sub = match (st.autostart, st.install) {
        (true, _) => "Starts automatically when you sign in".to_string(),
        (false, Install::Missing) => "Not set up yet — Apply sets everything up".to_string(),
        (false, _) => "Doesn't start with Windows".to_string(),
    };
    ui.c.text(&sub, RectF::new(r.x + 42.0 * s, r.y + 40.0 * s, r.w - 220.0 * s, 22.0 * s), font(), 12.0 * s, false, Align::Left, ui.t.dim);
    let b = RectF::new(r.right() - 166.0 * s, r.y + 20.0 * s, 150.0 * s, 36.0 * s);
    if st.running {
        ui.button(b, "Stop tiling", Some('\u{E71A}'), Kind::Secondary, Hit::Do(Action::StartStop));
    } else {
        ui.button(b, "Start tiling", Some('\u{E768}'), Kind::Secondary, Hit::Do(Action::StartStop));
    }
}

fn point(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            let (sx, sy) = point(lp);
            let r = with_app(|a| {
                let (x, y) = (sx - a.x as f32, sy - a.y as f32);
                if y >= 0.0 && y < TITLE_H * a.s && a.hit_at(x, y).is_none() && a.drag.is_none() {
                    Some(LRESULT(HTCAPTION as isize))
                } else {
                    None
                }
            })
            .flatten();
            r.unwrap_or(LRESULT(HTCLIENT as isize))
        }
        WM_MOVE => {
            let (x, y) = point(lp);
            with_app(|a| {
                a.x = x as i32;
                a.y = y as i32;
            });
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let hand = with_app(|a| a.hover.is_some_and(|h| !matches!(h, Hit::Slider(_))) || a.drag.is_some()).unwrap_or(false);
            let cursor = LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).unwrap_or_default();
            SetCursor(cursor);
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            with_app(|a| {
                if a.drag.is_some() {
                    let before = a.model.clone();
                    a.drag_to(x);
                    if a.model != before {
                        a.draw();
                    }
                    return;
                }
                let hit = a.hit_at(x, y);
                if hit != a.hover {
                    a.hover = hit;
                    a.draw();
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|a| {
                if a.hover.is_some() && a.drag.is_none() {
                    a.hover = None;
                    a.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lp);
            with_app(|a| a.mouse_down(x, y));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lp);
            with_app(|a| a.mouse_up(x, y));
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            with_app(|a| a.drag = None);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            with_app(|a| {
                a.scroll = (a.scroll - delta / 120.0 * 72.0 * a.s).clamp(0.0, a.max_scroll());
                a.hover = None;
                a.draw();
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            match wp.0 as u32 {
                0x53 if ctrl => {
                    with_app(|a| a.activate(Hit::Do(Action::Apply)));
                }
                0x21 | 0x22 => {
                    with_app(|a| {
                        let page = a.content_rect().h * 0.8;
                        let d = if wp.0 == 0x21 { -page } else { page };
                        a.scroll = (a.scroll + d).clamp(0.0, a.max_scroll());
                        a.draw();
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_STATUS => {
            with_app(|a| a.tick());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_ANIM => {
            with_app(|a| {
                let more = a.preview.animating();
                a.draw();
                if !more {
                    let _ = KillTimer(h, TIMER_ANIM);
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            with_app(|a| a.activate(Hit::Close));
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
