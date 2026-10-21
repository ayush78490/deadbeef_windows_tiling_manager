//! Window-manager state. Lives on the main thread only (see `main.rs`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::animation::{Animator, CubicBezier};
use crate::bar::{AppInfo, BarHandle, Snapshot};
use crate::shell::Taskbar;
use crate::config::types::{LayoutKind, Tristate};
use crate::config::{self, Config, Launcher, Paths};
use crate::decoration::{dwm, overlay::Overlay};
use crate::input::hotkey::Hotkeys;
use crate::layout::Rect;
use crate::script::{ClientView, Pending, Script};
use crate::win::{self, Monitor};
use crate::wm::hook::Event;
use crate::wm::rules::{self, Effects, WindowInfo};
use crate::wm::workspace::Workspaces;

#[derive(Debug, Clone)]
pub struct Client {
    pub hwnd: isize,
    pub exe: String,
    pub class: String,
    pub title: String,
    pub floating: bool,
    /// Some(0) = whole monitor, Some(1) = work area (keeps the bar visible).
    pub fullscreen: Option<u8>,
    pub no_border: bool,
    pub opacity: Option<(f32, Option<f32>)>,
    pub applied_alpha: Option<u8>,
    pub added_layered: bool,
    pub iconic: bool,
    pub zoomed: bool,
    /// Not yet placed by the tiler — use the `windowsIn` animation.
    pub fresh: bool,
    /// Last visual rect the tiler assigned.
    pub target: Option<Rect>,
    /// Cached virtual desktop (1-based). Refreshed when the window is
    /// cloaked/uncloaked — which is what a desktop change looks like — so the
    /// bar never needs a cross-process query per window.
    pub workspace: Option<u32>,
    /// Consecutive checks in which the window was larger than its tile
    /// (it has a minimum size); after two it is floated instead.
    pub size_strikes: u8,
    /// Smallest visual size the window accepts, learned at runtime.
    pub min_size: Option<(i32, i32)>,
    /// Floated only because it didn't fit; re-tiled when space frees up.
    pub auto_floated: bool,
}

#[derive(Debug, Serialize)]
pub struct ClientJson {
    pub hwnd: String,
    pub exe: String,
    pub class: String,
    pub title: String,
    pub floating: bool,
    pub fullscreen: Option<u8>,
    pub pinned: bool,
    pub focused: bool,
    pub visible: bool,
    pub monitor: usize,
    pub workspace: Option<u32>,
    pub rect: Option<Rect>,
}

/// Windows that are never managed regardless of style.
const IGNORED_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "MultitaskingViewFrame",
    "ForegroundStaging",
    "TopLevelWindowForOverflowXamlIsland",
    "Shell_InputSwitchTopLevelWindow",
    "NotifyIconOverflowWindow",
    "Windows.Internal.Shell.TabProxyWindow",
    "tooltips_class32",
    "#32768",
    "deadbeef_main",
    "deadbeef_border",
    // The settings app runs in its own process, so the own-pid check misses it.
    "deadbeef_settings",
];

/// Transient shell surfaces that take the foreground without the user
/// leaving their window: Alt+Tab / three-finger task switcher, Task View,
/// Start, Search, the notification centre.
const SHELL_UI_CLASSES: &[&str] = &[
    "XamlExplorerHostIslandWindow",
    "ForegroundStaging",
    "MultitaskingViewFrame",
    "TaskSwitcherWnd",
    "TaskSwitcherOverlayWnd",
    "Windows.UI.Core.CoreWindow",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
];

pub struct State {
    pub paths: Paths,
    pub main: isize,
    pub config: Config,
    pub launcher: Launcher,
    pub clients: Vec<Client>,
    pub ignored: HashSet<isize>,
    pub focused: Option<isize>,
    pub monitors: Vec<Monitor>,
    pub workspaces: Workspaces,
    pub hotkeys: Hotkeys,
    pub animator: Animator,
    pub curves: HashMap<String, Arc<CubicBezier>>,
    pub overlay: Option<Overlay>,
    pub script: Option<Script>,
    pub win11: bool,
    pub layout_override: Option<LayoutKind>,
    pub split_ratio: f32,
    pub split_flip: bool,
    /// Dwindle split trees, keyed by (workspace, monitor index).
    pub trees: std::collections::HashMap<(u32, usize), crate::layout::tree::Tree>,
    /// Last focused window that was already tiled — the one a newly opened
    /// window splits (the new window itself takes focus before it is tiled).
    pub last_tiled_focus: Option<isize>,
    /// Focus-follows-mouse: last cursor position and the window it has been
    /// resting on (with how many ticks), so crossing a window doesn't steal focus.
    last_cursor: (i32, i32),
    /// Window being dragged and its visual rect when the drag started.
    drag_start: Option<(isize, Rect)>,
    /// Live drag-resize in progress: place windows without animation.
    pub resizing: bool,
    hover: Option<(isize, u8)>,
    pub mfact: f32,
    pub nmaster: usize,
    pub dragging: Option<isize>,
    pub exec_once_done: bool,
    pub quit_requested: bool,
    /// Shell UI (task switcher, Task View, Start, our own panels) is in the
    /// foreground: every window is drawn opaque until a real window is back.
    opacity_suspended: bool,
    pub bar: Option<BarHandle>,
    bar_msg: u32,
    bar_dirty: bool,
    pub taskbar: Taskbar,
    config_sig: u64,
    script_depth: u32,
}

impl State {
    pub fn new(paths: Paths, main: isize, ll_msg: u32, bar_msg: u32) -> Self {
        let monitors = win::monitors();
        let refresh = monitors.iter().map(|m| m.refresh_hz).max().unwrap_or(60);
        let taskbar = Taskbar::new(&paths.root);
        Self {
            paths,
            main,
            config: Config::default(),
            launcher: Launcher::default(),
            clients: Vec::new(),
            ignored: HashSet::new(),
            focused: None,
            monitors,
            workspaces: Workspaces::new(),
            hotkeys: Hotkeys::new(main, ll_msg),
            animator: Animator::spawn(refresh),
            curves: HashMap::new(),
            overlay: None,
            script: None,
            win11: win::is_windows11(),
            layout_override: None,
            split_ratio: 1.0,
            split_flip: false,
            trees: std::collections::HashMap::new(),
            last_tiled_focus: None,
            last_cursor: (i32::MIN, i32::MIN),
            drag_start: None,
            resizing: false,
            hover: None,
            mfact: 0.55,
            nmaster: 1,
            dragging: None,
            exec_once_done: false,
            quit_requested: false,
            opacity_suspended: false,
            bar: None,
            bar_msg,
            bar_dirty: true,
            taskbar,
            config_sig: 0,
            script_depth: 0,
        }
    }

    // ───────────────────────────── config ─────────────────────────────

    /// (Re)loads the active profile and applies it. Returns config errors.
    pub fn reload(&mut self) -> Vec<String> {
        let t0 = Instant::now();
        let (cfg, launcher) = config::load(&self.paths);
        self.config = cfg;
        self.config_sig = self.profile_signature();
        self.launcher = launcher;

        for (k, v) in &self.config.env {
            std::env::set_var(k, config::parser::expand_env(v));
        }

        self.curves.clear();
        self.curves.insert("default".into(), Arc::new(CubicBezier::default_curve()));
        for (name, p) in &self.config.animations.beziers {
            self.curves.insert(name.clone(), Arc::new(CubicBezier::from_points(*p)));
        }

        self.hotkeys.register_all(&self.config.binds, self.config.input.hotkey_fallback);
        for s in &self.hotkeys.status {
            if s.method == "none" {
                self.config.errors.push(format!("bind {}: {}", s.combo, s.error.clone().unwrap_or_default()));
            }
        }

        self.setup_overlay();

        let (script, errs) = Script::load(&self.config.scripts, &self.launcher);
        self.script = script;
        self.config.errors.extend(errs);

        self.split_ratio = self.config.dwindle.default_split_ratio;
        self.mfact = self.config.master.mfact;
        self.layout_override = None;

        self.setup_bar();
        self.apply_monitor_rules();

        // Rules may have changed: re-evaluate border/opacity decisions.
        for i in 0..self.clients.len() {
            let fx = rules::evaluate(&self.config.rules, &self.clients[i].info());
            self.clients[i].no_border = fx.no_border;
            self.clients[i].opacity = fx.opacity;
            self.clients[i].applied_alpha = None;
        }
        for h in self.clients.iter().map(|c| c.hwnd).collect::<Vec<_>>() {
            self.apply_static_decorations(h);
        }
        self.refresh_borders();

        for cmd in self.config.exec.clone() {
            self.exec(&cmd);
        }
        if !self.exec_once_done {
            self.exec_once_done = true;
            for cmd in self.config.exec_once.clone() {
                self.exec(&cmd);
            }
        }

        self.retile();

        let errors = self.config.errors.clone();
        crate::log_info!(
            "config loaded in {:.2}ms ({} binds, {} rules, {} errors)",
            t0.elapsed().as_secs_f64() * 1000.0,
            self.config.binds.len(),
            self.config.rules.len(),
            errors.len()
        );
        for e in &errors {
            crate::log_warn!("config: {e}");
        }
        if !errors.is_empty() && self.config.misc.notify_errors {
            crate::tray::balloon(
                self.main,
                &format!("deadbeef: {} config error(s)", errors.len()),
                &errors[0],
                true,
            );
        }
        crate::tray::set_tip(
            self.main,
            &format!("deadbeef — {}", config::profile::origin(&self.paths).unwrap_or_else(|| "active".into())),
        );
        errors
    }

    /// Starts, reconfigures or stops the bar and the taskbar replacement.
    fn setup_bar(&mut self) {
        let cfg = self.config.bar.clone();
        if cfg.enabled {
            match &mut self.bar {
                Some(b) => b.configure(cfg.clone()),
                None => match BarHandle::spawn(cfg.clone(), self.main, self.bar_msg) {
                    Ok(b) => self.bar = Some(b),
                    Err(e) => {
                        self.config.errors.push(format!("bar: {e}"));
                        crate::log_error!("bar: {e}");
                    }
                },
            }
        } else {
            self.bar = None;
        }
        if cfg.enabled && cfg.hide_taskbar && self.bar.is_some() {
            self.taskbar.hide();
        } else {
            self.taskbar.show();
        }
        self.bar_dirty = true;
    }

    /// Sends the bar a fresh snapshot if anything it shows may have changed.
    pub fn push_bar(&mut self) {
        if !std::mem::take(&mut self.bar_dirty) || self.bar.is_none() {
            return;
        }
        let snap = self.bar_snapshot();
        if let Some(b) = &mut self.bar {
            b.update(snap);
        }
    }

    pub fn mark_bar_dirty(&mut self) {
        self.bar_dirty = true;
    }

    fn bar_snapshot(&self) -> Snapshot {
        let current = self.workspaces.current().unwrap_or(1);
        let mut occupied = Vec::new();
        let mut apps = Vec::new();
        for c in &self.clients {
            if !win::is_window(c.hwnd) {
                continue;
            }
            if let Some(ws) = c.workspace {
                if !occupied.contains(&ws) {
                    occupied.push(ws);
                }
            }
            // Minimised windows stay in the bar; cloaked ones live on other desktops.
            if win::is_visible(c.hwnd) && !win::is_cloaked(c.hwnd) {
                apps.push(AppInfo {
                    hwnd: c.hwnd,
                    title: c.title.clone(),
                    exe: c.exe.clone(),
                    focused: self.focused == Some(c.hwnd),
                    minimized: win::is_iconic(c.hwnd),
                    monitor: self.monitor_index_of(c.hwnd),
                });
            }
        }
        occupied.sort_unstable();
        let title = self
            .focused
            .and_then(|h| self.index_of(h))
            .map(|i| self.clients[i].title.clone())
            .unwrap_or_default();
        Snapshot { workspaces: self.workspaces.count(), current, occupied, apps, title }
    }

    /// Wallpaper settings resolved against the current config.
    pub fn wallpaper_settings(&self) -> crate::wallpaper::Settings {
        let w = &self.config.wallpaper;
        let curve = self.config.animations.beziers.get(&w.bezier).copied().unwrap_or([0.0, 0.75, 0.15, 1.0]);
        let dir = if w.directory.as_os_str().is_empty() { self.paths.root.join("wallpapers") } else { w.directory.clone() };
        let b = &self.config.bar;
        crate::wallpaper::Settings {
            cfg: w.clone(),
            theme: crate::wallpaper::Theme {
                bg: b.background,
                fg: b.foreground,
                accent: b.accent,
                dim: b.dim,
                critical: b.critical,
            },
            curve,
            dir,
        }
    }

    /// Size + mtime of every profile file and everything it sources.
    fn profile_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(self.paths.active())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.extend(self.config.files.iter().cloned());
        files.extend(self.config.scripts.iter().cloned());
        files.sort();
        files.dedup();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for f in files {
            f.hash(&mut h);
            if let Ok(m) = std::fs::metadata(&f) {
                m.len().hash(&mut h);
                m.modified().ok().hash(&mut h);
            }
        }
        h.finish()
    }

    /// True when the profile on disk differs from what was last loaded.
    pub fn profile_changed(&self) -> bool {
        self.profile_signature() != self.config_sig
    }

    fn setup_overlay(&mut self) {
        let want = match self.config.general.border_overlay {
            Tristate::Auto => !self.win11,
            Tristate::On => true,
            Tristate::Off => false,
        };
        if want && self.overlay.is_none() {
            match Overlay::new() {
                Ok(o) => self.overlay = Some(o),
                Err(e) => crate::log_error!("border overlay unavailable: {e}"),
            }
        } else if !want {
            self.overlay = None;
        }
    }

    fn apply_monitor_rules(&mut self) {
        self.monitors = win::monitors();
        for rule in self.config.monitors.clone() {
            let Some(m) = self.monitors.get(rule.index).cloned() else {
                if rule.index != 0 {
                    crate::log_info!("monitor rule for index {} ignored: monitor not connected", rule.index);
                }
                continue;
            };
            if let Err(e) = win::apply_display_mode(&m, &rule) {
                self.config.errors.push(format!("monitor {}: {e}", rule.index));
            }
            if rule.scale != 1.0 && (rule.scale - m.scale).abs() > 0.01 {
                crate::log_info!(
                    "monitor {}: scale {} requested, Windows reports {} (set scaling in Settings > Display)",
                    rule.index,
                    rule.scale,
                    m.scale
                );
            }
        }
        self.monitors = win::monitors();
    }

    /// Runs a keyword change (`dbctl keyword`, Lua) and re-applies what it affects.
    pub fn keyword(&mut self, key: &str, value: &str) -> Result<String, String> {
        self.config.set(key, value)?;
        if key == "bind" {
            self.hotkeys.register_all(&self.config.binds, self.config.input.hotkey_fallback);
        }
        if key == "general:border_overlay" {
            self.setup_overlay();
        }
        if key == "dwindle:default_split_ratio" {
            self.split_ratio = self.config.dwindle.default_split_ratio;
        }
        if key == "master:mfact" {
            self.mfact = self.config.master.mfact;
        }
        if key.starts_with("decoration:") || key.starts_with("general:") {
            for i in 0..self.clients.len() {
                self.clients[i].applied_alpha = None;
            }
            for h in self.clients.iter().map(|c| c.hwnd).collect::<Vec<_>>() {
                self.apply_static_decorations(h);
            }
            self.refresh_borders();
        }
        self.retile();
        Ok(format!("{key} = {}", self.config.get(key).unwrap_or_else(|| value.to_string())))
    }

    pub fn exec(&self, cmd: &str) {
        match win::shell_exec(cmd) {
            Ok(()) => crate::log_info!("exec: {cmd}"),
            Err(e) => crate::log_warn!("exec '{cmd}': {e}"),
        }
    }

    // ──────────────────────────── clients ─────────────────────────────

    pub fn index_of(&self, h: isize) -> Option<usize> {
        self.clients.iter().position(|c| c.hwnd == h)
    }

    pub fn is_managed(&self, h: isize) -> bool {
        self.index_of(h).is_some()
    }

    /// Structural test: is this a normal application window?
    fn is_manageable(h: isize) -> bool {
        if h == 0 || !win::is_window(h) || !win::is_top_level(h) || !win::is_visible(h) || win::is_cloaked(h) {
            return false;
        }
        if win::pid(h) == win::own_pid() {
            return false;
        }
        use windows::Win32::UI::WindowsAndMessaging::{
            WS_CAPTION, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_THICKFRAME,
        };
        let style = win::style(h);
        let ex = win::ex_style(h);
        if style & WS_CHILD.0 != 0 || ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return false;
        }
        if style & (WS_CAPTION.0 | WS_THICKFRAME.0) == 0 {
            return false;
        }
        let class = win::class_name(h);
        if IGNORED_CLASSES.contains(&class.as_str()) {
            return false;
        }
        !win::title(h).is_empty()
    }

    fn default_float(&self, h: isize, class: &str) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{WS_EX_DLGMODALFRAME, WS_THICKFRAME};
        let misc = &self.config.misc;
        if misc.float_dialogs
            && (win::owner(h).is_some() || class == "#32770" || win::ex_style(h) & WS_EX_DLGMODALFRAME.0 != 0)
        {
            return true;
        }
        misc.float_unresizable && win::style(h) & WS_THICKFRAME.0 == 0
    }

    /// Starts managing `h` if it is an application window. Returns true if added.
    pub fn manage(&mut self, h: isize) -> bool {
        if self.is_managed(h) || self.ignored.contains(&h) || !Self::is_manageable(h) {
            return false;
        }
        let info = WindowInfo { exe: win::exe_name(h), class: win::class_name(h), title: win::title(h) };
        let mut fx = rules::evaluate(&self.config.rules, &info);
        if let Some(script) = &self.script {
            let view = ClientView {
                hwnd: h as i64,
                exe: info.exe.clone(),
                class: info.class.clone(),
                title: info.title.clone(),
                floating: false,
                monitor: self.monitor_index_of(h),
            };
            script.window_open(&view, &mut fx);
        }
        if fx.ignore {
            self.ignored.insert(h);
            crate::log_info!("ignoring {} ({}) by rule", info.exe, info.class);
            return false;
        }
        let floating = fx.float.unwrap_or_else(|| self.default_float(h, &info.class));
        let client = Client {
            hwnd: h,
            exe: info.exe,
            class: info.class,
            title: info.title,
            floating,
            fullscreen: if fx.fullscreen { Some(0) } else { None },
            no_border: fx.no_border,
            opacity: fx.opacity,
            applied_alpha: None,
            added_layered: false,
            iconic: win::is_iconic(h),
            zoomed: win::is_zoomed(h),
            fresh: true,
            target: None,
            workspace: self.workspaces.of_window(h),
            size_strikes: 0,
            min_size: None,
            auto_floated: false,
        };
        crate::log_info!(
            "manage {:#x} {} [{}] \"{}\"{}",
            h,
            client.exe,
            client.class,
            client.title,
            if floating { " (floating)" } else { "" }
        );
        let new_on_top = self.layout_for_current() == LayoutKind::Master && self.config.master.new_on_top;
        if new_on_top {
            self.clients.insert(0, client);
        } else {
            self.clients.push(client);
        }
        self.apply_static_decorations(h);
        self.apply_effects(h, &fx);
        // Tiling replaces maximising: a window that arrives maximised is
        // restored and tiled (the maximise button toggles "maximise with gaps").
        if let Some(i) = self.index_of(h) {
            if self.clients[i].zoomed && !self.clients[i].floating && !fx.maximize {
                win::restore(h);
                self.clients[i].zoomed = false;
            }
        }
        self.refresh_border(h);
        true
    }

    /// Placement side effects of rules that need a live window.
    fn apply_effects(&mut self, h: isize, fx: &Effects) {
        if fx.pin {
            if let Err(e) = self.workspaces.pin(h, true) {
                crate::log_warn!("{e}");
            }
        }
        if let Some(m) = fx.monitor {
            self.move_to_monitor(h, m);
        }
        if let Some((ws, silent)) = fx.workspace {
            self.send_to_workspace(h, ws, !silent);
        }
        if fx.maximize {
            win::maximize(h);
        }
        let is_float = self.index_of(h).map(|i| self.clients[i].floating).unwrap_or(false);
        if is_float && (fx.size.is_some() || fx.position.is_some() || fx.center) {
            let mon = self.monitor_of(h);
            let area = self.work_area(&mon);
            let cur = win::visual_rect(h).unwrap_or(area);
            let (w, hgt) = fx.size.map(|(w, hh)| (w.resolve(area.w), hh.resolve(area.h))).unwrap_or((cur.w, cur.h));
            let (mut x, mut y) = fx
                .position
                .map(|(x, y)| (area.x + x.resolve(area.w), area.y + y.resolve(area.h)))
                .unwrap_or((cur.x, cur.y));
            if fx.center {
                x = area.x + (area.w - w) / 2;
                y = area.y + (area.h - hgt) / 2;
            }
            let visual = Rect::new(x, y, w.max(50), hgt.max(50));
            win::move_window_raw(h, win::outer_for_visual(h, visual));
        }
    }

    pub fn unmanage(&mut self, h: isize) -> bool {
        self.ignored.remove(&h);
        self.animator.cancel(h);
        if self.dragging == Some(h) {
            self.dragging = None;
        }
        if self.focused == Some(h) {
            self.focused = None;
            self.update_overlay();
        }
        match self.index_of(h) {
            Some(i) => {
                let c = self.clients.remove(i);
                crate::log_info!("unmanage {:#x} {}", h, c.exe);
                true
            }
            None => false,
        }
    }

    // ─────────────────────────── decorations ──────────────────────────

    fn border_hidden(&self, c: &Client) -> bool {
        c.no_border
            || self.config.general.border_size <= 0
            || (c.floating && self.config.general.no_border_on_floating)
            || c.fullscreen == Some(0)
    }

    /// Corners and backdrop: set once per window and on config change.
    fn apply_static_decorations(&mut self, h: isize) {
        if !self.win11 {
            return;
        }
        dwm::set_rounding(h, self.config.decoration.rounding);
    }

    /// Border colour + opacity for one window, according to focus.
    fn refresh_border(&mut self, h: isize) {
        let Some(i) = self.index_of(h) else { return };
        let active = self.is_active(h);
        let c = self.clients[i].clone();
        if self.win11 {
            if self.overlay.is_some() || self.border_hidden(&c) {
                dwm::hide_border(h);
            } else {
                let g = if active { &self.config.general.active_border } else { &self.config.general.inactive_border };
                dwm::set_border_color(h, g.primary().colorref());
            }
            // Blur belongs behind *translucent* windows, as in Hyprland: the
            // active window keeps the app's own (solid) background.
            if self.config.decoration.blur_enabled && dwm::supports_backdrop() {
                let blur = !active && !self.opacity_suspended;
                dwm::set_backdrop(h, blur.then_some(self.config.decoration.backdrop));
            }
        }

        let deco = &self.config.decoration;
        let alpha = if self.opacity_suspended { 1.0 } else { match c.opacity {
            Some((a, inactive)) => {
                if active {
                    a
                } else {
                    inactive.unwrap_or(a)
                }
            }
            None => {
                if active {
                    deco.active_opacity
                } else {
                    deco.inactive_opacity
                }
            }
        } };
        let alpha8 = (alpha * 255.0).round() as u8;
        let c = &mut self.clients[i];
        if c.applied_alpha == Some(alpha8) {
            return;
        }
        if alpha8 < 255 || c.added_layered {
            let added = win::set_opacity(h, alpha);
            c.added_layered |= added;
        }
        c.applied_alpha = Some(alpha8);
    }

    /// Swaps two clients in the layout (list order and every split tree).
    pub fn swap_clients(&mut self, i: usize, j: usize) {
        let (a, b) = (self.clients[i].hwnd, self.clients[j].hwnd);
        self.clients.swap(i, j);
        for t in self.trees.values_mut() {
            t.swap(a, b);
        }
    }

    /// The split tree holding `h`, if it is tiled in one.
    pub fn tree_of(&mut self, h: isize) -> Option<&mut crate::layout::tree::Tree> {
        self.trees.values_mut().find(|t| t.contains(h))
    }

    /// Re-applies border colour and alpha to every window, ignoring the cache.
    fn reapply_opacity(&mut self) {
        for c in &mut self.clients {
            c.applied_alpha = None;
        }
        self.refresh_borders();
    }

    /// Periodic safety net: Windows does not always report a foreground
    /// change after touchpad gestures, which would leave focus (and
    /// translucency) stale.
    /// Focus follows the mouse (`input:follow_mouse = 1`), called every ~50 ms.
    /// Acts only when the cursor moved and has rested on a managed window for
    /// two ticks; never while a button is held (dragging, selecting), while
    /// shell UI is open, or while the focused app has a menu/popup up.
    pub fn follow_mouse_tick(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        if self.config.input.follow_mouse == 0 || self.opacity_suspended || self.dragging.is_some() {
            return;
        }
        let pos = win::cursor_pos();
        let moved = pos != self.last_cursor;
        self.last_cursor = pos;
        let buttons_down = [0x01, 0x02, 0x04].iter().any(|&vk| unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0);
        if buttons_down {
            self.hover = None;
            return;
        }
        let under = unsafe {
            let p = windows::Win32::Foundation::POINT { x: pos.0, y: pos.1 };
            let h = windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(p);
            win::raw(windows::Win32::UI::WindowsAndMessaging::GetAncestor(h, windows::Win32::UI::WindowsAndMessaging::GA_ROOT))
        };
        if !self.is_managed(under) || self.focused == Some(under) {
            self.hover = None;
            return;
        }
        // A menu / dropdown of the focused app is open: leave it alone.
        let fg = win::foreground();
        if fg != 0 && !self.is_managed(fg) && self.app_window(fg) == self.focused.unwrap_or(0) && self.focused.is_some() {
            return;
        }
        match self.hover {
            Some((h, n)) if h == under => {
                if n >= 1 {
                    self.hover = None;
                    if self.is_tile_visible(under) {
                        win::focus(under);
                        self.focus_changed(under);
                    }
                } else {
                    self.hover = Some((h, n + 1));
                }
            }
            _ if moved => self.hover = Some((under, 0)),
            _ => {}
        }
    }

    /// Learns minimum sizes. A tiled window that stays larger than its tile
    /// after being re-sized again (to rule out a slow app) has
    /// a minimum size; record it and re-tile so the layout gives it room and
    /// squeezes its neighbours instead.
    pub fn check_min_sizes(&mut self) {
        let mut learned = false;
        for c in &mut self.clients {
            let Some(target) = c.target else { continue };
            if c.floating
                || c.fullscreen.is_some()
                || self.dragging == Some(c.hwnd)
                || self.animator.target(c.hwnd).is_some()
                || !win::is_visible(c.hwnd)
                || win::is_iconic(c.hwnd)
                || win::is_cloaked(c.hwnd)
            {
                c.size_strikes = 0;
                continue;
            }
            let Some(actual) = win::visual_rect(c.hwnd) else { continue };
            let (over_w, over_h) = (actual.w > target.w + 8, actual.h > target.h + 8);
            if !over_w && !over_h {
                c.size_strikes = 0;
                continue;
            }
            c.size_strikes += 1;
            if c.size_strikes == 1 {
                // Maybe it was just slow: ask once more and re-check next tick.
                win::move_window_raw(c.hwnd, win::outer_for_visual(c.hwnd, target));
                continue;
            }
            c.size_strikes = 0;
            let (pw, ph) = c.min_size.unwrap_or((0, 0));
            let min = (if over_w { actual.w.max(pw) } else { pw }, if over_h { actual.h.max(ph) } else { ph });
            if c.min_size != Some(min) {
                crate::log_info!("{} has a minimum size of {}x{}; giving it room", c.exe, min.0, min.1);
                c.min_size = Some(min);
                c.target = None;
                learned = true;
            }
        }
        if learned {
            self.retile();
        }
    }

    /// Live drag-resize of a tiled window: move the split boundaries next to
    /// the edges being dragged so the neighbours shrink/grow with it.
    fn live_resize(&mut self, h: isize) {
        let Some((_, start)) = self.drag_start.filter(|(d, _)| *d == h) else { return };
        let Some(i) = self.index_of(h) else { return };
        if self.clients[i].floating {
            return;
        }
        let Some(actual) = win::visual_rect(h) else { return };
        if (actual.w - start.w).abs() <= 2 && (actual.h - start.h).abs() <= 2 {
            return; // a move, not a resize
        }
        let half = self.config.general.gaps_in / 2;
        let mut edges = Vec::new();
        if actual.x != start.x {
            edges.push((crate::layout::tree::Edge::Left, actual.x - half));
        }
        if actual.right() != start.right() {
            edges.push((crate::layout::tree::Edge::Right, actual.right() + half));
        }
        if actual.y != start.y {
            edges.push((crate::layout::tree::Edge::Top, actual.y - half));
        }
        if actual.bottom() != start.bottom() {
            edges.push((crate::layout::tree::Edge::Bottom, actual.bottom() + half));
        }
        let mon = self.monitor_of(h);
        let g = &self.config.general;
        let area = crate::layout::gap_outer(self.work_area(&mon), g.gaps_in, g.gaps_out);
        let mins = self.min_sizes();
        let mins_fn = |w: isize| mins.get(&w).copied().unwrap_or((0, 0));
        let mut changed = false;
        if let Some(tree) = self.trees.values_mut().find(|t| t.contains(h)) {
            for (edge, pos) in edges {
                changed |= tree.set_edge(h, area, edge, pos, &mins_fn);
            }
        }
        if changed {
            self.resizing = true;
            self.retile();
            self.resizing = false;
        }
    }

    /// Learned minimum tile sizes (window minimum + inner gap) for the layout.
    pub fn min_sizes(&self) -> std::collections::HashMap<isize, (i32, i32)> {
        let gap = self.config.general.gaps_in;
        self.clients.iter().filter_map(|c| c.min_size.map(|(w, h)| (c.hwnd, (w + gap, h + gap)))).collect()
    }

    pub fn reconcile_focus(&mut self) {
        let fg = win::foreground();
        // The switcher can reappear on the same (already foreground) host
        // window, which produces no foreground event.
        if !self.opacity_suspended && self.is_shell_ui(fg) {
            self.focus_changed(fg);
            return;
        }
        if self.opacity_suspended {
            if !self.is_shell_ui(fg) {
                self.resume_opacity(fg);
            }
            return;
        }
        if self.is_managed(fg) && self.focused != Some(fg) {
            self.focus_changed(fg);
        }
    }

    pub fn refresh_borders(&mut self) {
        for h in self.clients.iter().map(|c| c.hwnd).collect::<Vec<_>>() {
            self.refresh_border(h);
        }
        self.update_overlay();
    }

    /// Positions the Direct2D border overlay around the focused window.
    pub fn update_overlay(&mut self) {
        if self.overlay.is_none() {
            return;
        }
        let target = self.focused.and_then(|h| self.index_of(h)).map(|i| self.clients[i].clone());
        let show = target.filter(|c| {
            !self.border_hidden(c) && win::is_visible(c.hwnd) && !win::is_iconic(c.hwnd) && !win::is_cloaked(c.hwnd)
        });
        let radius = if self.win11 {
            match self.config.decoration.rounding {
                0 => 0,
                1..=4 => (4.0 * self.monitor_of_client_scale(show.as_ref())) as i32,
                _ => (8.0 * self.monitor_of_client_scale(show.as_ref())) as i32,
            }
        } else {
            0
        };
        let border = self.config.general.border_size;
        let gradient = self.config.general.active_border.clone();
        let overlay = self.overlay.as_mut().unwrap();
        match show.and_then(|c| win::visual_rect(c.hwnd).map(|r| (c, r))) {
            Some((c, rect)) => {
                use windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST;
                let topmost = win::ex_style(c.hwnd) & WS_EX_TOPMOST.0 != 0;
                overlay.show(c.hwnd, rect, &gradient, border, radius, topmost);
            }
            None => overlay.hide(),
        }
    }

    fn monitor_of_client_scale(&self, c: Option<&Client>) -> f32 {
        c.map(|c| self.monitor_of(c.hwnd).scale).unwrap_or(1.0)
    }

    /// Restores every window we decorated (on exit).
    pub fn restore_all(&mut self) {
        self.bar = None;
        self.taskbar.show();
        for c in &self.clients {
            if !win::is_window(c.hwnd) {
                continue;
            }
            if self.win11 {
                dwm::reset_border(c.hwnd);
                dwm::reset_rounding(c.hwnd);
                if self.config.decoration.blur_enabled && dwm::supports_backdrop() {
                    dwm::set_backdrop(c.hwnd, None);
                }
            }
            if c.added_layered {
                win::set_opacity(c.hwnd, 1.0);
                win::clear_layered(c.hwnd);
            } else if c.applied_alpha.is_some_and(|a| a < 255) {
                win::set_opacity(c.hwnd, 1.0);
            }
        }
        if let Some(o) = self.overlay.as_mut() {
            o.hide();
        }
    }

    // ──────────────────────────── monitors ────────────────────────────

    pub fn monitor_index_of(&self, h: isize) -> usize {
        let handle = win::monitor_of_window(h);
        self.monitors.iter().position(|m| m.handle == handle).unwrap_or(0)
    }

    pub fn monitor_of(&self, h: isize) -> Monitor {
        self.monitors[self.monitor_index_of(h).min(self.monitors.len().saturating_sub(1))].clone()
    }

    /// Work area minus `addreserved` space.
    pub fn work_area(&self, m: &Monitor) -> Rect {
        match self.config.monitor_rule(m.index) {
            Some(r) => {
                let [t, b, l, rr] = r.reserved;
                m.work.inset(t, rr, b, l)
            }
            None => m.work,
        }
    }

    pub fn move_to_monitor(&mut self, h: isize, index: usize) {
        let Some(m) = self.monitors.get(index).cloned() else {
            crate::log_warn!("monitor {index} does not exist");
            return;
        };
        if self.monitor_index_of(h) == index {
            return;
        }
        let area = self.work_area(&m);
        let cur = win::visual_rect(h).unwrap_or(area);
        let w = cur.w.min(area.w);
        let hh = cur.h.min(area.h);
        let visual = Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - hh) / 2, w, hh);
        if win::is_zoomed(h) {
            win::restore(h);
        }
        // Synchronous so MonitorFromWindow sees the new monitor on retile.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};
            let o = win::outer_for_visual(h, visual);
            let _ = SetWindowPos(win::hwnd(h), None, o.x, o.y, o.w, o.h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    // ─────────────────────────── workspaces ───────────────────────────

    pub fn layout_for_current(&self) -> LayoutKind {
        if let Some(l) = self.layout_override {
            return l;
        }
        // Asking which desktop is current is a cross-process call; only pay
        // for it when a `workspace = N, layout:…` rule could apply.
        if !self.config.workspaces.iter().any(|w| w.layout.is_some()) {
            return self.config.general.layout;
        }
        self.workspaces
            .current()
            .and_then(|ws| self.config.workspace_rule(ws))
            .and_then(|r| r.layout)
            .unwrap_or(self.config.general.layout)
    }

    /// Moves a window to workspace `ws` (honouring `workspace = N, monitor:M`),
    /// optionally following it.
    pub fn send_to_workspace(&mut self, h: isize, ws: u32, follow: bool) {
        if let Some(m) = self.config.workspace_rule(ws).and_then(|r| r.monitor) {
            if m < self.monitors.len() {
                self.move_to_monitor(h, m);
            }
        }
        if !follow && win::foreground() == h {
            // Windows switches desktops to follow the foreground window, so
            // hand focus to another window here before moving this one away.
            let next = self
                .clients
                .iter()
                .map(|c| c.hwnd)
                .find(|&o| o != h && self.is_tile_visible(o));
            match next {
                Some(o) => win::focus(o),
                None => unsafe {
                    let shell = windows::Win32::UI::WindowsAndMessaging::GetShellWindow();
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(shell);
                },
            }
        }
        if let Err(e) = self.workspaces.move_window(h, ws) {
            crate::log_warn!("{e}");
            return;
        }
        if let Some(i) = self.index_of(h) {
            self.clients[i].workspace = Some(ws);
        }
        if follow {
            if let Err(e) = self.workspaces.switch(ws) {
                crate::log_warn!("{e}");
            }
            win::focus(h);
        }
    }

    // ──────────────────────────── events ──────────────────────────────

    pub fn handle_events(&mut self, events: Vec<(Event, isize)>) {
        let mut dirty = false;
        let mut overlay_dirty = false;
        for (ev, h) in events {
            if ev != Event::LocationChange {
                self.bar_dirty = true;
            }
            if ev == Event::Show && self.taskbar.is_hidden() && !self.is_managed(h) && crate::shell::is_taskbar(h) {
                self.taskbar.enforce();
                continue;
            }
            match ev {
                Event::Create | Event::Show | Event::Uncloaked | Event::MinimizeEnd => {
                    if ev == Event::Uncloaked {
                        if let Some(i) = self.index_of(h) {
                            self.clients[i].workspace = self.workspaces.of_window(h);
                        }
                    }
                    if self.is_managed(h) {
                        dirty |= matches!(ev, Event::Show | Event::Uncloaked | Event::MinimizeEnd);
                    } else {
                        dirty |= self.manage(h);
                    }
                    if self.focused == Some(h) {
                        overlay_dirty = true;
                    }
                }
                Event::NameChange => {
                    if let Some(i) = self.index_of(h) {
                        self.clients[i].title = win::title(h);
                    } else {
                        // Some apps only set a title after showing.
                        dirty |= self.manage(h);
                    }
                }
                Event::Destroy => {
                    if self.unmanage(h) {
                        dirty = true;
                        for c in &mut self.clients {
                            if c.auto_floated {
                                c.auto_floated = false;
                                c.floating = false;
                                c.target = None;
                            }
                        }
                    }
                }
                Event::Hide | Event::Cloaked | Event::MinimizeStart => {
                    if ev == Event::Cloaked {
                        if let Some(i) = self.index_of(h) {
                            self.clients[i].workspace = self.workspaces.of_window(h);
                        }
                    }
                    if self.is_managed(h) {
                        dirty = true;
                        overlay_dirty |= self.focused == Some(h);
                    }
                }
                Event::Foreground => {
                    if !self.is_managed(h) {
                        dirty |= self.manage(h);
                    }
                    self.focus_changed(h);
                }
                Event::MoveSizeStart => {
                    if self.is_managed(h) {
                        self.dragging = Some(h);
                        self.drag_start = win::visual_rect(h).map(|r| (h, r));
                        self.animator.cancel(h);
                    }
                }
                Event::MoveSizeEnd => {
                    if self.dragging == Some(h) {
                        self.dragging = None;
                    }
                    let start = self.drag_start.take().filter(|(d, _)| *d == h).map(|(_, r)| r);
                    if self.is_managed(h) {
                        let resized = match (start, win::visual_rect(h)) {
                            (Some(s), Some(a)) => (a.w - s.w).abs() > 2 || (a.h - s.h).abs() > 2,
                            _ => false,
                        };
                        if resized {
                            // The neighbours already follow live; settle the
                            // dragged window into its new tile.
                            if let Some(i) = self.index_of(h) {
                                self.clients[i].target = None;
                            }
                        } else {
                            self.handle_drop(h);
                        }
                        dirty = true;
                    }
                }
                Event::LocationChange => {
                    if self.dragging == Some(h) {
                        self.live_resize(h);
                    }
                    if let Some(i) = self.index_of(h) {
                        let (iconic, zoomed) = (win::is_iconic(h), win::is_zoomed(h));
                        let c = &mut self.clients[i];
                        if zoomed && !c.zoomed && !c.floating {
                            // Maximise button / title-bar double-click on a tiled
                            // window: toggle "maximise with gaps" instead.
                            c.fullscreen = if c.fullscreen == Some(1) { None } else { Some(1) };
                            c.target = None;
                            win::restore(h);
                            c.zoomed = false;
                            c.iconic = iconic;
                            dirty = true;
                        } else if c.iconic != iconic || c.zoomed != zoomed {
                            c.iconic = iconic;
                            c.zoomed = zoomed;
                            dirty = true;
                        }
                        overlay_dirty |= self.focused == Some(h);
                    }
                }
            }
        }
        if dirty {
            self.retile();
        }
        if dirty || overlay_dirty {
            self.update_overlay();
        }
    }

    /// Shell UI or one of deadbeef's own panels, currently on screen.
    /// (After the task switcher is dismissed, Windows can leave the foreground
    /// on its now-hidden host window — that no longer counts.)
    fn is_shell_ui(&self, h: isize) -> bool {
        h != 0
            && !self.is_managed(h)
            && win::is_visible(h)
            && (win::pid(h) == win::own_pid() || SHELL_UI_CLASSES.contains(&win::class_name(h).as_str()))
    }

    fn root_owner(h: isize) -> isize {
        unsafe {
            win::raw(windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                win::hwnd(h),
                windows::Win32::UI::WindowsAndMessaging::GA_ROOTOWNER,
            ))
        }
    }

    /// The focused window, or the app window that owns the focused dialog.
    fn is_active(&self, h: isize) -> bool {
        match self.focused {
            Some(f) => f == h || (f != h && Self::root_owner(f) == h),
            None => false,
        }
    }

    /// Maps a foreground window to the managed window it belongs to: a menu,
    /// dropdown, tooltip or dialog of the focused app counts as that app, so
    /// the app stays active (and opaque) while you use its popups.
    fn app_window(&self, h: isize) -> isize {
        if h == 0 || self.is_managed(h) {
            return h;
        }
        let root = Self::root_owner(h);
        if self.is_managed(root) {
            return root;
        }
        // Unowned popups (Chromium menus, omnibox dropdowns) from the same
        // process as the focused window.
        if let Some(f) = self.focused {
            if win::is_window(f) && win::pid(h) == win::pid(f) {
                return f;
            }
        }
        h
    }

    /// Shell UI went away: translucency back on, focus stays where it was
    /// unless a managed window took the foreground.
    fn resume_opacity(&mut self, fg: isize) {
        self.opacity_suspended = false;
        if self.is_managed(fg) {
            self.focused = Some(fg);
        }
        self.reapply_opacity();
        self.update_overlay();
    }

    pub fn focus_changed(&mut self, h: isize) {
        // A hidden window can't be where the user is working (e.g. the task
        // switcher's host after it is dismissed) — keep the current focus.
        if h != 0 && !self.is_managed(h) && !win::is_visible(h) {
            if self.opacity_suspended {
                self.resume_opacity(h);
            }
            return;
        }
        if self.is_shell_ui(h) {
            // The task switcher (three-finger swipe / Alt+Tab), Start or one of
            // our panels is up. The user hasn't left their window, so keep focus,
            // but make every window opaque: translucent windows look broken in
            // the switcher's thumbnails and animations.
            if !self.opacity_suspended {
                self.opacity_suspended = true;
                self.reapply_opacity();
            }
            return;
        }
        if self.opacity_suspended {
            self.resume_opacity(h);
            if !self.is_managed(h) {
                return; // e.g. the switcher's hidden host: keep the old focus
            }
        }
        let h = self.app_window(h);
        let new = if self.is_managed(h) { Some(h) } else { None };
        if let Some(n) = new {
            if self.trees.values().any(|t| t.contains(n)) {
                self.last_tiled_focus = Some(n);
            }
        }
        if new == self.focused {
            return;
        }
        let prev = std::mem::replace(&mut self.focused, new);
        if let Some(p) = prev {
            self.refresh_border(p);
            self.refresh_border(Self::root_owner(p));
        }
        if let Some(n) = new {
            self.refresh_border(Self::root_owner(n));
        }
        if let Some(n) = new {
            // Re-assert alpha even if our cache agrees: Windows and some apps
            // reset layered attributes on their own (e.g. after gestures).
            if let Some(i) = self.index_of(n) {
                self.clients[i].applied_alpha = None;
            }
            // A focused window is on the desktop being shown; keep its cached
            // workspace honest (IVirtualDesktopManager, one call per focus change).
            if self.workspaces.is_on_current(n) {
                if let (Some(i), Some(ws)) = (self.index_of(n), self.workspaces.current()) {
                    self.clients[i].workspace = Some(ws);
                }
            }
            self.refresh_border(n);
            if let (Some(script), Some(i)) = (&self.script, self.index_of(n)) {
                let view = self.client_view(&self.clients[i]);
                script.focus(&view);
            }
            self.run_script_pending();
        }
        self.update_overlay();
    }

    /// Dropping a tiled window onto another tiled window swaps them.
    fn handle_drop(&mut self, h: isize) {
        let Some(i) = self.index_of(h) else { return };
        if self.clients[i].floating {
            return;
        }
        let (x, y) = win::cursor_pos();
        let other = self.clients.iter().position(|c| {
            c.hwnd != h && !c.floating && c.target.is_some_and(|r| r.contains(x, y)) && self.is_tile_visible(c.hwnd)
        });
        if let Some(j) = other {
            self.swap_clients(i, j);
        }
        // Force a re-place even if the target rect did not change.
        self.clients[i].target = None;
    }

    // ───────────────────────────── scripts ────────────────────────────

    pub fn client_view(&self, c: &Client) -> ClientView {
        ClientView {
            hwnd: c.hwnd as i64,
            exe: c.exe.clone(),
            class: c.class.clone(),
            title: c.title.clone(),
            floating: c.floating,
            monitor: self.monitor_index_of(c.hwnd),
        }
    }

    pub fn script_snapshot(&self) {
        const KEYS: &[&str] = &[
            "general:gaps_in",
            "general:gaps_out",
            "general:border_size",
            "general:col.active_border",
            "general:col.inactive_border",
            "general:layout",
            "decoration:rounding",
            "decoration:active_opacity",
            "decoration:inactive_opacity",
            "decoration:blur:enabled",
            "animations:enabled",
            "dwindle:default_split_ratio",
            "master:mfact",
        ];
        if let Some(s) = &self.script {
            let clients = self.clients.iter().map(|c| self.client_view(c)).collect();
            let opts = KEYS.iter().filter_map(|k| self.config.get(k).map(|v| (k.to_string(), v))).collect();
            s.set_snapshot(clients, opts);
        }
    }

    pub fn call_lua(&mut self, name: &str) -> Result<String, String> {
        let Some(script) = &self.script else {
            return Err("no Lua script loaded (add `script = ...` to your config)".into());
        };
        self.script_snapshot();
        let r = script.call(name);
        self.run_script_pending();
        r.map(|_| format!("lua {name}: ok"))
    }

    /// Executes actions queued by Lua (bounded to avoid dispatch loops).
    pub fn run_script_pending(&mut self) {
        if self.script_depth > 4 {
            crate::log_warn!("[lua] dispatch recursion limit reached");
            return;
        }
        let pending = match &self.script {
            Some(s) => s.take_pending(),
            None => return,
        };
        self.script_depth += 1;
        for p in pending {
            let r = match &p {
                Pending::Dispatch(n, a) => self.dispatch(n, a),
                Pending::Keyword(k, v) => self.keyword(k, v),
            };
            if let Err(e) = r {
                crate::log_warn!("[lua] {p:?}: {e}");
            }
        }
        self.script_depth -= 1;
    }

    // ───────────────────────────── queries ────────────────────────────

    pub fn clients_json(&self) -> Vec<ClientJson> {
        self.clients
            .iter()
            .filter(|c| win::is_window(c.hwnd))
            .map(|c| ClientJson {
                hwnd: format!("{:#x}", c.hwnd),
                exe: c.exe.clone(),
                class: c.class.clone(),
                title: win::title(c.hwnd),
                floating: c.floating,
                fullscreen: c.fullscreen,
                pinned: self.workspaces.is_pinned(c.hwnd),
                focused: self.focused == Some(c.hwnd),
                visible: self.is_tile_visible(c.hwnd),
                monitor: self.monitor_index_of(c.hwnd),
                workspace: self.workspaces.of_window(c.hwnd),
                rect: win::visual_rect(c.hwnd),
            })
            .collect()
    }
}

impl Client {
    pub fn info(&self) -> WindowInfo {
        WindowInfo { exe: self.exe.clone(), class: self.class.clone(), title: self.title.clone() }
    }
}
