//! Keybinding cheat sheet (`binds` dispatcher, SUPER+F1 / SUPER+/ in the
//! template): every bind from the active profile, grouped, as keycaps.
//! Type to filter · Esc clears / closes · click "Edit" to open the config.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{Align, Canvas, RectF};
use crate::config::types::Rgba;
use crate::wallpaper::Theme;
use crate::win::{self, hwnd};

const WM_SHEET_CLOSE: u32 = WM_APP + 50;
static OPEN: AtomicIsize = AtomicIsize::new(0);

#[derive(Debug, Clone)]
pub struct Entry {
    pub combo: String,
    pub group: String,
    pub text: String,
    /// The bind could not be registered at all.
    pub broken: bool,
}

const GROUP_ORDER: &[&str] = &["Apps", "Windows", "Focus & move", "Layout", "Workspaces", "Monitors", "Extras", "deadbeef"];

/// Opens the sheet, or closes it if it is already open.
pub fn toggle(entries: Vec<Entry>, theme: Theme, conf: PathBuf) {
    let open = OPEN.load(Ordering::SeqCst);
    if open != 0 {
        unsafe {
            let _ = PostMessageW(hwnd(open), WM_SHEET_CLOSE, WPARAM(0), LPARAM(0));
        }
        return;
    }
    let _ = std::thread::Builder::new().name("deadbeef-binds".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        run(compress(entries), theme, conf);
    });
}

/// Folds `MODS + 1 … MODS + 0` workspace binds into one row each.
pub fn compress(entries: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut runs: Vec<(String, String, Vec<String>, usize)> = Vec::new(); // (mods, text template, keys, index in out)
    for e in entries {
        let split = e.combo.rsplit_once(" + ").map(|(m, k)| (m.to_string(), k.to_string()));
        let numeric = e.text.rsplit_once(' ').and_then(|(head, n)| {
            let n = n.trim_end_matches(" (stay)");
            n.parse::<u32>().ok().map(|_| head.to_string())
        });
        let stay = e.text.ends_with("(stay)");
        if let (Some((mods, key)), Some(head)) = (&split, numeric.or_else(|| {
            // "Send window to workspace 3 (stay)"
            e.text.strip_suffix(" (stay)").and_then(|t| t.rsplit_once(' ')).filter(|(_, n)| n.parse::<u32>().is_ok()).map(|(h, _)| h.to_string())
        })) {
            if key.len() == 1 && key.chars().all(|c| c.is_ascii_digit()) {
                let template = format!("{head}{}", if stay { "|stay" } else { "" });
                if let Some(r) = runs.iter_mut().find(|r| r.0 == *mods && r.1 == template) {
                    r.2.push(key.clone());
                    continue;
                }
                runs.push((mods.clone(), template, vec![key.clone()], out.len()));
                out.push(e);
                continue;
            }
        }
        out.push(e);
    }
    for (mods, template, keys, idx) in runs {
        if keys.len() < 3 {
            continue;
        }
        let (head, stay) = match template.strip_suffix("|stay") {
            Some(h) => (h.to_string(), true),
            None => (template.clone(), false),
        };
        let nums: Vec<u32> = keys.iter().map(|k| if k == "0" { 10 } else { k.parse().unwrap_or(0) }).collect();
        let (lo, hi) = (nums.iter().min().unwrap(), nums.iter().max().unwrap());
        out[idx].combo = format!("{mods} + {}…{}", keys.first().unwrap(), keys.last().unwrap());
        out[idx].text = format!("{head} {lo}–{hi}{}", if stay { " (stay)" } else { "" });
    }
    out
}

fn key_label(k: &str) -> String {
    match k.to_ascii_lowercase().as_str() {
        "super" => "Win".into(),
        "shift" => "Shift".into(),
        "ctrl" => "Ctrl".into(),
        "alt" => "Alt".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "return" | "enter" => "Enter".into(),
        "print" => "PrtSc".into(),
        "period" => ".".into(),
        "comma" => ",".into(),
        "slash" => "/".into(),
        "space" => "Space".into(),
        "escape" => "Esc".into(),
        _ if k.len() == 1 => k.to_ascii_uppercase(),
        _ => k.to_string(),
    }
}

fn keycaps(combo: &str) -> Vec<String> {
    let (mods, key) = combo.rsplit_once(" + ").unwrap_or(("", combo));
    mods.split_whitespace().map(key_label).chain(std::iter::once(key_label(key))).collect()
}

#[derive(Clone, PartialEq)]
enum Target {
    Edit,
    Close,
}

struct Sheet {
    canvas: Canvas,
    entries: Vec<Entry>,
    theme: Theme,
    conf: PathBuf,
    query: String,
    scroll: f32,
    content_h: f32,
    s: f32,
    x: i32,
    y: i32,
    w: f32,
    h: f32,
    hits: Vec<(RectF, Target)>,
    hover: Option<Target>,
}

thread_local! {
    static SHEET: RefCell<Option<Sheet>> = const { RefCell::new(None) };
}

fn with_sheet<R>(f: impl FnOnce(&mut Sheet) -> R) -> Option<R> {
    SHEET.with(|s| {
        let mut g = s.try_borrow_mut().ok()?;
        Some(f(g.as_mut()?))
    })
}

fn run(entries: Vec<Entry>, theme: Theme, conf: PathBuf) {
    super::register_class(w!("deadbeef_binds"), Some(proc_));
    let canvas = match Canvas::new(w!("deadbeef_binds"), "deadbeef keybindings", WS_EX_TOOLWINDOW | WS_EX_TOPMOST, true) {
        Ok(c) => c,
        Err(e) => {
            crate::log_error!("cheat sheet: {e}");
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
    let w = (1400.0 * s).min(m.work.w as f32 * 0.88);
    let h = (860.0 * s).min(m.work.h as f32 * 0.88);
    let sheet = Sheet {
        canvas,
        entries,
        theme,
        conf,
        query: String::new(),
        scroll: 0.0,
        content_h: 0.0,
        s,
        x: m.work.x + ((m.work.w as f32 - w) / 2.0) as i32,
        y: m.work.y + ((m.work.h as f32 - h) / 2.0) as i32,
        w,
        h,
        hits: Vec::new(),
        hover: None,
    };
    SHEET.with(|c| *c.borrow_mut() = Some(sheet));
    with_sheet(|sh| sh.draw());
    win::focus(me);

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    SHEET.with(|c| c.borrow_mut().take());
    OPEN.store(0, Ordering::SeqCst);
}

impl Sheet {
    fn visible(&self) -> Vec<(&'static str, Vec<&Entry>)> {
        let q = self.query.to_lowercase();
        let matches = |e: &&Entry| {
            q.is_empty()
                || e.text.to_lowercase().contains(&q)
                || e.combo.to_lowercase().contains(&q)
                || e.group.to_lowercase().contains(&q)
                || keycaps(&e.combo).iter().any(|k| k.to_lowercase() == q)
        };
        GROUP_ORDER
            .iter()
            .map(|g| (*g, self.entries.iter().filter(|e| e.group == *g).filter(matches).collect::<Vec<_>>()))
            .filter(|(_, v)| !v.is_empty())
            .collect()
    }

    fn draw(&mut self) {
        let (s, w, h, t) = (self.s, self.w, self.h, self.theme);
        let font = super::text_font();
        let fs = 13.0 * s;
        let groups: Vec<(&'static str, Vec<Entry>)> =
            self.visible().into_iter().map(|(g, v)| (g, v.into_iter().cloned().collect())).collect();
        let total: usize = groups.iter().map(|g| g.1.len()).sum();
        if self.canvas.begin(w as i32, h as i32).is_err() {
            return;
        }
        let mut hits = Vec::new();
        let hover = self.hover.clone();
        let c = &mut self.canvas;
        let full = RectF::new(0.0, 0.0, w, h);
        c.fill_rrect(full, 14.0 * s, Rgba { a: 0xff, ..t.bg });
        c.stroke_rrect(full, 14.0 * s, 1.5 * s, super::with_alpha(t.accent, 0x90));

        // Header.
        c.text("\u{E765}", RectF::new(22.0 * s, 16.0 * s, 26.0 * s, 30.0 * s), super::icon_font(), 18.0 * s, false, Align::Left, t.accent);
        c.text("Keybindings", RectF::new(54.0 * s, 16.0 * s, 200.0 * s, 30.0 * s), font, 18.0 * s, true, Align::Left, t.fg);
        let sb = RectF::new(240.0 * s, 16.0 * s, w - 470.0 * s, 32.0 * s);
        c.fill_rrect(sb, 10.0 * s, super::with_alpha(t.fg, if self.query.is_empty() { 0x14 } else { 0x26 }));
        c.text(&super::glyph::SEARCH.to_string(), RectF::new(sb.x + 12.0 * s, sb.y, 20.0 * s, sb.h), super::icon_font(), fs, false, Align::Left, t.accent);
        let (q, qc) = if self.query.is_empty() {
            ("Type to filter — e.g. workspace, focus, Shift".to_string(), t.dim)
        } else {
            (format!("{}▏", self.query), t.fg)
        };
        c.text(&q, RectF::new(sb.x + 40.0 * s, sb.y, sb.w - 140.0 * s, sb.h), font, fs, false, Align::Left, qc);
        c.text(&format!("{total} binds"), RectF::new(sb.right() - 110.0 * s, sb.y, 100.0 * s, sb.h), font, fs * 0.9, false, Align::Right, t.dim);

        let edit = RectF::new(w - 216.0 * s, 16.0 * s, 150.0 * s, 32.0 * s);
        let close = RectF::new(w - 54.0 * s, 18.0 * s, 28.0 * s, 28.0 * s);
        c.fill_rrect(edit, 10.0 * s, if hover == Some(Target::Edit) { t.fg } else { t.accent });
        c.text(&super::glyph::SETTINGS.to_string(), RectF::new(edit.x + 12.0 * s, edit.y, 20.0 * s, edit.h), super::icon_font(), fs, false, Align::Left, Rgba { a: 0xff, ..t.bg });
        c.text("Edit binds", RectF::new(edit.x + 38.0 * s, edit.y, edit.w - 40.0 * s, edit.h), font, fs, true, Align::Left, Rgba { a: 0xff, ..t.bg });
        hits.push((edit, Target::Edit));
        if hover == Some(Target::Close) {
            c.fill_rrect(close, 8.0 * s, super::with_alpha(t.fg, 0x26));
        }
        c.text("\u{E711}", close, super::icon_font(), 12.0 * s, false, Align::Center, t.fg);
        hits.push((close, Target::Close));

        // Masonry columns of group cards.
        let (pad, gap) = (22.0 * s, 16.0 * s);
        let top = 64.0 * s;
        let bottom = h - 40.0 * s;
        let cols = (((w - 2.0 * pad + gap) / (380.0 * s + gap)).floor() as usize).max(1);
        let col_w = (w - 2.0 * pad - gap * (cols - 1) as f32) / cols as f32;
        let mut col_y = vec![top; cols];
        let row_h = 30.0 * s;
        c.push_clip(RectF::new(0.0, top - 4.0 * s, w, bottom - top + 4.0 * s));
        for (g, items) in &groups {
            let ci = (0..cols).min_by(|a, b| col_y[*a].total_cmp(&col_y[*b])).unwrap();
            let card_h = 38.0 * s + items.len() as f32 * row_h + 10.0 * s;
            let card = RectF::new(pad + ci as f32 * (col_w + gap), col_y[ci] - self.scroll, col_w, card_h);
            col_y[ci] += card_h + gap;
            if card.bottom() < top || card.y > bottom {
                continue;
            }
            c.fill_rrect(card, 12.0 * s, super::with_alpha(t.fg, 0x0d));
            c.text(g, RectF::new(card.x + 16.0 * s, card.y + 6.0 * s, card.w - 32.0 * s, 28.0 * s), font, fs * 1.05, true, Align::Left, t.accent);
            for (ri, e) in items.iter().enumerate() {
                let ry = card.y + 38.0 * s + ri as f32 * row_h;
                let mut kx = card.x + 16.0 * s;
                for (ki, cap) in keycaps(&e.combo).iter().enumerate() {
                    if ki > 0 {
                        c.text("+", RectF::new(kx, ry, 12.0 * s, row_h), font, fs * 0.85, false, Align::Center, t.dim);
                        kx += 12.0 * s;
                    }
                    let cw = (c.measure(cap, font, fs * 0.9, true) + 14.0 * s).max(26.0 * s);
                    let kr = RectF::new(kx, ry + 4.0 * s, cw, row_h - 8.0 * s);
                    c.fill_rrect(kr, 6.0 * s, super::with_alpha(t.fg, 0x1c));
                    c.fill_rect(RectF::new(kr.x + 3.0 * s, kr.bottom() - 2.0 * s, kr.w - 6.0 * s, 2.0 * s), super::with_alpha(t.fg, 0x14));
                    c.text(cap, kr, font, fs * 0.9, true, Align::Center, t.fg);
                    kx += cw;
                }
                let tx = (card.x + col_w * 0.48).max(kx + 10.0 * s);
                let col = if e.broken { t.critical } else { super::with_alpha(t.fg, 0xd8) };
                let label = if e.broken { format!("{}  (not registered)", e.text) } else { e.text.clone() };
                c.text(&label, RectF::new(tx, ry, card.right() - tx - 12.0 * s, row_h), font, fs * 0.95, false, Align::Left, col);
            }
        }
        c.pop_clip();
        let content_h = col_y.iter().cloned().fold(top, f32::max) - top;
        if groups.is_empty() {
            c.text("No binds match", RectF::new(0.0, top, w, 60.0 * s), font, fs * 1.2, false, Align::Center, t.dim);
        }

        let hint = format!("Esc close · type to filter · scroll for more · binds live in {}", self.conf.display());
        c.text(&hint, RectF::new(pad, h - 34.0 * s, w - 2.0 * pad, 24.0 * s), font, fs * 0.85, false, Align::Left, t.dim);

        self.hits = hits;
        self.content_h = content_h;
        let _ = self.canvas.end(self.x, self.y, 255);
    }

    fn max_scroll(&self) -> f32 {
        (self.content_h - (self.h - 104.0 * self.s)).max(0.0)
    }
}

fn point(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SHEET_CLOSE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        // A cheat sheet is transient: clicking anywhere else closes it.
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match wp.0 as u32 {
                0x1B => {
                    let cleared = with_sheet(|sh| {
                        let had = !sh.query.is_empty();
                        sh.query.clear();
                        sh.scroll = 0.0;
                        sh.draw();
                        had
                    });
                    if cleared != Some(true) {
                        PostQuitMessage(0);
                    }
                }
                0x08 => {
                    with_sheet(|sh| {
                        sh.query.pop();
                        sh.scroll = 0.0;
                        sh.draw();
                    });
                }
                0x21 | 0x22 | 0x26 | 0x28 => {
                    with_sheet(|sh| {
                        let step = match wp.0 as u32 {
                            0x21 => -sh.h * 0.8,
                            0x22 => sh.h * 0.8,
                            0x26 => -60.0 * sh.s,
                            _ => 60.0 * sh.s,
                        };
                        sh.scroll = (sh.scroll + step).clamp(0.0, sh.max_scroll());
                        sh.draw();
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wp.0 as u32).filter(|c| !c.is_control()) {
                with_sheet(|sh| {
                    if sh.query.chars().count() < 40 {
                        sh.query.push(c);
                        sh.scroll = 0.0;
                        sh.draw();
                    }
                });
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32;
            with_sheet(|sh| {
                sh.scroll = (sh.scroll - delta / 120.0 * 80.0 * sh.s).clamp(0.0, sh.max_scroll());
                sh.draw();
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = point(lp);
            with_sheet(|sh| {
                let t = sh.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, t)| t.clone());
                if t != sh.hover {
                    sh.hover = t;
                    sh.draw();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point(lp);
            let target = with_sheet(|sh| sh.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, t)| t.clone())).flatten();
            match target {
                Some(Target::Edit) => {
                    let conf = with_sheet(|sh| sh.conf.clone()).unwrap_or_default();
                    let _ = win::shell_exec(&format!("notepad.exe \"{}\"", conf.display()));
                    PostQuitMessage(0);
                }
                Some(Target::Close) => PostQuitMessage(0),
                None => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(combo: &str, text: &str) -> Entry {
        Entry { combo: combo.into(), group: "Workspaces".into(), text: text.into(), broken: false }
    }

    #[test]
    fn workspace_runs_collapse() {
        let mut v: Vec<Entry> = (1..=9).map(|n| e(&format!("SUPER + {n}"), &format!("Go to workspace {n}"))).collect();
        v.push(e("SUPER + 0", "Go to workspace 10"));
        v.extend((1..=3).map(|n| e(&format!("SUPER SHIFT + {n}"), &format!("Move window to workspace {n}"))));
        v.push(e("SUPER CTRL + down", "Go to the next workspace"));
        let out = compress(v);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].combo, "SUPER + 1…0");
        assert_eq!(out[0].text, "Go to workspace 1–10");
        assert_eq!(out[1].text, "Move window to workspace 1–3");
        assert_eq!(keycaps("SUPER SHIFT + left"), vec!["Win", "Shift", "←"]);
    }
}
