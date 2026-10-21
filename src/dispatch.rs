//! Dispatchers — the verbs shared by keybinds, `dbctl dispatch` and Lua.

use crate::config::types::{LayoutKind, Orientation};
use crate::layout::Rect;
use crate::state::State;
use crate::win;

pub const DISPATCHERS: &[(&str, &str)] = &[
    ("exec", "CMD — launch a program/URI through the Windows shell"),
    ("launcher", "run [main] command from launcher.ini"),
    ("killactive", "close the focused window (WM_CLOSE)"),
    ("togglefloating", "toggle floating for the focused window"),
    ("fullscreen", "[0|1] 0 = whole monitor, 1 = maximise with gaps"),
    ("pin", "toggle showing the focused window on every workspace"),
    ("centerwindow", "center the focused floating window"),
    ("movefocus", "l|r|u|d"),
    ("movewindow", "l|r|u|d | mon:N | mon:+1 | mon:-1"),
    ("swapwithmaster", "swap the focused window with the first tile"),
    ("cyclenext", "[prev] focus the next window"),
    ("togglesplit", "flip dwindle split direction"),
    ("splitratio", "+D | -D | exact N (dwindle ratio / master mfact)"),
    ("layout", "dwindle | master | toggle"),
    ("layoutmsg", "addmaster | removemaster | orientationleft|right|top|bottom | swapwithmaster"),
    ("resizeactive", "DX DY — resize the focused floating window"),
    ("moveactive", "DX DY — move the focused floating window"),
    ("focusmonitor", "N | +1 | -1"),
    ("workspace", "N | +1 | -1 | e+1 | e-1 — switch virtual desktop"),
    ("movetoworkspace", "N[,WINDOW] — move a window (default: focused) and follow"),
    ("movetoworkspacesilent", "N[,WINDOW] — move a window (default: focused), stay"),
    ("focuswindow", "exe:REGEX | title:REGEX | class:REGEX"),
    ("closewindow", "exe:REGEX | title:REGEX | class:REGEX"),
    ("lua", "FUNCTION — call a Lua function from the profile script"),
    ("binds", "show the keybinding cheat sheet (toggle)"),
    ("wallpaper", "[picker | search QUERY | random | set PATH] — browse wallhaven.cc / apply with animation"),
    ("reload", "reload the active profile"),
    ("exit", "quit deadbeef"),
];

pub fn is_known(name: &str) -> bool {
    DISPATCHERS.iter().any(|(n, _)| *n == name)
}

/// Section a bind is listed under in the cheat sheet.
pub fn group(name: &str, arg: &str) -> &'static str {
    match name {
        "exec" | "launcher" => "Apps",
        "killactive" | "togglefloating" | "fullscreen" | "pin" | "centerwindow" | "resizeactive" | "moveactive"
        | "closewindow" => "Windows",
        "movewindow" if arg.starts_with("mon:") => "Monitors",
        "movefocus" | "movewindow" | "cyclenext" | "swapwithmaster" | "focuswindow" => "Focus & move",
        "togglesplit" | "splitratio" | "layout" | "layoutmsg" => "Layout",
        "workspace" | "movetoworkspace" | "movetoworkspacesilent" => "Workspaces",
        "focusmonitor" => "Monitors",
        "wallpaper" | "lua" | "binds" => "Extras",
        _ => "deadbeef",
    }
}

/// Human description of a bind for the cheat sheet.
pub fn describe(name: &str, arg: &str) -> String {
    let dir = |a: &str| match a.trim() {
        "l" | "left" => "left",
        "r" | "right" => "right",
        "u" | "up" | "t" => "up",
        "d" | "down" | "b" => "down",
        o => o,
    }
    .to_string();
    let ws = |a: &str| match a.trim() {
        "e+1" | "+1" => "the next workspace".to_string(),
        "e-1" | "-1" => "the previous workspace".to_string(),
        n => format!("workspace {n}"),
    };
    let mon = |a: &str| match a.trim() {
        "+1" => "the next monitor".to_string(),
        "-1" => "the previous monitor".to_string(),
        n => format!("monitor {n}"),
    };
    match name {
        "exec" => {
            let prog = crate::win::split_command(arg).0;
            let prog = prog.rsplit('\\').next().unwrap_or(&prog).trim_end_matches(".exe").to_string();
            match prog.as_str() {
                "wt" => "Terminal".into(),
                "explorer" if arg.contains("ms-screenclip") => "Screenshot (Snipping)".into(),
                "explorer" => "File Explorer".into(),
                "msedge" => "Browser (Edge)".into(),
                _ => format!("Run {arg}"),
            }
        }
        "launcher" => "App launcher".into(),
        "killactive" => "Close window".into(),
        "togglefloating" => "Float / tile window".into(),
        "fullscreen" if arg.trim() == "1" => "Maximise (keep gaps)".into(),
        "fullscreen" => "Fullscreen".into(),
        "pin" => "Pin window to all workspaces".into(),
        "centerwindow" => "Center floating window".into(),
        "movefocus" => format!("Focus {}", dir(arg)),
        "movewindow" => match arg.strip_prefix("mon:") {
            Some(m) => format!("Move window to {}", mon(m)),
            None => format!("Move window {}", dir(arg)),
        },
        "cyclenext" if arg.trim() == "prev" => "Previous window".into(),
        "cyclenext" => "Next window".into(),
        "swapwithmaster" => "Swap with master".into(),
        "togglesplit" => "Toggle split direction".into(),
        "splitratio" if arg.trim_start().starts_with('-') => "Shrink split".into(),
        "splitratio" => "Grow split".into(),
        "layout" if arg.is_empty() || arg == "toggle" => "Switch layout (dwindle / master)".into(),
        "layout" => format!("Layout: {arg}"),
        "layoutmsg" => format!("Layout: {arg}"),
        "resizeactive" => format!("Resize window by {arg}"),
        "moveactive" => format!("Move window by {arg}"),
        "focusmonitor" => format!("Focus {}", mon(arg)),
        "workspace" => format!("Go to {}", ws(arg)),
        "movetoworkspace" => format!("Move window to {}", ws(arg)),
        "movetoworkspacesilent" => format!("Send window to {} (stay)", ws(arg)),
        "focuswindow" => format!("Focus {arg}"),
        "closewindow" => format!("Close {arg}"),
        "wallpaper" if arg.trim() == "random" => "Random wallpaper".into(),
        "wallpaper" => "Wallpaper picker".into(),
        "lua" => format!("Lua: {arg}"),
        "binds" => "This cheat sheet".into(),
        "reload" => "Reload config".into(),
        "exit" => "Quit deadbeef".into(),
        o => format!("{o} {arg}"),
    }
}

#[derive(Clone, Copy)]
enum Dir {
    L,
    R,
    U,
    D,
}

fn parse_dir(s: &str) -> Result<Dir, String> {
    match s.trim() {
        "l" | "left" => Ok(Dir::L),
        "r" | "right" => Ok(Dir::R),
        "u" | "up" | "t" => Ok(Dir::U),
        "d" | "down" | "b" => Ok(Dir::D),
        o => Err(format!("invalid direction '{o}' (l | r | u | d)")),
    }
}

/// Nearest rect in direction `dir` from `from` (centre-to-centre, with the
/// off-axis distance weighted so straight neighbours win).
fn nearest(from: Rect, dir: Dir, candidates: impl Iterator<Item = (isize, Rect)>) -> Option<isize> {
    let (fx, fy) = from.center();
    candidates
        .filter_map(|(h, r)| {
            let (cx, cy) = r.center();
            let (along, across) = match dir {
                Dir::L => (fx - cx, (cy - fy).abs()),
                Dir::R => (cx - fx, (cy - fy).abs()),
                Dir::U => (fy - cy, (cx - fx).abs()),
                Dir::D => (cy - fy, (cx - fx).abs()),
            };
            (along > 0).then_some((h, along as i64 + 2 * across as i64))
        })
        .min_by_key(|&(_, d)| d)
        .map(|(h, _)| h)
}

impl State {
    /// The real foreground window when we manage it (our cached focus can
    /// lag one hook event behind), else the last focused client.
    fn focused_or_foreground(&self) -> Option<isize> {
        let f = win::foreground();
        if self.is_managed(f) {
            Some(f)
        } else {
            self.focused.filter(|h| win::is_window(*h))
        }
    }

    /// `ARG` or `ARG,WINDOW` where WINDOW is `hwnd:0x…`, `exe:RE`, `class:RE`
    /// or `title:RE` (Hyprland's `address:` equivalent). Without WINDOW the
    /// focused window is used.
    fn split_target(&self, arg: &str) -> Result<(String, isize), String> {
        match arg.split_once(',') {
            Some((a, spec)) => {
                let spec = spec.trim();
                let h = if let Some(hex) = spec.strip_prefix("hwnd:").or_else(|| spec.strip_prefix("address:")) {
                    let h = isize::from_str_radix(hex.trim().trim_start_matches("0x"), 16)
                        .map_err(|_| format!("invalid hwnd '{hex}'"))?;
                    self.is_managed(h).then_some(h).ok_or_else(|| format!("{hex} is not a managed window"))?
                } else {
                    self.find_client(spec)?.ok_or("no matching window")?
                };
                Ok((a.trim().to_string(), h))
            }
            None => Ok((arg.to_string(), self.require_focused()?)),
        }
    }

    fn require_focused(&self) -> Result<isize, String> {
        self.focused_or_foreground().ok_or_else(|| "no focused window".to_string())
    }

    fn visible_clients(&self, tiled_only: bool) -> Vec<(isize, Rect)> {
        self.clients
            .iter()
            .filter(|c| !tiled_only || (!c.floating && c.fullscreen.is_none()))
            .filter(|c| self.is_tile_visible(c.hwnd))
            .filter_map(|c| win::visual_rect(c.hwnd).map(|r| (c.hwnd, r)))
            .collect()
    }

    fn find_client(&self, spec: &str) -> Result<Option<isize>, String> {
        let rule = crate::wm::rules::parse_rule(&format!("float, {spec}"))?;
        Ok(self.clients.iter().find(|c| rule.matches(&c.info())).map(|c| c.hwnd))
    }

    fn resolve_monitor(&self, arg: &str, current: usize) -> Result<usize, String> {
        let n = self.monitors.len().max(1) as i64;
        let arg = arg.trim();
        if arg.starts_with('+') || arg.starts_with('-') {
            let d: i64 = arg.parse().map_err(|_| format!("invalid monitor '{arg}'"))?;
            Ok((current as i64 + d).rem_euclid(n) as usize)
        } else {
            let i: usize = arg.parse().map_err(|_| format!("invalid monitor '{arg}'"))?;
            if i >= self.monitors.len() {
                return Err(format!("monitor {i} does not exist ({} connected)", self.monitors.len()));
            }
            Ok(i)
        }
    }

    pub fn run_bind(&mut self, index: usize) {
        let Some(b) = self.config.binds.get(index).cloned() else { return };
        if let Err(e) = self.dispatch(&b.dispatcher, &b.arg) {
            crate::log_warn!("bind {} → {} {}: {e}", b.combo, b.dispatcher, b.arg);
        }
    }

    pub fn dispatch(&mut self, name: &str, arg: &str) -> Result<String, String> {
        let arg = arg.trim();
        match name {
            "exec" => {
                win::shell_exec(arg)?;
                Ok(format!("launched {arg}"))
            }
            "launcher" => {
                // `command = builtin` (or empty) uses deadbeef's own launcher;
                // anything else is run, falling back to the built-in one.
                let configured = self.launcher.command().filter(|c| {
                    let c = c.trim();
                    !c.is_empty() && !c.eq_ignore_ascii_case("builtin")
                });
                if let Some(cmd) = &configured {
                    match win::shell_exec(cmd) {
                        Ok(()) => return Ok(format!("launched {cmd}")),
                        Err(e) => crate::log_warn!("launcher: {e}; using the built-in launcher"),
                    }
                }
                crate::launcher::toggle(crate::launcher::Look::from(&self.launcher));
                Ok("launcher toggled".into())
            }
            "killactive" => {
                let h = self.focused_or_foreground().unwrap_or_else(win::foreground);
                win::close(h);
                Ok("closed".into())
            }
            "togglefloating" => {
                let h = self.require_focused()?;
                let i = self.index_of(h).unwrap();
                let floating = !self.clients[i].floating;
                self.clients[i].floating = floating;
                self.clients[i].auto_floated = false;
                self.clients[i].target = None;
                if floating {
                    // Pop out at 60% of the work area, centred, above the tiles.
                    let area = self.work_area(&self.monitor_of(h));
                    let r = Rect::new(area.x + area.w / 5, area.y + area.h / 5, area.w * 3 / 5, area.h * 3 / 5);
                    self.animator.cancel(h);
                    win::move_window_raw(h, win::outer_for_visual(h, r));
                    win::bring_to_top(h);
                }
                self.refresh_borders();
                self.retile();
                Ok(format!("floating = {floating}"))
            }
            "fullscreen" => {
                let mode: u8 = if arg.is_empty() { 0 } else { arg.parse().map_err(|_| "fullscreen 0 | 1")? };
                let h = self.require_focused()?;
                let i = self.index_of(h).unwrap();
                let c = &mut self.clients[i];
                c.fullscreen = if c.fullscreen == Some(mode) { None } else { Some(mode) };
                c.target = None;
                let on = c.fullscreen.is_some();
                self.refresh_borders();
                self.retile();
                Ok(format!("fullscreen = {on}"))
            }
            "pin" => {
                let h = self.require_focused()?;
                let on = !self.workspaces.is_pinned(h);
                self.workspaces.pin(h, on)?;
                Ok(format!("pinned = {on}"))
            }
            "centerwindow" => {
                let h = self.require_focused()?;
                let i = self.index_of(h).unwrap();
                if !self.clients[i].floating {
                    return Err("centerwindow only applies to floating windows".into());
                }
                let area = self.work_area(&self.monitor_of(h));
                let r = win::visual_rect(h).ok_or("window has no rect")?;
                let c = Rect::new(area.x + (area.w - r.w) / 2, area.y + (area.h - r.h) / 2, r.w, r.h);
                win::move_window_raw(h, win::outer_for_visual(h, c));
                Ok("centered".into())
            }
            "movefocus" => {
                let dir = parse_dir(arg)?;
                let candidates = self.visible_clients(false);
                let target = match self.focused_or_foreground().and_then(|h| win::visual_rect(h).map(|r| (h, r))) {
                    Some((h, r)) => nearest(r, dir, candidates.into_iter().filter(|(c, _)| *c != h)),
                    None => candidates.first().map(|(h, _)| *h),
                };
                let t = target.ok_or("no window in that direction")?;
                win::focus(t);
                Ok(format!("focused {t:#x}"))
            }
            "movewindow" => {
                let h = self.require_focused()?;
                if let Some(m) = arg.strip_prefix("mon:") {
                    let target = self.resolve_monitor(m, self.monitor_index_of(h))?;
                    self.move_to_monitor(h, target);
                    let i = self.index_of(h).unwrap();
                    self.clients[i].target = None;
                    self.retile();
                    self.update_overlay();
                    return Ok(format!("moved to monitor {target}"));
                }
                let dir = parse_dir(arg)?;
                let from = win::visual_rect(h).ok_or("window has no rect")?;
                let tiled = self.visible_clients(true);
                match nearest(from, dir, tiled.into_iter().filter(|(c, _)| *c != h)) {
                    Some(other) => {
                        let (a, b) = (self.index_of(h).unwrap(), self.index_of(other).unwrap());
                        self.swap_clients(a, b);
                        self.retile();
                        Ok("swapped".into())
                    }
                    None => {
                        // Nothing that way on this monitor: hop to the adjacent monitor.
                        let mon_rects = self.monitors.iter().map(|m| (m.index as isize, m.rect));
                        let cur = self.monitor_of(h);
                        let next = nearest(cur.rect, dir, mon_rects.filter(|(i, _)| *i as usize != cur.index))
                            .ok_or("no window or monitor in that direction")?;
                        self.move_to_monitor(h, next as usize);
                        self.retile();
                        Ok(format!("moved to monitor {next}"))
                    }
                }
            }
            "swapwithmaster" => self.dispatch("layoutmsg", "swapwithmaster"),
            "cyclenext" => {
                let visible: Vec<isize> = self.visible_clients(false).into_iter().map(|(h, _)| h).collect();
                if visible.is_empty() {
                    return Err("no windows".into());
                }
                let cur = self.focused_or_foreground().and_then(|h| visible.iter().position(|&v| v == h));
                let n = visible.len();
                let next = match (cur, arg == "prev") {
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                    (None, _) => 0,
                };
                win::focus(visible[next]);
                Ok(format!("focused {:#x}", visible[next]))
            }
            "togglesplit" => {
                let focused = self.focused_or_foreground();
                let flipped = focused.and_then(|h| self.tree_of(h)).is_some_and(|t| t.toggle_split(focused.unwrap()));
                if !flipped {
                    // spiral mode / no tree: flip the global split rule
                    self.split_flip = !self.split_flip;
                }
                self.retile();
                Ok("split toggled".into())
            }
            "splitratio" => {
                let (exact, v) = match arg.strip_prefix("exact") {
                    Some(rest) => (true, rest.trim()),
                    None => (false, arg),
                };
                let v: f32 = v.parse().map_err(|_| format!("invalid ratio '{arg}'"))?;
                match self.layout_for_current() {
                    LayoutKind::Dwindle => {
                        let focused = self.focused_or_foreground();
                        let r = focused.and_then(|h| self.tree_of(h)).and_then(|t| t.resize(focused.unwrap(), v, exact));
                        let r = match r {
                            Some(r) => r,
                            None => {
                                self.split_ratio = if exact { v } else { self.split_ratio + v }.clamp(0.1, 1.9);
                                self.split_ratio
                            }
                        };
                        self.retile();
                        Ok(format!("split ratio = {r:.2}"))
                    }
                    LayoutKind::Master => {
                        self.mfact = if exact { v } else { self.mfact + v / 2.0 }.clamp(0.05, 0.95);
                        self.retile();
                        Ok(format!("mfact = {:.2}", self.mfact))
                    }
                }
            }
            "layout" => {
                let next = match arg {
                    "toggle" | "" => match self.layout_for_current() {
                        LayoutKind::Dwindle => LayoutKind::Master,
                        LayoutKind::Master => LayoutKind::Dwindle,
                    },
                    other => LayoutKind::parse(other)?,
                };
                self.layout_override = Some(next);
                for c in &mut self.clients {
                    c.target = None;
                }
                self.retile();
                Ok(format!("layout = {}", next.name()))
            }
            "layoutmsg" => {
                match arg {
                    "addmaster" => self.nmaster += 1,
                    "removemaster" => self.nmaster = self.nmaster.saturating_sub(1).max(1),
                    "orientationleft" => self.config.master.orientation = Orientation::Left,
                    "orientationright" => self.config.master.orientation = Orientation::Right,
                    "orientationtop" => self.config.master.orientation = Orientation::Top,
                    "orientationbottom" => self.config.master.orientation = Orientation::Bottom,
                    "swapwithmaster" => {
                        let h = self.require_focused()?;
                        let tiled: Vec<isize> = self.visible_clients(true).into_iter().map(|(h, _)| h).collect();
                        let master = *tiled.first().ok_or("no tiled windows")?;
                        let (a, b) = (self.index_of(h).unwrap(), self.index_of(master).unwrap());
                        self.swap_clients(a, b);
                    }
                    o => return Err(format!("unknown layoutmsg '{o}'")),
                }
                self.retile();
                Ok(format!("layoutmsg {arg}"))
            }
            "resizeactive" | "moveactive" => {
                let h = self.require_focused()?;
                let i = self.index_of(h).unwrap();
                if !self.clients[i].floating {
                    return Err(format!("{name} only applies to floating windows"));
                }
                let mut parts = arg.split_whitespace().map(|v| v.parse::<i32>());
                let (Some(Ok(dx)), Some(Ok(dy))) = (parts.next(), parts.next()) else {
                    return Err(format!("{name} DX DY"));
                };
                let r = win::visual_rect(h).ok_or("window has no rect")?;
                let n = if name == "moveactive" {
                    Rect::new(r.x + dx, r.y + dy, r.w, r.h)
                } else {
                    Rect::new(r.x, r.y, (r.w + dx).max(50), (r.h + dy).max(50))
                };
                win::move_window_raw(h, win::outer_for_visual(h, n));
                Ok(format!("{name} {dx} {dy}"))
            }
            "focusmonitor" => {
                let cur = self.focused_or_foreground().map(|h| self.monitor_index_of(h)).unwrap_or(0);
                let target = self.resolve_monitor(arg, cur)?;
                let on_target = self
                    .visible_clients(false)
                    .into_iter()
                    .map(|(h, _)| h)
                    .find(|&h| self.monitor_index_of(h) == target);
                match on_target {
                    Some(h) => win::focus(h),
                    None => {
                        let (x, y) = self.monitors[target].rect.center();
                        unsafe {
                            let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y);
                        }
                    }
                }
                Ok(format!("focused monitor {target}"))
            }
            "workspace" => {
                let n = self.workspaces.resolve(arg)?;
                self.workspaces.switch(n)?;
                Ok(format!("workspace {n}"))
            }
            "movetoworkspace" | "movetoworkspacesilent" => {
                let (ws, h) = self.split_target(arg)?;
                let n = self.workspaces.resolve(&ws)?;
                self.send_to_workspace(h, n, name == "movetoworkspace");
                self.retile();
                Ok(format!("moved to workspace {n}"))
            }
            "focuswindow" => {
                let h = self.find_client(arg)?.ok_or("no matching window")?;
                win::focus(h);
                Ok(format!("focused {h:#x}"))
            }
            "closewindow" => {
                let h = self.find_client(arg)?.ok_or("no matching window")?;
                win::close(h);
                Ok(format!("closed {h:#x}"))
            }
            "lua" => self.call_lua(arg),
            "binds" => {
                let entries = self
                    .config
                    .binds
                    .iter()
                    .enumerate()
                    .map(|(i, b)| crate::ui::cheatsheet::Entry {
                        combo: b.combo.clone(),
                        group: group(&b.dispatcher, &b.arg).to_string(),
                        text: describe(&b.dispatcher, &b.arg),
                        broken: self.hotkeys.status.get(i).is_some_and(|s| s.method == "none"),
                    })
                    .collect();
                let theme = self.wallpaper_settings().theme;
                crate::ui::cheatsheet::toggle(entries, theme, self.paths.active_conf());
                Ok("cheat sheet toggled".into())
            }
            "wallpaper" => {
                let settings = self.wallpaper_settings();
                let (verb, rest) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
                match verb {
                    "" | "picker" => {
                        crate::wallpaper::open_picker(settings, None);
                        Ok("wallpaper picker opened".into())
                    }
                    "search" => {
                        crate::wallpaper::open_picker(settings, Some(rest.trim().to_string()));
                        Ok(format!("searching wallhaven.cc for '{}'", rest.trim()))
                    }
                    "random" => crate::wallpaper::random(settings).map(|p| format!("applying {}", p.display())),
                    "set" => {
                        let path = std::path::PathBuf::from(crate::config::parser::expand_env(rest.trim().trim_matches('"')));
                        crate::wallpaper::apply(settings, path.clone()).map(|_| format!("applying {}", path.display()))
                    }
                    o => Err(format!("unknown wallpaper command '{o}' (picker | search Q | random | set PATH)")),
                }
            }
            "reload" => {
                let errors = self.reload();
                Ok(if errors.is_empty() { "reloaded".into() } else { format!("reloaded with errors:\n{}", errors.join("\n")) })
            }
            "exit" => {
                self.quit_requested = true;
                Ok("bye".into())
            }
            o => Err(format!("unknown dispatcher '{o}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_prefers_straight_neighbours() {
        let from = Rect::new(0, 0, 100, 100);
        let c = vec![(1, Rect::new(110, 0, 100, 100)), (2, Rect::new(110, 300, 100, 100)), (3, Rect::new(-200, 0, 100, 100))];
        assert_eq!(nearest(from, Dir::R, c.clone().into_iter()), Some(1));
        assert_eq!(nearest(from, Dir::L, c.clone().into_iter()), Some(3));
        assert_eq!(nearest(from, Dir::D, c.clone().into_iter()), Some(2));
        assert_eq!(nearest(from, Dir::U, c.into_iter()), None);
    }
}
