//! Built-in app launcher (PowerToys Run / fuzzel style), styled from
//! `launcher.ini`.
//!
//! Type to fuzzy-search every installed app (Start's "All apps": desktop and
//! Store apps). `=` or plain arithmetic evaluates (Enter copies the result),
//! `>` runs a command, `?` searches the web, and paths open directly.
//! Enter launches, Ctrl+Enter launches as administrator, Esc or clicking
//! elsewhere closes. Frequently used apps rank first.

pub mod calc;
pub mod fuzzy;
pub mod index;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc;

use windows::core::w;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::types::Rgba;
use crate::config::Launcher;
use crate::ui::{self, glyph, Align, Canvas, RectF};
use crate::win::{self, hwnd, wide};

const WM_INDEX_READY: u32 = WM_APP + 60;
const WM_ICON_READY: u32 = WM_APP + 61;
const WM_LAUNCHER_CLOSE: u32 = WM_APP + 62;
const MAX_RESULTS: usize = 60;

static OPEN: AtomicIsize = AtomicIsize::new(0);

/// Colours and sizes from launcher.ini (fuzzel-compatible keys).
#[derive(Debug, Clone)]
pub struct Look {
    bg: Rgba,
    text: Rgba,
    prompt_color: Rgba,
    input: Rgba,
    matched: Rgba,
    selection: Rgba,
    selection_text: Rgba,
    selection_match: Rgba,
    border: Rgba,
    border_width: f32,
    radius: f32,
    font: String,
    prompt: String,
    lines: usize,
    width: f32,
}

fn hex(s: Option<&str>, fallback: Rgba) -> Rgba {
    let Some(s) = s.map(|s| s.trim().trim_start_matches('#')) else { return fallback };
    let byte = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
    match s.len() {
        8 => (|| Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: byte(6)? }))().unwrap_or(fallback),
        6 => (|| Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: 255 }))().unwrap_or(fallback),
        _ => fallback,
    }
}

impl Look {
    pub fn from(l: &Launcher) -> Look {
        let c = |k: &str, d: u32| {
            let d = Rgba { r: (d >> 24) as u8, g: (d >> 16) as u8, b: (d >> 8) as u8, a: d as u8 };
            hex(l.get("colors", k), d)
        };
        Look {
            bg: c("background", 0x2e3440f5),
            text: c("text", 0xe5e9f0ff),
            prompt_color: c("prompt", 0x88c0d0ff),
            input: c("input", 0xe5e9f0ff),
            matched: c("match", 0xebcb8bff),
            selection: c("selection", 0x3b4252ff),
            selection_text: c("selection-text", 0xe5e9f0ff),
            selection_match: c("selection-match", 0xebcb8bff),
            border: c("border", 0x88c0d0ff),
            border_width: l.get("border", "width").and_then(|v| v.parse().ok()).unwrap_or(1.0),
            radius: l.get("border", "radius").and_then(|v| v.parse().ok()).unwrap_or(10.0),
            font: l.get("main", "font").unwrap_or("").to_string(),
            prompt: l.get("main", "prompt").unwrap_or("> ").to_string(),
            lines: l.get("main", "lines").and_then(|v| v.parse().ok()).unwrap_or(8usize).clamp(3, 20),
            width: l.get("main", "width").and_then(|v| v.parse().ok()).unwrap_or(680.0f32).clamp(360.0, 1400.0),
        }
    }
}

/// Opens the launcher, or closes it if open.
pub fn toggle(look: Look) {
    let open = OPEN.load(Ordering::SeqCst);
    if open != 0 {
        unsafe {
            let _ = PostMessageW(hwnd(open), WM_LAUNCHER_CLOSE, WPARAM(0), LPARAM(0));
        }
        return;
    }
    let _ = std::thread::Builder::new().name("deadbeef-launcher".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        run(look);
    });
}

#[derive(Debug, Clone)]
enum Kind {
    App(String),
    Calc(String),
    Command(String),
    Path(String),
    Web(String),
}

#[derive(Debug, Clone)]
struct Item {
    title: String,
    subtitle: String,
    kind: Kind,
    matches: Vec<usize>,
    glyph: char,
}

struct State {
    canvas: Canvas,
    look: Look,
    font: String,
    query: String,
    apps: Vec<index::App>,
    history: index::History,
    items: Vec<Item>,
    selected: usize,
    top: usize,
    hover: Option<usize>,
    rows: Vec<(RectF, usize)>,
    bitmaps: HashMap<String, Option<ID2D1Bitmap>>,
    requested: HashSet<String>,
    icon_tx: mpsc::Sender<String>,
    s: f32,
    x: i32,
    y: i32,
    w: f32,
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    ST.with(|s| {
        let mut g = s.try_borrow_mut().ok()?;
        Some(f(g.as_mut()?))
    })
}

fn run(look: Look) {
    ui::register_class(w!("deadbeef_launcher"), Some(proc_));
    let canvas = match Canvas::new(w!("deadbeef_launcher"), "deadbeef launcher", WS_EX_TOOLWINDOW | WS_EX_TOPMOST, true) {
        Ok(c) => c,
        Err(e) => {
            crate::log_error!("launcher: {e}");
            return;
        }
    };
    let me = canvas.hwnd;
    if OPEN.compare_exchange(0, me, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    let (cx, cy) = win::cursor_pos();
    let monitors = win::monitors();
    let Some(m) = monitors.iter().find(|m| m.rect.contains(cx, cy)).or(monitors.first()).cloned() else {
        OPEN.store(0, Ordering::SeqCst);
        return;
    };
    let s = m.scale;
    let w = (look.width * s).min(m.work.w as f32 * 0.9);
    let font = if !look.font.is_empty() && canvas.has_font(&look.font) { look.font.clone() } else { ui::text_font().to_string() };

    // Icons load on a worker so the list never waits for the shell.
    let (icon_tx, icon_rx) = mpsc::channel::<String>();
    let icon_px = (32.0 * s) as i32;
    let _ = std::thread::Builder::new().name("deadbeef-launcher-icons".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        while let Ok(target) = icon_rx.recv() {
            let _ = index::icon(&target, icon_px);
            unsafe {
                let _ = PostMessageW(hwnd(me), WM_ICON_READY, WPARAM(0), LPARAM(0));
            }
        }
    });

    // Show the cached index immediately, refresh it in the background.
    let apps = index::cached().unwrap_or_default();
    let _ = std::thread::Builder::new().name("deadbeef-launcher-index".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        index::store(index::enumerate());
        unsafe {
            let _ = PostMessageW(hwnd(me), WM_INDEX_READY, WPARAM(0), LPARAM(0));
        }
    });

    let st = State {
        canvas,
        look,
        font,
        query: String::new(),
        apps,
        history: index::load_history(),
        items: Vec::new(),
        selected: 0,
        top: 0,
        hover: None,
        rows: Vec::new(),
        bitmaps: HashMap::new(),
        requested: HashSet::new(),
        icon_tx,
        s,
        x: m.work.x + ((m.work.w as f32 - w) / 2.0) as i32,
        y: m.work.y + (m.work.h as f32 * 0.2) as i32,
        w,
    };
    ST.with(|c| *c.borrow_mut() = Some(st));
    with(|st| {
        st.refilter();
        st.draw();
    });
    win::focus(me);

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    ST.with(|c| c.borrow_mut().take());
    OPEN.store(0, Ordering::SeqCst);
}

impl State {
    fn refilter(&mut self) {
        let q = self.query.trim().to_string();
        let mut items = Vec::new();

        if let Some(cmd) = q.strip_prefix('>') {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                items.push(Item { title: cmd.to_string(), subtitle: "Run command".into(), kind: Kind::Command(cmd.into()), matches: vec![], glyph: '\u{E756}' });
            }
        } else if let Some(web) = q.strip_prefix('?') {
            let web = web.trim();
            if !web.is_empty() {
                items.push(Item { title: web.to_string(), subtitle: "Search the web".into(), kind: Kind::Web(web.into()), matches: vec![], glyph: glyph::SEARCH });
            }
        } else {
            if let Some(v) = calc::eval(&q) {
                let r = calc::format(v);
                items.push(Item { title: format!("= {r}"), subtitle: "Enter copies the result".into(), kind: Kind::Calc(r), matches: vec![], glyph: '\u{E8EF}' });
            }
            let expanded = crate::config::parser::expand_env(&q);
            let looks_path = expanded.len() > 2 && (expanded.as_bytes().get(1) == Some(&b':') || expanded.starts_with("\\\\"));
            if looks_path && std::path::Path::new(&expanded).exists() {
                items.push(Item { title: expanded.clone(), subtitle: "Open".into(), kind: Kind::Path(expanded), matches: vec![], glyph: glyph::FOLDER });
            }
            let mut scored: Vec<(i32, Item)> = self
                .apps
                .iter()
                .filter_map(|a| {
                    let (sc, m) = fuzzy::score(&q, &a.name)?;
                    let bonus = index::bonus(&self.history, &a.target);
                    Some((
                        sc + bonus,
                        Item {
                            title: a.name.clone(),
                            subtitle: if a.target.contains('!') { "Store app".into() } else { "Application".into() },
                            kind: Kind::App(a.target.clone()),
                            matches: m,
                            glyph: '\u{E737}',
                        },
                    ))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.to_lowercase().cmp(&b.1.title.to_lowercase())));
            items.extend(scored.into_iter().take(MAX_RESULTS).map(|(_, i)| i));
            if !q.is_empty() && items.is_empty() {
                items.push(Item { title: q.clone(), subtitle: "No apps found — search the web".into(), kind: Kind::Web(q.clone()), matches: vec![], glyph: glyph::SEARCH });
            }
        }
        self.items = items;
        self.selected = 0;
        self.top = 0;
    }

    fn row_h(&self) -> f32 {
        50.0 * self.s
    }

    fn ensure_visible(&mut self) {
        let lines = self.look.lines;
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + lines {
            self.top = self.selected + 1 - lines;
        }
    }

    fn draw(&mut self) {
        let (s, w) = (self.s, self.w);
        let l = self.look.clone();
        let font = self.font.clone();
        let fs = 14.0 * s;
        let header = 60.0 * s;
        let pad = 10.0 * s;
        let visible = self.items.len().saturating_sub(self.top).min(l.lines);
        let h = header + pad + l.lines as f32 * self.row_h() + 34.0 * s;

        // Request icons for the rows about to be shown.
        for it in self.items.iter().skip(self.top).take(l.lines) {
            if let Kind::App(t) = &it.kind {
                if !self.bitmaps.contains_key(t) {
                    match index::icon_cached(t) {
                        Some(px) => {
                            let bmp = px.and_then(|p| self.canvas.bitmap(p.w, p.h, &p.data));
                            self.bitmaps.insert(t.clone(), bmp);
                        }
                        None => {
                            if self.requested.insert(t.clone()) {
                                let _ = self.icon_tx.send(t.clone());
                            }
                        }
                    }
                }
            }
        }

        if self.canvas.begin(w as i32, h as i32).is_err() {
            return;
        }
        let mut rows = Vec::new();
        let c = &mut self.canvas;
        let full = RectF::new(0.0, 0.0, w, h);
        c.fill_rrect(full, l.radius * s, l.bg);
        if l.border_width > 0.0 {
            c.stroke_rrect(full, l.radius * s, l.border_width * s, l.border);
        }

        // Prompt + query.
        let pw = c.measure(&l.prompt, &font, fs * 1.25, true);
        c.text(&l.prompt, RectF::new(20.0 * s, 0.0, pw + 4.0, header), &font, fs * 1.25, true, Align::Left, l.prompt_color);
        let qx = 20.0 * s + pw + 4.0 * s;
        if self.query.is_empty() {
            c.text("Search apps · = calculate · > run · ? web", RectF::new(qx, 0.0, w - qx - 20.0 * s, header), &font, fs * 1.15, false, Align::Left, ui::with_alpha(l.text, 0x70));
        } else {
            c.text(&format!("{}▏", self.query), RectF::new(qx, 0.0, w - qx - 20.0 * s, header), &font, fs * 1.25, false, Align::Left, l.input);
        }
        c.line(16.0 * s, header, w - 16.0 * s, header, 1.0 * s, ui::with_alpha(l.text, 0x18));

        // Results.
        for (vi, idx) in (self.top..self.top + visible).enumerate() {
            let it = &self.items[idx];
            let r = RectF::new(pad, header + pad / 2.0 + vi as f32 * 50.0 * s, w - 2.0 * pad, 50.0 * s - 2.0 * s);
            let sel = idx == self.selected;
            if sel {
                c.fill_rrect(r, 8.0 * s, l.selection);
            } else if self.hover == Some(idx) {
                c.fill_rrect(r, 8.0 * s, ui::with_alpha(l.selection, 0x90));
            }
            let (tc, mc) = if sel { (l.selection_text, l.selection_match) } else { (l.text, l.matched) };
            let icon = RectF::new(r.x + 10.0 * s, r.y + (r.h - 32.0 * s) / 2.0, 32.0 * s, 32.0 * s);
            let bmp = match &it.kind {
                Kind::App(t) => self.bitmaps.get(t).and_then(|b| b.clone()),
                _ => None,
            };
            match bmp {
                Some(b) => c.draw_bitmap(&b, icon, 1.0),
                None => c.text(&it.glyph.to_string(), icon, ui::icon_font(), 18.0 * s, false, Align::Center, l.prompt_color),
            }
            let tx = icon.right() + 14.0 * s;
            let title_r = RectF::new(tx, r.y + 5.0 * s, r.right() - tx - 10.0 * s, 24.0 * s);
            c.text(&it.title, title_r, &font, fs, sel, Align::Left, tc);
            // Highlight matched characters by redrawing them in the match colour.
            let chars: Vec<char> = it.title.chars().collect();
            for &mi in &it.matches {
                if mi < chars.len() {
                    let prefix: String = chars[..mi].iter().collect();
                    let x0 = c.measure(&prefix, &font, fs, sel);
                    let ch = chars[mi].to_string();
                    let cw = c.measure(&ch, &font, fs, sel) + 2.0;
                    if x0 + cw < title_r.w {
                        c.text(&ch, RectF::new(title_r.x + x0, title_r.y, cw, title_r.h), &font, fs, true, Align::Left, mc);
                    }
                }
            }
            c.text(&it.subtitle, RectF::new(tx, r.y + 27.0 * s, r.right() - tx - 10.0 * s, 18.0 * s), &font, fs * 0.8, false, Align::Left, ui::with_alpha(tc, 0x90));
            rows.push((r, idx));
        }
        if self.items.is_empty() {
            let msg = if self.apps.is_empty() { "Indexing apps…" } else { "Type to search" };
            c.text(msg, RectF::new(0.0, header, w, 60.0 * s), &font, fs, false, Align::Center, ui::with_alpha(l.text, 0x80));
        }

        // Footer.
        let fy = h - 30.0 * s;
        let count = if self.items.is_empty() { String::new() } else { format!("{} of {}", self.selected + 1, self.items.len()) };
        c.text(&count, RectF::new(20.0 * s, fy, 120.0 * s, 22.0 * s), &font, fs * 0.8, false, Align::Left, ui::with_alpha(l.text, 0x80));
        c.text("Enter open · Ctrl+Enter admin · Esc close", RectF::new(140.0 * s, fy, w - 160.0 * s, 22.0 * s), &font, fs * 0.8, false, Align::Right, ui::with_alpha(l.text, 0x80));

        self.rows = rows;
        let _ = self.canvas.end(self.x, self.y, 255);
    }

    fn launch(&mut self, admin: bool) {
        let Some(it) = self.items.get(self.selected).cloned() else { return };
        let verb = if admin { "runas" } else { "open" };
        let r = match &it.kind {
            Kind::App(t) => {
                index::record(t);
                win::shell_exec_verb(&format!("shell:AppsFolder\\{t}"), "", verb)
            }
            Kind::Calc(v) => set_clipboard(self.canvas.hwnd, v),
            Kind::Command(cmd) => {
                let (f, p) = win::split_command(&crate::config::parser::expand_env(cmd));
                win::shell_exec_verb(&f, &p, verb)
            }
            Kind::Path(p) => win::shell_exec_verb(p, "", verb),
            Kind::Web(q) => win::shell_exec_verb(&format!("https://www.google.com/search?q={}", crate::wallpaper::http::encode(q)), "", "open"),
        };
        if let Err(e) = r {
            crate::log_warn!("launcher: {e}");
        }
        unsafe { PostQuitMessage(0) };
    }
}

fn set_clipboard(owner: isize, text: &str) -> Result<(), String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let data = wide(text);
    unsafe {
        OpenClipboard(hwnd(owner)).map_err(|e| e.to_string())?;
        let _ = EmptyClipboard();
        let r = (|| {
            let mem = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2).map_err(|e| e.to_string())?;
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                return Err("GlobalLock failed".to_string());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(mem.0)).map_err(|e| e.to_string())?;
            Ok(())
        })();
        let _ = CloseClipboard();
        r
    }
}

fn ctrl_down() -> bool {
    unsafe { GetKeyState(0x11) < 0 }
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LAUNCHER_CLOSE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_INDEX_READY => {
            with(|st| {
                if let Some(apps) = index::cached() {
                    let sel = st.selected;
                    st.apps = apps;
                    st.refilter();
                    st.selected = sel.min(st.items.len().saturating_sub(1));
                    st.ensure_visible();
                    st.draw();
                }
            });
            LRESULT(0)
        }
        WM_ICON_READY => {
            with(|st| st.draw());
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wp.0 as u32;
            with(|st| {
                let n = st.items.len();
                match vk {
                    0x1B => {
                        if st.query.is_empty() {
                            PostQuitMessage(0);
                        } else {
                            st.query.clear();
                            st.refilter();
                        }
                    }
                    0x0D => {
                        st.launch(ctrl_down());
                        return;
                    }
                    0x26 if n > 0 => st.selected = (st.selected + n - 1) % n,
                    0x28 if n > 0 => st.selected = (st.selected + 1) % n,
                    0x09 if n > 0 => {
                        st.selected = if GetKeyState(0x10) < 0 { (st.selected + n - 1) % n } else { (st.selected + 1) % n }
                    }
                    0x21 => st.selected = st.selected.saturating_sub(st.look.lines),
                    0x22 if n > 0 => st.selected = (st.selected + st.look.lines).min(n - 1),
                    0x08 => {
                        if ctrl_down() {
                            let t = st.query.trim_end().to_string();
                            st.query = t.rsplit_once(' ').map(|(a, _)| format!("{a} ")).unwrap_or_default();
                        } else {
                            st.query.pop();
                        }
                        st.refilter();
                    }
                    _ => return,
                }
                st.ensure_visible();
                st.draw();
            });
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wp.0 as u32).filter(|c| !c.is_control()) {
                with(|st| {
                    if st.query.chars().count() < 120 {
                        st.query.push(c);
                        st.refilter();
                        st.draw();
                    }
                });
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32);
            with(|st| {
                let hov = st.rows.iter().find(|(r, _)| r.contains(x, y)).map(|(_, i)| *i);
                if hov != st.hover {
                    st.hover = hov;
                    st.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32);
            with(|st| {
                if let Some(i) = st.rows.iter().find(|(r, _)| r.contains(x, y)).map(|(_, i)| *i) {
                    st.selected = i;
                    st.launch(ctrl_down());
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let d = ((wp.0 >> 16) & 0xFFFF) as i16;
            with(|st| {
                let n = st.items.len();
                if n > st.look.lines {
                    st.top = if d > 0 { st.top.saturating_sub(3) } else { (st.top + 3).min(n - st.look.lines) };
                    st.draw();
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_from_template() {
        let l = Launcher::parse(include_str!("../../templates/nerdy-deadbeef/launcher.ini")).unwrap();
        let look = Look::from(&l);
        assert_eq!(look.bg, Rgba { r: 0x2e, g: 0x34, b: 0x40, a: 0xff });
        assert_eq!(look.matched, Rgba { r: 0xeb, g: 0xcb, b: 0x8b, a: 0xff });
        assert_eq!(look.prompt, "> ");
        assert_eq!(look.radius, 10.0);
    }
}
