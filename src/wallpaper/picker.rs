//! Wallpaper picker window.
//!
//! Shows a page of wallhaven.cc results (or the downloaded library) as a
//! thumbnail grid. Thumbnails and downloads run on a small worker pool;
//! results come back to the UI thread as posted messages. Choosing a
//! wallpaper downloads it (with progress), closes the picker and runs the
//! animated transition.
//!
//! Keys: arrows move · Enter apply · D download only · / search ·
//! Tab switch source · PgUp/PgDn page · F5 refresh · Esc close.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::wallhaven::{self, Item};
use super::{http, transition, Theme};
use crate::config::types::{Rgba, WallpaperConfig};
use crate::ui::{self, glyph, image, Align, Canvas, RectF};
use crate::win::{self, hwnd};

const WM_PICKER_RESULT: u32 = WM_APP + 40;
const CLOSE: char = '\u{E711}';
const CHECK: char = '\u{E73E}';

/// HWND of the open picker (0 = none). One picker at a time.
pub static OPEN: AtomicIsize = AtomicIsize::new(0);

enum Msg {
    Search { gen: u64, result: Result<wallhaven::Page, String> },
    Local { gen: u64, files: Vec<PathBuf> },
    Thumb { gen: u64, idx: usize, result: Result<image::Pixels, String> },
    Progress { done: u64, total: Option<u64> },
    Downloaded { result: Result<PathBuf, String>, apply: bool },
}

type Job = Box<dyn FnOnce() + Send>;
/// (pending jobs, shutting down) + wake-up signal.
type Queue = Arc<(Mutex<(VecDeque<Job>, bool)>, Condvar)>;

struct Pool {
    q: Queue,
}

impl Pool {
    fn new(n: usize) -> Pool {
        let q: Queue = Arc::new((Mutex::new((VecDeque::new(), false)), Condvar::new()));
        for i in 0..n {
            let q = q.clone();
            let _ = std::thread::Builder::new().name(format!("deadbeef-wall-{i}")).spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                loop {
                    let job = {
                        let (lock, cv) = &*q;
                        let mut g = lock.lock().unwrap();
                        while g.0.is_empty() && !g.1 {
                            g = cv.wait(g).unwrap();
                        }
                        if g.1 {
                            return;
                        }
                        g.0.pop_front().unwrap()
                    };
                    job();
                }
            });
        }
        Pool { q }
    }
    fn submit(&self, job: Job) {
        let (lock, cv) = &*self.q;
        lock.lock().unwrap().0.push_back(job);
        cv.notify_one();
    }
    /// Drops queued (not yet started) jobs, e.g. thumbnails of an old page.
    fn clear(&self) {
        self.q.0.lock().unwrap().0.clear();
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let (lock, cv) = &*self.q;
        lock.lock().unwrap().1 = true;
        cv.notify_all();
    }
}

#[derive(Clone)]
struct Inbox {
    msgs: Arc<Mutex<Vec<Msg>>>,
    hwnd: isize,
}

impl Inbox {
    fn post(&self, m: Msg) {
        self.msgs.lock().unwrap().push(m);
        unsafe {
            let _ = PostMessageW(hwnd(self.hwnd), WM_PICKER_RESULT, WPARAM(0), LPARAM(0));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Online,
    Local,
}

#[derive(Debug, Clone, PartialEq)]
enum Target {
    Tab(Tab),
    Search,
    Cell(usize),
    Prev,
    Next,
    Apply,
    Download,
    Folder,
    Close,
}

struct Entry {
    caption: String,
    item: Option<Item>,
    path: Option<PathBuf>,
    bitmap: Option<ID2D1Bitmap>,
    failed: bool,
}

struct Picker {
    canvas: Canvas,
    cfg: WallpaperConfig,
    theme: Theme,
    curve: [f32; 4],
    dir: PathBuf,
    atleast: String,
    tab: Tab,
    query: String,
    editing: bool,
    page: u32,
    last_page: u32,
    total: u64,
    entries: Vec<Entry>,
    selected: usize,
    scroll: f32,
    status: String,
    error: bool,
    progress: Option<(u64, Option<u64>)>,
    gen: u64,
    pool: Pool,
    inbox: Inbox,
    s: f32,
    x: i32,
    y: i32,
    w: f32,
    h: f32,
    cols: usize,
    cell_w: f32,
    img_h: f32,
    hits: Vec<(RectF, Target)>,
    hover: Option<Target>,
    last_click: Option<(Instant, usize)>,
    apply_after: Option<PathBuf>,
}

thread_local! {
    static PICK: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

fn with_picker<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICK.with(|p| {
        let mut g = p.try_borrow_mut().ok()?;
        Some(f(g.as_mut()?))
    })
}

pub fn list_local(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "bmp" | "webp"))
        })
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.into_iter().map(|(_, p)| p).collect()
}

/// Runs the picker on the calling (dedicated) thread until it closes.
pub fn run(cfg: WallpaperConfig, theme: Theme, curve: [f32; 4], dir: PathBuf, query: Option<String>) {
    ui::register_class(w!("deadbeef_wallpicker"), Some(proc_));
    let canvas = match Canvas::new(w!("deadbeef_wallpicker"), "deadbeef wallpapers", WS_EX_TOOLWINDOW | WS_EX_TOPMOST, false) {
        Ok(c) => c,
        Err(e) => {
            crate::log_error!("wallpaper picker: {e}");
            return;
        }
    };
    let main_hwnd = canvas.hwnd;
    if OPEN.compare_exchange(0, main_hwnd, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }

    // Open on the monitor under the cursor.
    let (cx, cy) = win::cursor_pos();
    let monitors = win::monitors();
    let Some(mon) = monitors.iter().find(|m| m.rect.contains(cx, cy)).or(monitors.first()).cloned() else {
        OPEN.store(0, Ordering::SeqCst);
        return;
    };
    let s = mon.scale;
    let w = (1280.0 * s).min(mon.work.w as f32 * 0.9);
    let h = (820.0 * s).min(mon.work.h as f32 * 0.9);
    let pad = 20.0 * s;
    let gap = 14.0 * s;
    let cols = (((w - 2.0 * pad + gap) / (250.0 * s + gap)).floor() as usize).max(2);
    let cell_w = (w - 2.0 * pad - gap * (cols - 1) as f32) / cols as f32;
    let primary = monitors.iter().find(|m| m.primary).unwrap_or(&mon);
    let atleast = if cfg.atleast.is_empty() { format!("{}x{}", primary.rect.w, primary.rect.h) } else { cfg.atleast.clone() };

    let picker = Picker {
        canvas,
        query: query.unwrap_or_else(|| cfg.query.clone()),
        cfg,
        theme,
        curve,
        dir,
        atleast,
        tab: Tab::Online,
        editing: false,
        page: 1,
        last_page: 1,
        total: 0,
        entries: Vec::new(),
        selected: 0,
        scroll: 0.0,
        status: String::new(),
        error: false,
        progress: None,
        gen: 0,
        pool: Pool::new(6),
        inbox: Inbox { msgs: Arc::new(Mutex::new(Vec::new())), hwnd: main_hwnd },
        s,
        x: mon.work.x + ((mon.work.w as f32 - w) / 2.0) as i32,
        y: mon.work.y + ((mon.work.h as f32 - h) / 2.0) as i32,
        w,
        h,
        cols,
        cell_w,
        img_h: (cell_w * 9.0 / 16.0).round(),
        hits: Vec::new(),
        hover: None,
        last_click: None,
        apply_after: None,
    };
    PICK.with(|p| *p.borrow_mut() = Some(picker));
    with_picker(|p| {
        p.search(1);
        p.draw();
    });
    win::focus(main_hwnd);

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let picker = PICK.with(|p| p.borrow_mut().take());
    OPEN.store(0, Ordering::SeqCst);
    if let Some(p) = picker {
        let (apply, cfg, curve) = (p.apply_after.clone(), p.cfg.clone(), p.curve);
        drop(p); // window gone before the transition starts
        if let Some(path) = apply {
            if let Err(e) = transition::run(&path, &cfg, curve) {
                crate::log_error!("wallpaper: {e}");
            }
        }
    }
}

impl Picker {
    fn set_status(&mut self, s: impl Into<String>, error: bool) {
        self.status = s.into();
        self.error = error;
    }

    fn search(&mut self, page: u32) {
        self.tab = Tab::Online;
        self.gen += 1;
        self.pool.clear();
        self.entries.clear();
        self.selected = 0;
        self.scroll = 0.0;
        self.page = page.max(1);
        self.set_status(
            if self.query.is_empty() { "Loading top wallpapers…".to_string() } else { format!("Searching “{}”…", self.query) },
            false,
        );
        let (gen, cfg, query, atleast, inbox) = (self.gen, self.cfg.clone(), self.query.clone(), self.atleast.clone(), self.inbox.clone());
        self.pool.submit(Box::new(move || {
            let result = wallhaven::search(&cfg, &query, page, &atleast);
            inbox.post(Msg::Search { gen, result });
        }));
    }

    fn show_local(&mut self) {
        self.tab = Tab::Local;
        self.gen += 1;
        self.pool.clear();
        self.entries.clear();
        self.selected = 0;
        self.scroll = 0.0;
        self.set_status(format!("Downloaded wallpapers in {}", self.dir.display()), false);
        let (gen, dir, inbox) = (self.gen, self.dir.clone(), self.inbox.clone());
        self.pool.submit(Box::new(move || inbox.post(Msg::Local { gen, files: list_local(&dir) })));
    }

    fn queue_thumb(&self, idx: usize, source: Result<String, PathBuf>) {
        let (gen, inbox) = (self.gen, self.inbox.clone());
        let (tw, th) = (self.cell_w.round() as u32, self.img_h as u32);
        self.pool.submit(Box::new(move || {
            let bytes = match source {
                Ok(url) => http::get(&url, |_, _| true),
                Err(path) => std::fs::read(&path).map_err(|e| e.to_string()),
            };
            let result = bytes.and_then(|b| image::decode_fill(&b, tw, th));
            inbox.post(Msg::Thumb { gen, idx, result });
        }));
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Search { gen, result } if gen == self.gen => match result {
                Ok(page) => {
                    self.last_page = page.last;
                    self.total = page.total;
                    self.entries = page
                        .items
                        .into_iter()
                        .map(|it| Entry {
                            caption: format!("{}  ·  {:.1} MB", it.resolution, it.file_size as f64 / 1_048_576.0),
                            item: Some(it),
                            path: None,
                            bitmap: None,
                            failed: false,
                        })
                        .collect();
                    for i in 0..self.entries.len() {
                        let url = self.entries[i].item.as_ref().unwrap().thumbs.small.clone();
                        self.queue_thumb(i, Ok(url));
                    }
                    let msg = if self.entries.is_empty() {
                        "No results — press / to search for something else".to_string()
                    } else {
                        format!("{} wallpapers · page {} of {}", self.total, self.page, self.last_page)
                    };
                    self.set_status(msg, false);
                }
                Err(e) => self.set_status(format!("wallhaven: {e}"), true),
            },
            Msg::Local { gen, files } if gen == self.gen => {
                self.entries = files
                    .into_iter()
                    .map(|p| Entry {
                        caption: p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        item: None,
                        path: Some(p),
                        bitmap: None,
                        failed: false,
                    })
                    .collect();
                for i in 0..self.entries.len() {
                    let p = self.entries[i].path.clone().unwrap();
                    self.queue_thumb(i, Err(p));
                }
                if self.entries.is_empty() {
                    self.set_status("Nothing downloaded yet — Tab back to wallhaven.cc", false);
                }
            }
            Msg::Thumb { gen, idx, result } if gen == self.gen => {
                if let Some(e) = self.entries.get_mut(idx) {
                    match result {
                        Ok(px) => e.bitmap = self.canvas.bitmap(px.w, px.h, &px.data),
                        Err(_) => e.failed = true,
                    }
                }
            }
            Msg::Progress { done, total } => self.progress = Some((done, total)),
            Msg::Downloaded { result, apply } => {
                self.progress = None;
                match result {
                    Ok(path) if apply => {
                        self.apply_after = Some(path);
                        unsafe { PostQuitMessage(0) };
                    }
                    Ok(path) => self.set_status(format!("Saved {}", path.display()), false),
                    Err(e) => self.set_status(format!("Download failed: {e}"), true),
                }
            }
            _ => {} // stale generation
        }
    }

    fn local_path(&self, item: &Item) -> PathBuf {
        self.dir.join(item.file_name())
    }

    /// Downloads (if needed) and optionally applies the selected wallpaper.
    fn choose(&mut self, apply: bool) {
        if self.progress.is_some() {
            return;
        }
        let Some(e) = self.entries.get(self.selected) else { return };
        if let Some(p) = &e.path {
            if apply {
                self.apply_after = Some(p.clone());
                unsafe { PostQuitMessage(0) };
            }
            return;
        }
        let item = e.item.clone().unwrap();
        let dest = self.local_path(&item);
        if dest.exists() {
            self.handle(Msg::Downloaded { result: Ok(dest), apply });
            return;
        }
        if let Err(err) = std::fs::create_dir_all(&self.dir) {
            self.set_status(format!("Cannot create {}: {err}", self.dir.display()), true);
            return;
        }
        self.progress = Some((0, Some(item.file_size)));
        self.set_status(format!("Downloading {}…", item.file_name()), false);
        let inbox = self.inbox.clone();
        self.pool.submit(Box::new(move || {
            let mut last = 0u64;
            let bytes = http::get(&item.path, |done, total| {
                if done - last >= 128 * 1024 || Some(done) == total {
                    last = done;
                    inbox.post(Msg::Progress { done, total });
                }
                true
            });
            let result = bytes.and_then(|b| {
                // Validate before saving so a broken download never becomes a wallpaper.
                image::dimensions(&b)?;
                let part = dest.with_extension("part");
                std::fs::write(&part, &b).map_err(|e| e.to_string())?;
                std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
                Ok(dest)
            });
            inbox.post(Msg::Downloaded { result, apply });
        }));
    }

    // ───────────────────────────── layout ─────────────────────────────

    fn grid_top(&self) -> f32 {
        (56.0 + 44.0 + 12.0) * self.s
    }
    fn grid_bottom(&self) -> f32 {
        self.h - 52.0 * self.s
    }
    fn row_h(&self) -> f32 {
        self.img_h + 28.0 * self.s + 14.0 * self.s
    }
    fn cell_rect(&self, i: usize) -> RectF {
        let (pad, gap) = (20.0 * self.s, 14.0 * self.s);
        let (col, row) = (i % self.cols, i / self.cols);
        RectF::new(
            pad + col as f32 * (self.cell_w + gap),
            self.grid_top() + row as f32 * self.row_h() - self.scroll,
            self.cell_w,
            self.img_h + 28.0 * self.s,
        )
    }
    fn max_scroll(&self) -> f32 {
        let rows = self.entries.len().div_ceil(self.cols);
        (rows as f32 * self.row_h() - (self.grid_bottom() - self.grid_top())).max(0.0)
    }
    fn ensure_visible(&mut self) {
        let r = self.cell_rect(self.selected);
        if r.y < self.grid_top() {
            self.scroll -= self.grid_top() - r.y;
        } else if r.bottom() > self.grid_bottom() {
            self.scroll += r.bottom() - self.grid_bottom();
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn draw(&mut self) {
        let s = self.s;
        let t = self.theme;
        let font = ui::text_font();
        let fs = 13.0 * s;
        let (w, h) = (self.w, self.h);
        if self.canvas.begin(w as i32, h as i32).is_err() {
            return;
        }
        let mut hits: Vec<(RectF, Target)> = Vec::new();
        let hover = self.hover.clone();
        let (gt, gb) = (self.grid_top(), self.grid_bottom());
        let c = &mut self.canvas;
        let full = RectF::new(0.0, 0.0, w, h);
        c.fill_rrect(full, 14.0 * s, Rgba { a: 0xff, ..t.bg });
        c.stroke_rrect(full, 14.0 * s, 1.5 * s, ui::with_alpha(t.accent, 0x90));

        // Header: title, source tabs, close.
        c.text(&glyph::IMAGE.to_string(), RectF::new(20.0 * s, 14.0 * s, 24.0 * s, 28.0 * s), ui::icon_font(), 18.0 * s, false, Align::Left, t.accent);
        c.text("Wallpapers", RectF::new(50.0 * s, 14.0 * s, 160.0 * s, 28.0 * s), font, 17.0 * s, true, Align::Left, t.fg);
        let mut tx = 200.0 * s;
        for (tab, label) in [(Tab::Online, "wallhaven.cc"), (Tab::Local, "Downloaded")] {
            let tw = c.measure(label, font, fs, true) + 28.0 * s;
            let r = RectF::new(tx, 14.0 * s, tw, 28.0 * s);
            let active = self.tab == tab;
            let bg = if active { t.accent } else if hover == Some(Target::Tab(tab)) { ui::with_alpha(t.fg, 0x30) } else { ui::with_alpha(t.fg, 0x14) };
            c.fill_rrect(r, 14.0 * s, bg);
            c.text(label, r, font, fs, true, Align::Center, if active { Rgba { a: 0xff, ..t.bg } } else { t.fg });
            hits.push((r, Target::Tab(tab)));
            tx += tw + 8.0 * s;
        }
        let folder = RectF::new(w - 84.0 * s, 14.0 * s, 28.0 * s, 28.0 * s);
        let close = RectF::new(w - 48.0 * s, 14.0 * s, 28.0 * s, 28.0 * s);
        for (r, g, target) in [(folder, glyph::FOLDER, Target::Folder), (close, CLOSE, Target::Close)] {
            if hover.as_ref() == Some(&target) {
                c.fill_rrect(r, 8.0 * s, ui::with_alpha(t.fg, 0x26));
            }
            c.text(&g.to_string(), r, ui::icon_font(), 13.0 * s, false, Align::Center, t.fg);
            hits.push((r, target));
        }

        // Search box + paging.
        let sy = 56.0 * s;
        if self.tab == Tab::Online {
            let sb = RectF::new(20.0 * s, sy, w - 300.0 * s, 36.0 * s);
            c.fill_rrect(sb, 10.0 * s, ui::with_alpha(t.fg, if self.editing { 0x26 } else { 0x14 }));
            if self.editing {
                c.stroke_rrect(sb, 10.0 * s, 1.5 * s, t.accent);
            }
            c.text(&glyph::SEARCH.to_string(), RectF::new(sb.x + 12.0 * s, sb.y, 20.0 * s, sb.h), ui::icon_font(), fs, false, Align::Left, t.accent);
            let (txt, col) = if self.query.is_empty() && !self.editing {
                ("Search wallhaven.cc — press / (empty = toplist)".to_string(), t.dim)
            } else {
                (format!("{}{}", self.query, if self.editing { "▏" } else { "" }), t.fg)
            };
            c.text(&txt, RectF::new(sb.x + 40.0 * s, sb.y, sb.w - 50.0 * s, sb.h), font, fs, false, Align::Left, col);
            hits.push((sb, Target::Search));

            let prev = RectF::new(w - 270.0 * s, sy, 36.0 * s, 36.0 * s);
            let next = RectF::new(w - 56.0 * s, sy, 36.0 * s, 36.0 * s);
            for (r, label, target, enabled) in
                [(prev, "‹", Target::Prev, self.page > 1), (next, "›", Target::Next, self.page < self.last_page)]
            {
                let bg = if hover.as_ref() == Some(&target) && enabled { ui::with_alpha(t.accent, 0x60) } else { ui::with_alpha(t.fg, 0x14) };
                c.fill_rrect(r, 10.0 * s, bg);
                c.text(label, r, font, 20.0 * s, true, Align::Center, if enabled { t.fg } else { t.dim });
                hits.push((r, target));
            }
            let pg = format!("Page {} / {}", self.page, self.last_page);
            c.text(&pg, RectF::new(w - 230.0 * s, sy, 170.0 * s, 36.0 * s), font, fs, false, Align::Center, t.fg);
        } else {
            c.text(&self.dir.display().to_string(), RectF::new(20.0 * s, sy, w - 40.0 * s, 36.0 * s), font, fs, false, Align::Left, t.dim);
        }

        // Grid.
        c.push_clip(RectF::new(0.0, gt - 6.0 * s, w, gb - gt + 6.0 * s));
        for i in 0..self.entries.len() {
            let r = {
                let (pad, gap) = (20.0 * s, 14.0 * s);
                let (col, row) = (i % self.cols, i / self.cols);
                RectF::new(pad + col as f32 * (self.cell_w + gap), gt + row as f32 * (self.img_h + 42.0 * s) - self.scroll, self.cell_w, self.img_h + 28.0 * s)
            };
            if r.bottom() < gt || r.y > gb {
                continue;
            }
            let e = &self.entries[i];
            let img = RectF::new(r.x, r.y, r.w, self.img_h);
            c.fill_rrect(img, 8.0 * s, ui::with_alpha(t.fg, 0x10));
            match &e.bitmap {
                Some(b) => c.draw_bitmap(b, img, 1.0),
                None => {
                    let label = if e.failed { "unavailable" } else { "loading…" };
                    c.text(label, img, font, fs * 0.9, false, Align::Center, t.dim);
                }
            }
            if hover == Some(Target::Cell(i)) && i != self.selected {
                c.fill_rect(img, ui::with_alpha(t.fg, 0x22));
            }
            if i == self.selected {
                c.stroke_rrect(img.inset(-3.0 * s, -3.0 * s), 10.0 * s, 3.0 * s, t.accent);
            }
            let saved = e.item.as_ref().is_some_and(|it| self.dir.join(it.file_name()).exists());
            let cap = RectF::new(r.x + 2.0 * s, r.y + self.img_h + 4.0 * s, r.w - 24.0 * s, 22.0 * s);
            c.text(&e.caption, cap, font, fs * 0.9, i == self.selected, Align::Left, if i == self.selected { t.fg } else { ui::with_alpha(t.fg, 0xb0) });
            if saved {
                c.text(&CHECK.to_string(), RectF::new(r.right() - 20.0 * s, cap.y, 20.0 * s, cap.h), ui::icon_font(), fs * 0.9, false, Align::Right, t.accent);
            }
            hits.push((r, Target::Cell(i)));
        }
        c.pop_clip();

        // Footer: status / progress + actions.
        let fy = h - 44.0 * s;
        let apply = RectF::new(w - 140.0 * s, fy, 120.0 * s, 32.0 * s);
        let download = RectF::new(w - 270.0 * s, fy, 120.0 * s, 32.0 * s);
        for (r, g, label, target, primary) in [
            (download, glyph::DOWNLOAD, "Download", Target::Download, false),
            (apply, glyph::IMAGE, "Apply", Target::Apply, true),
        ] {
            let hov = hover.as_ref() == Some(&target);
            let bg = match (primary, hov) {
                (true, false) => t.accent,
                (true, true) => Rgba { a: 0xff, ..ui::with_alpha(t.fg, 0xff) },
                (false, true) => ui::with_alpha(t.fg, 0x30),
                (false, false) => ui::with_alpha(t.fg, 0x18),
            };
            let fg = if primary { Rgba { a: 0xff, ..t.bg } } else { t.fg };
            c.fill_rrect(r, 10.0 * s, bg);
            c.text(&g.to_string(), RectF::new(r.x + 12.0 * s, r.y, 20.0 * s, r.h), ui::icon_font(), fs, false, Align::Left, fg);
            c.text(label, RectF::new(r.x + 36.0 * s, r.y, r.w - 40.0 * s, r.h), font, fs, true, Align::Left, fg);
            hits.push((r, target));
        }
        let status_r = RectF::new(20.0 * s, fy, w - 310.0 * s, 32.0 * s);
        if let Some((done, total)) = self.progress {
            let pct = total.filter(|t| *t > 0).map(|t| done as f32 / t as f32 * 100.0);
            let track = RectF::new(status_r.x, fy + 22.0 * s, status_r.w, 6.0 * s);
            c.fill_rrect(track, 3.0 * s, ui::with_alpha(t.fg, 0x22));
            if let Some(p) = pct {
                c.fill_rrect(RectF::new(track.x, track.y, track.w * p / 100.0, track.h), 3.0 * s, t.accent);
            }
            let label = match (pct, total) {
                (Some(p), Some(tot)) => format!("Downloading… {p:.0}%  ({:.1} / {:.1} MB)", done as f64 / 1_048_576.0, tot as f64 / 1_048_576.0),
                _ => format!("Downloading… {:.1} MB", done as f64 / 1_048_576.0),
            };
            c.text(&label, RectF::new(status_r.x, fy - 4.0 * s, status_r.w, 24.0 * s), font, fs * 0.95, false, Align::Left, t.fg);
        } else {
            let hint = "Enter apply · D download · / search · Tab source · PgUp/PgDn page · Esc close";
            c.text(&self.status, RectF::new(status_r.x, fy - 6.0 * s, status_r.w, 22.0 * s), font, fs * 0.95, false, Align::Left, if self.error { t.critical } else { t.fg });
            c.text(hint, RectF::new(status_r.x, fy + 14.0 * s, status_r.w, 20.0 * s), font, fs * 0.85, false, Align::Left, t.dim);
        }

        self.hits = hits;
        let _ = self.canvas.end(self.x, self.y, 255);
    }

    // ───────────────────────────── input ──────────────────────────────

    fn activate(&mut self, target: Target) {
        match target {
            Target::Tab(Tab::Online) if self.tab != Tab::Online => self.search(1),
            Target::Tab(Tab::Local) if self.tab != Tab::Local => self.show_local(),
            Target::Search => self.editing = true,
            Target::Prev if self.tab == Tab::Online && self.page > 1 => self.search(self.page - 1),
            Target::Next if self.tab == Tab::Online && self.page < self.last_page => self.search(self.page + 1),
            Target::Apply => self.choose(true),
            Target::Download => self.choose(false),
            Target::Folder => {
                let _ = std::fs::create_dir_all(&self.dir);
                let _ = win::shell_exec(&format!("explorer.exe \"{}\"", self.dir.display()));
            }
            Target::Close => unsafe { PostQuitMessage(0) },
            Target::Cell(i) => {
                let dbl = unsafe { GetDoubleClickTime() } as u128;
                let double = self.last_click.is_some_and(|(t, j)| j == i && t.elapsed().as_millis() <= dbl);
                self.selected = i;
                self.last_click = Some((Instant::now(), i));
                if double {
                    self.choose(true);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, vk: u32) {
        if self.editing {
            match vk {
                0x0D => {
                    self.editing = false;
                    self.search(1);
                }
                0x1B => self.editing = false,
                0x08 => {
                    self.query.pop();
                }
                _ => {}
            }
            return;
        }
        let n = self.entries.len();
        match vk {
            0x1B => unsafe { PostQuitMessage(0) },
            0x25 if self.selected > 0 => self.selected -= 1,
            0x27 if self.selected + 1 < n => self.selected += 1,
            0x26 if self.selected >= self.cols => self.selected -= self.cols,
            0x28 if self.selected + self.cols < n => self.selected += self.cols,
            0x0D => self.choose(true),
            0x44 => self.choose(false), // D
            0x09 => {
                if self.tab == Tab::Online {
                    self.show_local()
                } else {
                    self.search(self.page)
                }
            }
            0x21 => self.activate(Target::Prev),
            0x22 => self.activate(Target::Next),
            0x74 => {
                if self.tab == Tab::Online {
                    self.search(self.page)
                } else {
                    self.show_local()
                }
            }
            _ => return,
        }
        self.ensure_visible();
    }

    fn char(&mut self, c: char) {
        if self.editing {
            if !c.is_control() && self.query.chars().count() < 80 {
                self.query.push(c);
            }
        } else if c == '/' {
            self.editing = true;
        }
    }
}

fn point(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let redraw = |f: &mut dyn FnMut(&mut Picker)| {
        with_picker(|p| {
            f(p);
            p.draw();
        });
    };
    match msg {
        WM_PICKER_RESULT => {
            redraw(&mut |p| {
                let msgs = std::mem::take(&mut *p.inbox.msgs.lock().unwrap());
                for m in msgs {
                    p.handle(m);
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            redraw(&mut |p| p.key(wp.0 as u32));
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wp.0 as u32) {
                redraw(&mut |p| p.char(c));
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lp);
            redraw(&mut |p| {
                let target = p.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, t)| t.clone());
                if let Some(t) = target {
                    if t != Target::Search {
                        p.editing = false;
                    }
                    p.activate(t);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            with_picker(|p| {
                let target = p.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, t)| t.clone());
                if target != p.hover {
                    p.hover = target;
                    p.draw();
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            redraw(&mut |p| p.scroll = (p.scroll - delta / 120.0 * p.row_h() * 0.6).clamp(0.0, p.max_scroll()));
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
