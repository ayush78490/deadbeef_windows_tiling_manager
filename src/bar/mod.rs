//! The deadbeef bar: a top-edge AppBar on each monitor that replaces the
//! Windows taskbar.
//!
//! Runs on its own thread (own message loop, own COM apartment), so stat
//! sampling or a slow icon query can never delay tiling. The window manager
//! pushes a [`Snapshot`] (workspaces, apps, focused title); the bar sends
//! [`BarCmd`]s back. Each bar window is registered with `SHAppBarMessage`, so
//! Windows itself shrinks the work area and tiled windows sit below it.
//! Clicking a module opens a small flyout with details and actions.

pub mod clock;
pub mod sysinfo;

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::size_of;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, SHGetFileInfoW, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA, SHFILEINFOW,
    SHGFI_ICON, SHGFI_SMALLICON,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::types::{BarConfig, Rgba};
use crate::ui::{self, glyph, image, Align, Canvas, RectF};
use crate::win::{self, hwnd, raw, Monitor};
use sysinfo::{Sampler, Stats};

const WM_BAR_SNAPSHOT: u32 = WM_APP + 20;
const WM_BAR_CONFIG: u32 = WM_APP + 21;
const WM_BAR_APPBAR: u32 = WM_APP + 22;
const TIMER_TICK: usize = 1;
const TIMER_REBUILD: usize = 2;
const ABN_POSCHANGED: usize = 1;
const ABN_FULLSCREENAPP: usize = 2;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppInfo {
    pub hwnd: isize,
    pub title: String,
    pub exe: String,
    pub focused: bool,
    pub minimized: bool,
    pub monitor: usize,
}

/// What the window manager tells the bar.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub workspaces: u32,
    pub current: u32,
    pub occupied: Vec<u32>,
    pub apps: Vec<AppInfo>,
    pub title: String,
}

/// What the bar asks the window manager to do.
#[derive(Debug, Clone)]
pub enum BarCmd {
    Dispatch(String, String),
}

struct Shared {
    snapshot: Snapshot,
    config: BarConfig,
}

pub struct BarHandle {
    ctl: isize,
    shared: Arc<Mutex<Shared>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub commands: Receiver<BarCmd>,
    last: Snapshot,
    config: BarConfig,
}

impl BarHandle {
    /// Starts the bar thread. Commands are announced to `main` with `cmd_msg`.
    pub fn spawn(config: BarConfig, main: isize, cmd_msg: u32) -> Result<BarHandle, String> {
        let shared = Arc::new(Mutex::new(Shared { snapshot: Snapshot::default(), config: config.clone() }));
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("deadbeef-bar".into())
            .spawn(move || run(s, main, cmd_msg, tx, ready_tx))
            .map_err(|e| e.to_string())?;
        let ctl = ready_rx.recv().map_err(|_| "bar thread died during startup".to_string())??;
        Ok(BarHandle { ctl, shared, thread: Some(thread), commands: rx, last: Snapshot::default(), config })
    }

    pub fn update(&mut self, snap: Snapshot) {
        if snap == self.last {
            return;
        }
        self.last = snap.clone();
        self.shared.lock().unwrap().snapshot = snap;
        unsafe {
            let _ = PostMessageW(hwnd(self.ctl), WM_BAR_SNAPSHOT, WPARAM(0), LPARAM(0));
        }
    }

    /// Re-registers the AppBars (e.g. after explorer.exe restarted).
    pub fn rebuild(&self) {
        unsafe {
            let _ = PostMessageW(hwnd(self.ctl), WM_DISPLAYCHANGE, WPARAM(0), LPARAM(0));
        }
    }

    pub fn configure(&mut self, config: BarConfig) {
        if config == self.config {
            return;
        }
        self.config = config.clone();
        self.shared.lock().unwrap().config = config;
        unsafe {
            let _ = PostMessageW(hwnd(self.ctl), WM_BAR_CONFIG, WPARAM(0), LPARAM(0));
        }
    }
}

impl Drop for BarHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(hwnd(self.ctl), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ───────────────────────────── bar thread ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Module {
    Clock,
    Cpu,
    Memory,
    Temperature,
    Volume,
    Wifi,
    Bluetooth,
    Battery,
    Power,
}

#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Workspace(u32),
    App(isize),
    Launcher,
    Wallpaper,
    Popup(Module),
    Exec(String),
    Mute,
    VolumeSlider(f32, f32),
    Lock,
    Sleep,
}

struct BarWin {
    canvas: Canvas,
    monitor: Monitor,
    rect: RECT,
    hits: Vec<(RectF, Hit)>,
    hover: Option<Hit>,
    icons: HashMap<isize, Option<ID2D1Bitmap>>,
    fullscreen_hidden: bool,
    tracking: bool,
}

impl Drop for BarWin {
    fn drop(&mut self) {
        let mut abd = appbar_data(self.canvas.hwnd);
        unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
    }
}

struct Popup {
    canvas: Canvas,
    module: Module,
    bar: usize,
    anchor: f32,
    hits: Vec<(RectF, Hit)>,
    hover: Option<Hit>,
}

struct BarThread {
    main: isize,
    cmd_msg: u32,
    tx: Sender<BarCmd>,
    shared: Arc<Mutex<Shared>>,
    cfg: BarConfig,
    snap: Snapshot,
    sampler: Sampler,
    bars: Vec<BarWin>,
    popup: Option<Popup>,
    /// (module, when) of the last popup close — a click on the same module
    /// right after means "toggle closed", not "reopen".
    closed: Option<(Module, Instant)>,
    tick: u32,
}

thread_local! {
    static BAR: RefCell<Option<BarThread>> = const { RefCell::new(None) };
}

fn with_bar<R>(f: impl FnOnce(&mut BarThread) -> R) -> Option<R> {
    BAR.with(|b| {
        let mut g = b.try_borrow_mut().ok()?;
        Some(f(g.as_mut()?))
    })
}

fn run(shared: Arc<Mutex<Shared>>, main: isize, cmd_msg: u32, tx: Sender<BarCmd>, ready: Sender<Result<isize, String>>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    ui::register_class(w!("deadbeef_barctl"), Some(ctl_proc));
    ui::register_class(w!("deadbeef_bar"), Some(bar_proc));
    ui::register_class(w!("deadbeef_popup"), Some(popup_proc));
    let ctl = unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("deadbeef_barctl"),
            w!("deadbeef bar"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )
    };
    let ctl = match ctl {
        Ok(h) => raw(h),
        Err(e) => {
            let _ = ready.send(Err(format!("bar control window: {e}")));
            return;
        }
    };
    let _ = ready.send(Ok(ctl));
    let cfg = shared.lock().unwrap().config.clone();
    BAR.with(|b| {
        *b.borrow_mut() = Some(BarThread {
            main,
            cmd_msg,
            tx,
            shared,
            cfg,
            snap: Snapshot::default(),
            sampler: Sampler::new(),
            bars: Vec::new(),
            popup: None,
            closed: None,
            tick: 0,
        })
    });
    with_bar(|t| t.rebuild());
    unsafe { SetTimer(hwnd(ctl), TIMER_TICK, 1000, None) };

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    BAR.with(|b| b.borrow_mut().take());
    unsafe {
        let _ = DestroyWindow(hwnd(ctl));
        CoUninitialize();
    }
}

fn appbar_data(h: isize) -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd(h),
        uCallbackMessage: WM_BAR_APPBAR,
        uEdge: ABE_TOP,
        ..Default::default()
    }
}

/// Claims a band at the top of `m` and returns the rect Windows granted.
fn reserve(h: isize, m: &Monitor, height: i32) -> RECT {
    let mut abd = appbar_data(h);
    abd.rc = RECT { left: m.rect.x, top: m.rect.y, right: m.rect.right(), bottom: m.rect.y + height };
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.bottom = abd.rc.top + height;
        SHAppBarMessage(ABM_SETPOS, &mut abd);
    }
    abd.rc
}

impl BarThread {
    fn scale(m: &Monitor) -> f32 {
        m.dpi as f32 / 96.0
    }

    /// (Re)creates one bar per monitor.
    fn rebuild(&mut self) {
        self.popup = None;
        self.bars.clear();
        let monitors = win::monitors();
        for m in monitors.into_iter().filter(|m| self.cfg.all_monitors || m.primary) {
            let canvas = match Canvas::new(
                w!("deadbeef_bar"),
                "deadbeef bar",
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                true,
            ) {
                Ok(c) => c,
                Err(e) => {
                    crate::log_error!("bar: {e}");
                    continue;
                }
            };
            let height = (self.cfg.height as f32 * Self::scale(&m)).round() as i32;
            let mut abd = appbar_data(canvas.hwnd);
            unsafe { SHAppBarMessage(ABM_NEW, &mut abd) };
            let rect = reserve(canvas.hwnd, &m, height);
            self.bars.push(BarWin {
                canvas,
                monitor: m,
                rect,
                hits: Vec::new(),
                hover: None,
                icons: HashMap::new(),
                fullscreen_hidden: false,
                tracking: false,
            });
        }
        crate::log_info!("bar: {} bar(s) created", self.bars.len());
        self.redraw_all();
    }

    fn redraw_all(&mut self) {
        for i in 0..self.bars.len() {
            self.draw_bar(i);
        }
        if self.popup.is_some() {
            self.draw_popup();
        }
    }

    fn send(&self, cmd: BarCmd) {
        if self.tx.send(cmd).is_ok() {
            unsafe {
                let _ = PostMessageW(hwnd(self.main), self.cmd_msg, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn bar_index(&self, h: isize) -> Option<usize> {
        self.bars.iter().position(|b| b.canvas.hwnd == h)
    }

    // ───────────────────────────── rendering ────────────────────────────

    fn draw_bar(&mut self, i: usize) {
        let BarThread { cfg, snap, sampler, bars, .. } = self;
        let bar = &mut bars[i];
        if bar.fullscreen_hidden {
            return;
        }
        let s = Self::scale(&bar.monitor);
        let (w, h) = (bar.rect.right - bar.rect.left, bar.rect.bottom - bar.rect.top);
        let mon_index = bar.monitor.index;

        // Load icons for apps we haven't seen; drop ones that are gone.
        let wanted: Vec<isize> = snap.apps.iter().filter(|a| a.monitor == mon_index).map(|a| a.hwnd).collect();
        bar.icons.retain(|h, _| wanted.contains(h));
        for hw in &wanted {
            if !bar.icons.contains_key(hw) {
                let bmp = app_icon(*hw).and_then(|p| bar.canvas.bitmap(p.w, p.h, &p.data));
                bar.icons.insert(*hw, bmp);
            }
        }

        let ctx = Ctx { cfg, snap, stats: &sampler.stats, monitor: mon_index, s };
        let left: Vec<Pill> = cfg.modules_left.iter().filter_map(|m| ctx.module(m)).collect();
        let center: Vec<Pill> = cfg.modules_center.iter().filter_map(|m| ctx.module(m)).collect();
        let right: Vec<Pill> = cfg.modules_right.iter().filter_map(|m| ctx.module(m)).collect();

        if bar.canvas.begin(w, h).is_err() {
            return;
        }
        let mut hits = Vec::new();
        let font = text_font(cfg);
        let mut lay = Layout { canvas: &mut bar.canvas, cfg, s, h: h as f32, font: &font, hover: bar.hover.clone(), icons: &bar.icons };
        let outer = 8.0 * s;
        let between = 6.0 * s;

        let widths: Vec<f32> = left.iter().map(|p| lay.pill_width(p)).collect();
        let mut x = outer;
        for (p, pw) in left.iter().zip(&widths) {
            lay.draw_pill(p, x, *pw, &mut hits);
            x += pw + between;
        }
        let left_end = x;

        let widths: Vec<f32> = right.iter().map(|p| lay.pill_width(p)).collect();
        let mut x = w as f32 - outer;
        for (p, pw) in right.iter().zip(&widths).rev() {
            x -= pw;
            lay.draw_pill(p, x, *pw, &mut hits);
            x -= between;
        }
        let right_start = x;

        let widths: Vec<f32> = center.iter().map(|p| lay.pill_width(p)).collect();
        let total: f32 = widths.iter().sum::<f32>() + between * widths.len().saturating_sub(1) as f32;
        let mut x = ((w as f32 - total) / 2.0).max(left_end).min((right_start - total).max(left_end));
        for (p, pw) in center.iter().zip(&widths) {
            lay.draw_pill(p, x, *pw, &mut hits);
            x += pw + between;
        }

        bar.hits = hits;
        if let Err(e) = bar.canvas.end(bar.rect.left, bar.rect.top, 255) {
            crate::log_warn!("bar draw: {e}");
        }
    }

    fn draw_popup(&mut self) {
        let BarThread { cfg, sampler, bars, popup, .. } = self;
        let Some(p) = popup.as_mut() else { return };
        let Some(bar) = bars.get(p.bar) else { return };
        let s = Self::scale(&bar.monitor);
        let rows = popup_rows(p.module, cfg, &sampler.stats);
        let font = text_font(cfg);
        let width = 300.0 * s;
        let height = rows.iter().map(|r| r.height(s)).sum::<f32>() + 28.0 * s;
        let (w, h) = (width.round() as i32, height.round() as i32);
        let m = &bar.monitor.rect;
        let x = ((p.anchor - width / 2.0) as i32).clamp(m.x + (8.0 * s) as i32, m.right() - w - (8.0 * s) as i32);
        let y = bar.rect.bottom + (6.0 * s) as i32;
        if p.canvas.begin(w, h).is_err() {
            return;
        }
        let c = &mut p.canvas;
        let full = RectF::new(0.0, 0.0, width, height);
        c.fill_rrect(full, 12.0 * s, Rgba { a: 0xf2, ..cfg.background });
        c.stroke_rrect(full, 12.0 * s, 1.0 * s, ui::with_alpha(cfg.accent, 0x80));
        let mut hits = Vec::new();
        let mut y0 = 14.0 * s;
        for r in &rows {
            let rh = r.height(s);
            r.draw(c, cfg, &font, RectF::new(14.0 * s, y0, width - 28.0 * s, rh), s, p.hover.as_ref(), &mut hits);
            y0 += rh;
        }
        p.hits = hits;
        let _ = p.canvas.end(x, y, 255);
    }

    fn open_popup(&mut self, bar: usize, module: Module, anchor: f32) {
        if let Some((m, t)) = self.closed {
            if m == module && t.elapsed().as_millis() < 350 {
                self.closed = None;
                return; // the click that closed it was on this module: toggle
            }
        }
        if self.popup.as_ref().is_some_and(|p| p.module == module) {
            self.close_popup();
            return;
        }
        self.popup = None;
        let canvas = match Canvas::new(w!("deadbeef_popup"), "deadbeef flyout", WS_EX_TOOLWINDOW | WS_EX_TOPMOST, true) {
            Ok(c) => c,
            Err(e) => {
                crate::log_warn!("bar popup: {e}");
                return;
            }
        };
        let h = canvas.hwnd;
        self.popup = Some(Popup { canvas, module, bar, anchor, hits: Vec::new(), hover: None });
        self.sampler.sample();
        self.draw_popup();
        unsafe {
            let _ = SetForegroundWindow(hwnd(h));
        }
    }

    fn close_popup(&mut self) {
        if let Some(p) = self.popup.take() {
            self.closed = Some((p.module, Instant::now()));
        }
    }

    // ────────────────────────────── input ───────────────────────────────

    fn on_click(&mut self, bar: usize, x: f32, y: f32, button: u32) {
        let Some(hit) = self.bars[bar].hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(r, h)| (*r, h.clone())) else {
            return;
        };
        let (rect, hit) = hit;
        match (hit, button) {
            (Hit::App(h), WM_MBUTTONUP) => win::close(h),
            (Hit::App(h), _) => {
                let focused = self.snap.apps.iter().any(|a| a.hwnd == h && a.focused && !a.minimized);
                if focused {
                    unsafe {
                        let _ = ShowWindow(hwnd(h), SW_MINIMIZE);
                    }
                } else {
                    win::focus(h);
                }
            }
            (Hit::Workspace(n), _) => self.send(BarCmd::Dispatch("workspace".into(), n.to_string())),
            (Hit::Launcher, _) => self.send(BarCmd::Dispatch("launcher".into(), String::new())),
            (Hit::Wallpaper, _) => self.send(BarCmd::Dispatch("wallpaper".into(), String::new())),
            (Hit::Popup(m), _) => {
                let anchor = self.bars[bar].rect.left as f32 + rect.x + rect.w / 2.0;
                self.open_popup(bar, m, anchor);
            }
            (other, _) => self.run_action(other, x),
        }
    }

    fn on_wheel(&mut self, bar: usize, x: f32, y: f32, delta: i16) {
        let Some(hit) = self.bars[bar].hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone()) else {
            return;
        };
        match hit {
            Hit::Workspace(_) => {
                let arg = if delta > 0 { "e-1" } else { "e+1" };
                self.send(BarCmd::Dispatch("workspace".into(), arg.into()));
            }
            Hit::Popup(Module::Volume) => {
                self.sampler.change_volume(if delta > 0 { 5.0 } else { -5.0 });
                self.redraw_all();
            }
            _ => {}
        }
    }

    /// Actions shared by bar and flyout clicks.
    fn run_action(&mut self, hit: Hit, local_x: f32) {
        match hit {
            Hit::Exec(cmd) => {
                if let Err(e) = win::shell_exec(&cmd) {
                    crate::log_warn!("bar: {e}");
                }
                self.close_popup();
            }
            Hit::Mute => {
                self.sampler.toggle_mute();
                self.redraw_all();
            }
            Hit::VolumeSlider(x0, w) => {
                let target = ((local_x - x0) / w * 100.0).clamp(0.0, 100.0);
                let current = self.sampler.stats.volume.map(|v| v.0).unwrap_or(0.0);
                self.sampler.change_volume(target - current);
                self.redraw_all();
            }
            Hit::Lock => {
                self.close_popup();
                unsafe {
                    let _ = windows::Win32::System::Shutdown::LockWorkStation();
                }
            }
            Hit::Sleep => {
                self.close_popup();
                unsafe {
                    let _ = windows::Win32::System::Power::SetSuspendState(false, false, false);
                }
            }
            _ => {}
        }
    }
}

// ────────────────────────────── layout ────────────────────────────────

enum Kind {
    Icon(char),
    Text(String, bool),
    App(isize, char),
}

struct Item {
    kind: Kind,
    color: Rgba,
    hit: Option<Hit>,
    highlight: Option<Rgba>,
    max_w: Option<f32>,
    underline: bool,
    opacity: f32,
}

impl Item {
    fn new(kind: Kind, color: Rgba) -> Item {
        Item { kind, color, hit: None, highlight: None, max_w: None, underline: false, opacity: 1.0 }
    }
    fn hit(mut self, h: Hit) -> Item {
        self.hit = Some(h);
        self
    }
}

struct Pill {
    items: Vec<Item>,
    hit: Option<Hit>,
}

struct Ctx<'a> {
    cfg: &'a BarConfig,
    snap: &'a Snapshot,
    stats: &'a Stats,
    monitor: usize,
    s: f32,
}

fn text_font(cfg: &BarConfig) -> String {
    if cfg.font.is_empty() {
        ui::text_font().to_string()
    } else {
        cfg.font.clone()
    }
}

fn level(cfg: &BarConfig, v: f32, warn: f32, crit: f32) -> Rgba {
    if v >= crit {
        cfg.critical
    } else if v >= warn {
        cfg.warning
    } else {
        cfg.foreground
    }
}

impl Ctx<'_> {
    fn module(&self, name: &str) -> Option<Pill> {
        let c = self.cfg;
        let st = self.stats;
        let icon = |g: char, col: Rgba| Item::new(Kind::Icon(g), col);
        let text = |t: String, col: Rgba| Item::new(Kind::Text(t, false), col);
        let pill = |items: Vec<Item>, hit: Option<Hit>| Some(Pill { items, hit });
        match name {
            "launcher" => pill(vec![icon(glyph::HOME, c.accent).hit(Hit::Launcher)], Some(Hit::Launcher)),
            "wallpaper" => pill(vec![icon(glyph::IMAGE, c.foreground).hit(Hit::Wallpaper)], Some(Hit::Wallpaper)),
            "workspaces" => {
                let max = self.snap.workspaces.max(self.snap.occupied.iter().copied().max().unwrap_or(1)).max(1);
                let items = (1..=max)
                    .map(|n| {
                        let current = n == self.snap.current;
                        let occupied = self.snap.occupied.contains(&n);
                        let col = if current {
                            Rgba { a: 0xff, ..c.background }
                        } else if occupied {
                            c.foreground
                        } else {
                            c.dim
                        };
                        let mut it = Item::new(Kind::Text(n.to_string(), current), col).hit(Hit::Workspace(n));
                        if current {
                            it.highlight = Some(c.accent);
                        }
                        it
                    })
                    .collect();
                pill(items, None)
            }
            "apps" => {
                let items: Vec<Item> = self
                    .snap
                    .apps
                    .iter()
                    .filter(|a| a.monitor == self.monitor)
                    .map(|a| {
                        let letter = a.exe.chars().next().unwrap_or('?').to_ascii_uppercase();
                        let mut it = Item::new(Kind::App(a.hwnd, letter), c.foreground).hit(Hit::App(a.hwnd));
                        it.underline = a.focused;
                        it.opacity = if a.minimized { 0.45 } else { 1.0 };
                        it
                    })
                    .collect();
                if items.is_empty() {
                    None
                } else {
                    pill(items, None)
                }
            }
            "title" => {
                if self.snap.title.is_empty() {
                    return None;
                }
                let mut it = text(self.snap.title.clone(), c.foreground);
                it.max_w = Some(320.0 * self.s);
                pill(vec![it], None)
            }
            "clock" => {
                let t = win::local_time();
                let mut it = Item::new(Kind::Text(clock::format(&t, &c.clock_format), true), c.foreground);
                it.hit = Some(Hit::Popup(Module::Clock));
                pill(vec![it], Some(Hit::Popup(Module::Clock)))
            }
            "cpu" => pill(
                vec![icon(glyph::CPU, c.accent), text(format!("{:.0}%", st.cpu), level(c, st.cpu, 70.0, 90.0))],
                Some(Hit::Popup(Module::Cpu)),
            ),
            "memory" => pill(
                vec![icon(glyph::RAM, c.accent), text(format!("{:.0}%", st.mem_pct()), level(c, st.mem_pct(), 80.0, 92.0))],
                Some(Hit::Popup(Module::Memory)),
            ),
            "temperature" => {
                let t = st.max_temp()?;
                pill(
                    vec![icon(glyph::TEMP, c.accent), text(format!("{t:.0}°"), level(c, t, 75.0, 90.0))],
                    Some(Hit::Popup(Module::Temperature)),
                )
            }
            "volume" => {
                let (v, muted) = st.volume?;
                let (g, col) = if muted { (glyph::MUTE, c.dim) } else { (glyph::VOLUME, c.accent) };
                pill(vec![icon(g, col), text(format!("{v:.0}%"), c.foreground)], Some(Hit::Popup(Module::Volume)))
            }
            "wifi" => {
                let wifi = st.wifi.as_ref()?;
                let label = if !wifi.connected {
                    "Offline".to_string()
                } else {
                    let ssid = wifi.ssid.clone().unwrap_or_else(|| "Wi-Fi".into());
                    if ssid.chars().count() > 14 {
                        format!("{}…", ssid.chars().take(13).collect::<String>())
                    } else {
                        ssid
                    }
                };
                let col = if wifi.connected { c.accent } else { c.dim };
                pill(vec![icon(glyph::WIFI, col), text(label, c.foreground)], Some(Hit::Popup(Module::Wifi)))
            }
            "bluetooth" => {
                let bt = &st.bluetooth;
                if !bt.present {
                    return None;
                }
                let col = if !bt.connected.is_empty() {
                    c.accent
                } else if bt.on {
                    c.foreground
                } else {
                    c.dim
                };
                let mut items = vec![icon(glyph::BLUETOOTH, col)];
                if !bt.connected.is_empty() {
                    items.push(text(bt.connected.len().to_string(), c.foreground));
                }
                pill(items, Some(Hit::Popup(Module::Bluetooth)))
            }
            "battery" => {
                let b = st.battery.as_ref()?;
                let col = if b.charging || b.on_ac {
                    c.accent
                } else {
                    level(c, 100.0 - b.pct as f32, 80.0, 90.0)
                };
                pill(
                    vec![icon(glyph::battery(b.pct, b.charging), col), text(format!("{}%", b.pct), c.foreground)],
                    Some(Hit::Popup(Module::Battery)),
                )
            }
            "power" => pill(vec![icon(glyph::POWER, c.critical)], Some(Hit::Popup(Module::Power))),
            _ => None,
        }
    }
}

struct Layout<'a> {
    canvas: &'a mut Canvas,
    cfg: &'a BarConfig,
    s: f32,
    h: f32,
    font: &'a str,
    hover: Option<Hit>,
    icons: &'a HashMap<isize, Option<ID2D1Bitmap>>,
}

impl Layout<'_> {
    fn fs(&self) -> f32 {
        self.cfg.font_size * self.s
    }
    fn is(&self) -> f32 {
        self.cfg.font_size * 1.15 * self.s
    }
    fn item_width(&mut self, it: &Item) -> f32 {
        let w = match &it.kind {
            Kind::Icon(_) => self.is(),
            Kind::Text(t, bold) => {
                let (font, fs) = (self.font.to_string(), self.fs());
                (self.canvas.measure(t, &font, fs, *bold) + 2.0 * self.s).ceil()
            }
            Kind::App(..) => self.is() + 6.0 * self.s,
        };
        let w = it.max_w.map_or(w, |m| w.min(m));
        if it.highlight.is_some() {
            w.max(self.is()) + 6.0 * self.s
        } else {
            w
        }
    }
    fn pill_width(&mut self, p: &Pill) -> f32 {
        let gap = 7.0 * self.s;
        let sum: f32 = p.items.iter().map(|i| self.item_width(i)).sum();
        sum + gap * p.items.len().saturating_sub(1) as f32 + 2.0 * 11.0 * self.s
    }

    fn draw_pill(&mut self, p: &Pill, x: f32, w: f32, hits: &mut Vec<(RectF, Hit)>) {
        let s = self.s;
        let m = 4.0 * s;
        let r = RectF::new(x, m, w, self.h - 2.0 * m);
        let radius = 9.0 * s;
        self.canvas.fill_rrect(r, radius, self.cfg.background);
        if p.hit.is_some() && p.hit == self.hover {
            self.canvas.fill_rrect(r, radius, ui::with_alpha(self.cfg.foreground, 0x22));
        }
        if let Some(h) = &p.hit {
            hits.push((r, h.clone()));
        }
        let mut cx = x + 11.0 * s;
        let gap = 7.0 * s;
        for it in &p.items {
            let iw = self.item_width(it);
            let ir = RectF::new(cx, r.y, iw, r.h);
            if let Some(hl) = it.highlight {
                self.canvas.fill_rrect(ir.inset(0.0, 4.0 * s), 7.0 * s, hl);
            } else if it.hit.is_some() && it.hit == self.hover && p.hit.is_none() {
                self.canvas.fill_rrect(ir.inset(-3.0 * s, 4.0 * s), 6.0 * s, ui::with_alpha(self.cfg.foreground, 0x22));
            }
            let col = Rgba { a: (it.color.a as f32 * it.opacity) as u8, ..it.color };
            match &it.kind {
                Kind::Icon(g) => {
                    let fs = self.is();
                    self.canvas.text(&g.to_string(), ir, ui::icon_font(), fs, false, Align::Center, col);
                }
                Kind::Text(t, bold) => {
                    let (font, fs) = (self.font.to_string(), self.fs());
                    let align = if it.highlight.is_some() { Align::Center } else { Align::Left };
                    self.canvas.text(t, ir, &font, fs, *bold, align, col);
                }
                Kind::App(h, letter) => {
                    let size = self.is() + 2.0 * s;
                    let ix = ir.x + (ir.w - size) / 2.0;
                    let iy = r.y + (r.h - size) / 2.0 - 1.0 * s;
                    match self.icons.get(h).and_then(|b| b.as_ref()) {
                        Some(b) => self.canvas.draw_bitmap(b, RectF::new(ix, iy, size, size), it.opacity),
                        None => {
                            self.canvas.fill_circle(ix + size / 2.0, iy + size / 2.0, size / 2.0, ui::with_alpha(self.cfg.accent, 0x60));
                            let font = self.font.to_string();
                            self.canvas.text(&letter.to_string(), RectF::new(ix, iy, size, size), &font, self.fs() * 0.85, true, Align::Center, col);
                        }
                    }
                    if it.underline {
                        let uy = r.bottom() - 4.0 * s;
                        self.canvas.line(ir.x + 3.0 * s, uy, ir.right() - 3.0 * s, uy, 2.0 * s, self.cfg.accent);
                    }
                }
            }
            if let Some(h) = &it.hit {
                hits.push((ir, h.clone()));
            }
            cx += iw + gap;
        }
    }
}

/// Best icon for a window: WM_GETICON, then the class icon, then the exe's icon.
fn app_icon(h: isize) -> Option<image::Pixels> {
    unsafe {
        let mut result = 0usize;
        for kind in [2usize /* ICON_SMALL2 */, 0 /* ICON_SMALL */, 1 /* ICON_BIG */] {
            let ok = SendMessageTimeoutW(
                hwnd(h),
                WM_GETICON,
                WPARAM(kind),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                40,
                Some(&mut result),
            );
            if ok.0 != 0 && result != 0 {
                return image::icon(HICON(result as *mut _));
            }
        }
        for idx in [GCLP_HICONSM, GCLP_HICON] {
            let v = GetClassLongPtrW(hwnd(h), idx);
            if v != 0 {
                return image::icon(HICON(v as *mut _));
            }
        }
        let path = win::process_path(win::app_pid(h))?;
        let wpath = win::wide(&path);
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            windows::core::PCWSTR(wpath.as_ptr()),
            Default::default(),
            Some(&mut sfi),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        );
        if r == 0 || sfi.hIcon.is_invalid() {
            return None;
        }
        let px = image::icon(sfi.hIcon);
        let _ = DestroyIcon(sfi.hIcon);
        px
    }
}

// ────────────────────────────── flyouts ───────────────────────────────

enum Row {
    Header(char, String),
    Big(String, Rgba),
    Kv(String, String),
    Spark(Vec<f32>, Rgba),
    Meter(f32, Rgba, Option<Hit>),
    Buttons(Vec<(char, String, Hit)>),
    Calendar,
    Note(String),
}

impl Row {
    fn height(&self, s: f32) -> f32 {
        s * match self {
            Row::Header(..) => 32.0,
            Row::Big(..) => 44.0,
            Row::Kv(..) => 24.0,
            Row::Spark(..) => 54.0,
            Row::Meter(..) => 24.0,
            Row::Buttons(b) => 38.0 * b.len().div_ceil(2) as f32,
            Row::Calendar => 26.0 * 8.0,
            Row::Note(..) => 22.0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(&self, c: &mut Canvas, cfg: &BarConfig, font: &str, r: RectF, s: f32, hover: Option<&Hit>, hits: &mut Vec<(RectF, Hit)>) {
        let fs = cfg.font_size * s;
        match self {
            Row::Header(g, t) => {
                c.text(&g.to_string(), RectF::new(r.x, r.y, 22.0 * s, r.h), ui::icon_font(), fs * 1.3, false, Align::Left, cfg.accent);
                c.text(t, RectF::new(r.x + 28.0 * s, r.y, r.w - 28.0 * s, r.h), font, fs * 1.15, true, Align::Left, cfg.foreground);
            }
            Row::Big(t, col) => c.text(t, r, font, fs * 2.2, true, Align::Left, *col),
            Row::Kv(k, v) => {
                c.text(k, r, font, fs, false, Align::Left, ui::with_alpha(cfg.foreground, 0xb0));
                c.text(v, r, font, fs, true, Align::Right, cfg.foreground);
            }
            Row::Spark(values, col) => {
                let g = r.inset(0.0, 5.0 * s);
                c.fill_rrect(g, 6.0 * s, ui::with_alpha(cfg.foreground, 0x12));
                let n = sysinfo::HISTORY as f32;
                let pts: Vec<(f32, f32)> = values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let x = g.x + g.w * (i as f32 + (n - values.len() as f32)) / (n - 1.0);
                        (x, g.bottom() - 3.0 * s - (g.h - 6.0 * s) * v.clamp(0.0, 100.0) / 100.0)
                    })
                    .collect();
                for w2 in pts.windows(2) {
                    c.line(w2[0].0, w2[0].1, w2[1].0, w2[1].1, 1.6 * s, *col);
                }
            }
            Row::Meter(pct, col, hit) => {
                let track = RectF::new(r.x, r.y + r.h / 2.0 - 4.0 * s, r.w, 8.0 * s);
                c.fill_rrect(track, 4.0 * s, ui::with_alpha(cfg.foreground, 0x22));
                let fill = RectF::new(track.x, track.y, track.w * pct.clamp(0.0, 100.0) / 100.0, track.h);
                c.fill_rrect(fill, 4.0 * s, *col);
                if let Some(h) = hit {
                    hits.push((RectF::new(r.x, r.y, r.w, r.h), h.clone()));
                }
            }
            Row::Buttons(buttons) => {
                let bw = (r.w - 8.0 * s) / 2.0;
                for (i, (g, label, hit)) in buttons.iter().enumerate() {
                    let br = RectF::new(r.x + (i % 2) as f32 * (bw + 8.0 * s), r.y + (i / 2) as f32 * 38.0 * s + 3.0 * s, bw, 32.0 * s);
                    let bg = if hover == Some(hit) { ui::with_alpha(cfg.accent, 0x50) } else { ui::with_alpha(cfg.foreground, 0x18) };
                    c.fill_rrect(br, 8.0 * s, bg);
                    c.text(&g.to_string(), RectF::new(br.x + 10.0 * s, br.y, 20.0 * s, br.h), ui::icon_font(), fs, false, Align::Left, cfg.accent);
                    c.text(label, RectF::new(br.x + 34.0 * s, br.y, br.w - 40.0 * s, br.h), font, fs * 0.95, false, Align::Left, cfg.foreground);
                    hits.push((br, hit.clone()));
                }
            }
            Row::Calendar => {
                let t = win::local_time();
                let cw = r.w / 7.0;
                let ch = 26.0 * s;
                for (i, d) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
                    c.text(d, RectF::new(r.x + i as f32 * cw, r.y, cw, ch), font, fs * 0.9, true, Align::Center, cfg.dim);
                }
                let first = clock::first_weekday(&t) as usize;
                for day in 1..=clock::days_in_month(t.wYear, t.wMonth) as usize {
                    let cell = first + day - 1;
                    let cr = RectF::new(r.x + (cell % 7) as f32 * cw, r.y + ch * (1 + cell / 7) as f32, cw, ch);
                    let today = day == t.wDay as usize;
                    if today {
                        c.fill_circle(cr.x + cw / 2.0, cr.y + ch / 2.0, ch * 0.45, cfg.accent);
                    }
                    let col = if today { Rgba { a: 0xff, ..cfg.background } } else { cfg.foreground };
                    c.text(&day.to_string(), cr, font, fs * 0.95, today, Align::Center, col);
                }
            }
            Row::Note(t) => c.text(t, r, font, fs * 0.9, false, Align::Left, ui::with_alpha(cfg.foreground, 0x90)),
        }
    }
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else {
        format!("{h}h {m}m")
    }
}

fn popup_rows(m: Module, c: &BarConfig, st: &Stats) -> Vec<Row> {
    let settings = |label: &str, uri: &str| (glyph::SETTINGS, label.to_string(), Hit::Exec(uri.to_string()));
    match m {
        Module::Clock => {
            let t = win::local_time();
            vec![
                Row::Big(clock::format(&t, "%H:%M:%S"), c.foreground),
                Row::Kv(clock::format(&t, "%A"), clock::format(&t, "%e %B %Y")),
                Row::Header(glyph::CALENDAR, clock::month_name(&t).to_string()),
                Row::Calendar,
                Row::Buttons(vec![settings("Date & time", "ms-settings:dateandtime")]),
            ]
        }
        Module::Cpu => vec![
            Row::Header(glyph::CPU, "Processor".into()),
            Row::Big(format!("{:.0}%", st.cpu), level(c, st.cpu, 70.0, 90.0)),
            Row::Spark(st.cpu_hist.iter().copied().collect(), c.accent),
            Row::Kv("Logical processors".into(), st.cores.to_string()),
            Row::Kv("Uptime".into(), duration(st.uptime_secs)),
            Row::Buttons(vec![(glyph::CPU, "Task Manager".into(), Hit::Exec("taskmgr.exe".into()))]),
        ],
        Module::Memory => vec![
            Row::Header(glyph::RAM, "Memory".into()),
            Row::Big(format!("{:.0}%", st.mem_pct()), level(c, st.mem_pct(), 80.0, 92.0)),
            Row::Spark(st.mem_hist.iter().copied().collect(), c.accent),
            Row::Kv("In use".into(), gb(st.mem_used)),
            Row::Kv("Available".into(), gb(st.mem_total - st.mem_used)),
            Row::Kv("Total".into(), gb(st.mem_total)),
            Row::Buttons(vec![(glyph::RAM, "Task Manager".into(), Hit::Exec("taskmgr.exe".into()))]),
        ],
        Module::Temperature => {
            let mut rows = vec![Row::Header(glyph::TEMP, "Temperature".into())];
            if st.temps.is_empty() {
                rows.push(Row::Note("No ACPI thermal zones reported.".into()));
            }
            for (name, t) in &st.temps {
                rows.push(Row::Kv(name.clone(), format!("{t:.1} °C")));
            }
            rows.push(Row::Note("ACPI thermal zones (Windows performance counters)".into()));
            rows
        }
        Module::Volume => {
            let (v, muted) = st.volume.unwrap_or((0.0, false));
            vec![
                Row::Header(if muted { glyph::MUTE } else { glyph::VOLUME }, "Volume".into()),
                Row::Big(if muted { "Muted".into() } else { format!("{v:.0}%") }, c.foreground),
                Row::Meter(v, if muted { c.dim } else { c.accent }, Some(Hit::VolumeSlider(0.0, 0.0))),
                Row::Note("Click the bar to set · scroll on the module to adjust".into()),
                Row::Buttons(vec![
                    (if muted { glyph::VOLUME } else { glyph::MUTE }, if muted { "Unmute" } else { "Mute" }.into(), Hit::Mute),
                    settings("Sound", "ms-settings:sound"),
                ]),
            ]
        }
        Module::Wifi => {
            let mut rows = vec![Row::Header(glyph::WIFI, "Wi-Fi".into())];
            match &st.wifi {
                Some(w) if w.connected => {
                    rows.push(Row::Big(w.ssid.clone().unwrap_or_else(|| "Connected".into()), c.foreground));
                    rows.push(Row::Kv("Signal".into(), format!("{}%", w.signal)));
                    rows.push(Row::Meter(w.signal as f32, c.accent, None));
                    rows.push(Row::Kv("Adapter".into(), w.adapter.clone()));
                    if w.ssid.is_none() {
                        rows.push(Row::Note("Network name hidden: allow location access for desktop apps".into()));
                    }
                }
                Some(w) => {
                    rows.push(Row::Big("Not connected".into(), c.dim));
                    rows.push(Row::Kv("Adapter".into(), w.adapter.clone()));
                }
                None => rows.push(Row::Note("No wireless adapter".into())),
            }
            rows.push(Row::Buttons(vec![
                (glyph::WIFI, "Networks".into(), Hit::Exec("ms-availablenetworks:".into())),
                settings("Wi-Fi settings", "ms-settings:network-wifi"),
            ]));
            rows
        }
        Module::Bluetooth => {
            let bt = &st.bluetooth;
            let mut rows = vec![Row::Header(glyph::BLUETOOTH, "Bluetooth".into())];
            rows.push(Row::Big(if bt.on { "On".into() } else { "Off".into() }, if bt.on { c.accent } else { c.dim }));
            if !bt.radio.is_empty() {
                rows.push(Row::Kv("Radio".into(), bt.radio.clone()));
            }
            if bt.connected.is_empty() {
                rows.push(Row::Note("No devices connected".into()));
            }
            for d in &bt.connected {
                rows.push(Row::Kv(d.clone(), "connected".into()));
            }
            rows.push(Row::Buttons(vec![settings("Bluetooth settings", "ms-settings:bluetooth")]));
            rows
        }
        Module::Battery => {
            let mut rows = vec![];
            match &st.battery {
                Some(b) => {
                    rows.push(Row::Header(glyph::battery(b.pct, b.charging), "Battery".into()));
                    rows.push(Row::Big(format!("{}%", b.pct), c.foreground));
                    rows.push(Row::Meter(b.pct as f32, level(c, 100.0 - b.pct as f32, 80.0, 90.0), None));
                    let state = if b.charging {
                        "Charging"
                    } else if b.on_ac {
                        "Plugged in"
                    } else {
                        "On battery"
                    };
                    rows.push(Row::Kv("Status".into(), state.into()));
                    if let Some(s) = b.secs_left {
                        rows.push(Row::Kv("Time remaining".into(), duration(s as u64)));
                    }
                }
                None => rows.push(Row::Note("No battery".into())),
            }
            rows.push(Row::Buttons(vec![settings("Power settings", "ms-settings:powersleep")]));
            rows
        }
        Module::Power => vec![
            Row::Header(glyph::POWER, "Power".into()),
            Row::Buttons(vec![
                (glyph::LOCK, "Lock".into(), Hit::Lock),
                (glyph::SLEEP, "Sleep".into(), Hit::Sleep),
                (glyph::RESTART, "Restart".into(), Hit::Exec("shutdown.exe /r /t 0".into())),
                (glyph::POWER, "Shut down".into(), Hit::Exec("shutdown.exe /s /t 0".into())),
                (glyph::HOME, "Sign out".into(), Hit::Exec("shutdown.exe /l".into())),
                settings("Settings", "ms-settings:"),
            ]),
        ],
    }
}

// ─────────────────────────── window procedures ────────────────────────

fn lparam_point(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn ctl_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_TICK => {
            with_bar(|t| {
                t.tick += 1;
                if t.tick % 2 == 0 {
                    t.sampler.sample();
                }
                t.redraw_all();
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_REBUILD => {
            let _ = KillTimer(h, TIMER_REBUILD);
            with_bar(|t| t.rebuild());
            LRESULT(0)
        }
        WM_BAR_SNAPSHOT => {
            with_bar(|t| {
                let snap = t.shared.lock().unwrap().snapshot.clone();
                if snap != t.snap {
                    t.snap = snap;
                    t.redraw_all();
                }
            });
            LRESULT(0)
        }
        WM_BAR_CONFIG => {
            with_bar(|t| {
                let cfg = t.shared.lock().unwrap().config.clone();
                let structural = cfg.height != t.cfg.height || cfg.all_monitors != t.cfg.all_monitors;
                t.cfg = cfg;
                if structural {
                    t.rebuild();
                } else {
                    t.redraw_all();
                }
            });
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            SetTimer(h, TIMER_REBUILD, 500, None);
            LRESULT(0)
        }
        WM_CLOSE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

unsafe extern "system" fn bar_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let hw = raw(h);
    match msg {
        WM_LBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = lparam_point(lp);
            with_bar(|t| {
                if let Some(i) = t.bar_index(hw) {
                    t.on_click(i, x, y, msg);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            let (sx, sy) = lparam_point(lp);
            with_bar(|t| {
                if let Some(i) = t.bar_index(hw) {
                    let r = t.bars[i].rect;
                    t.on_wheel(i, sx - r.left as f32, sy - r.top as f32, delta);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_point(lp);
            with_bar(|t| {
                if let Some(i) = t.bar_index(hw) {
                    let bar = &mut t.bars[i];
                    if !bar.tracking {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: h,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        bar.tracking = true;
                    }
                    let hover = bar.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
                    if hover != bar.hover {
                        bar.hover = hover;
                        t.draw_bar(i);
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_bar(|t| {
                if let Some(i) = t.bar_index(hw) {
                    t.bars[i].tracking = false;
                    if t.bars[i].hover.take().is_some() {
                        t.draw_bar(i);
                    }
                }
            });
            LRESULT(0)
        }
        WM_BAR_APPBAR => {
            with_bar(|t| {
                let Some(i) = t.bar_index(hw) else { return };
                match wp.0 {
                    ABN_POSCHANGED => {
                        let height = t.bars[i].rect.bottom - t.bars[i].rect.top;
                        let m = t.bars[i].monitor.clone();
                        t.bars[i].rect = reserve(hw, &m, height);
                        t.draw_bar(i);
                    }
                    ABN_FULLSCREENAPP => {
                        // Only hide on the monitor that actually has the fullscreen app.
                        let fg = win::foreground();
                        let on_this = win::monitor_of_window(fg) == t.bars[i].monitor.handle;
                        let hide = lp.0 != 0 && on_this;
                        if hide != t.bars[i].fullscreen_hidden {
                            t.bars[i].fullscreen_hidden = hide;
                            if hide {
                                t.bars[i].canvas.hide();
                            } else {
                                t.draw_bar(i);
                            }
                        }
                    }
                    _ => {}
                }
            });
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

unsafe extern "system" fn popup_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            // Clicking anywhere else closes the flyout.
            let _ = PostMessageW(h, WM_APP + 30, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        m if m == WM_APP + 30 => {
            with_bar(|t| {
                if t.popup.as_ref().is_some_and(|p| p.canvas.hwnd == raw(h)) {
                    t.close_popup();
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            with_bar(|t| t.close_popup());
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = lparam_point(lp);
            with_bar(|t| {
                let Some(p) = t.popup.as_ref() else { return };
                let Some((r, hit)) = p.hits.iter().rev().find(|(r, _)| r.contains(x, y)).cloned() else { return };
                let hit = match hit {
                    Hit::VolumeSlider(..) => Hit::VolumeSlider(r.x, r.w),
                    o => o,
                };
                t.run_action(hit, x);
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_point(lp);
            with_bar(|t| {
                let Some(p) = t.popup.as_mut() else { return };
                let hover = p.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
                if hover != p.hover {
                    p.hover = hover;
                    t.draw_popup();
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
