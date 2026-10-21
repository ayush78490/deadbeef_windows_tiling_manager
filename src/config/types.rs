//! Typed configuration built from parsed [`Entry`]s.

use std::collections::HashMap;
use std::path::PathBuf;

use super::parser::Document;
use crate::wm::rules::{self, WindowRule};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// Win32 COLORREF layout: 0x00BBGGRR.
    pub fn colorref(self) -> u32 {
        self.r as u32 | (self.g as u32) << 8 | (self.b as u32) << 16
    }
    pub fn hex(self) -> String {
        format!("{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub colors: Vec<Rgba>,
    pub angle_deg: f32,
}

impl Gradient {
    pub fn solid(c: Rgba) -> Self {
        Self { colors: vec![c], angle_deg: 0.0 }
    }
    pub fn primary(&self) -> Rgba {
        self.colors[0]
    }
    pub fn to_config_string(&self) -> String {
        let mut s: Vec<String> = self.colors.iter().map(|c| format!("rgba({})", c.hex())).collect();
        if self.colors.len() > 1 {
            s.push(format!("{}deg", self.angle_deg));
        }
        s.join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    Dwindle,
    Master,
}

impl LayoutKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dwindle" => Ok(Self::Dwindle),
            "master" => Ok(Self::Master),
            o => Err(format!("unknown layout '{o}' (dwindle | master)")),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Dwindle => "dwindle",
            Self::Master => "master",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tristate {
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    Mica,
    Acrylic,
    Tabbed,
}

#[derive(Debug, Clone)]
pub struct General {
    pub gaps_in: i32,
    pub gaps_out: i32,
    pub border_size: i32,
    pub active_border: Gradient,
    pub inactive_border: Gradient,
    pub no_border_on_floating: bool,
    pub border_overlay: Tristate,
    pub layout: LayoutKind,
}

#[derive(Debug, Clone)]
pub struct Decoration {
    pub rounding: i32,
    pub active_opacity: f32,
    pub inactive_opacity: f32,
    pub blur_enabled: bool,
    pub backdrop: Backdrop,
}

#[derive(Debug, Clone)]
pub struct AnimEntry {
    pub enabled: bool,
    pub speed: f32,
    pub curve: String,
    pub style: String,
}

#[derive(Debug, Clone)]
pub struct Animations {
    pub enabled: bool,
    pub beziers: HashMap<String, [f32; 4]>,
    pub entries: HashMap<String, AnimEntry>,
}

impl Animations {
    /// Resolves an animation by name, falling back through Hyprland's tree
    /// (`windowsMove` → `windows` → `global`).
    pub fn get(&self, name: &str) -> Option<&AnimEntry> {
        if !self.enabled {
            return None;
        }
        let chain: &[&str] = match name {
            "windowsIn" | "windowsOut" | "windowsMove" => &[name, "windows", "global"],
            "borderangle" => &[name, "border", "global"],
            _ => &[name, "global"],
        };
        chain.iter().find_map(|n| self.entries.get(*n)).filter(|e| e.enabled && e.speed > 0.0)
    }
}

#[derive(Debug, Clone)]
pub struct Dwindle {
    pub default_split_ratio: f32,
    pub spiral: bool,
}

#[derive(Debug, Clone)]
pub struct Master {
    pub mfact: f32,
    pub new_on_top: bool,
    pub orientation: Orientation,
}

#[derive(Debug, Clone)]
pub struct Input {
    pub hotkey_fallback: bool,
    /// 0 = click to focus, 1 = focus follows the mouse (Hyprland default).
    pub follow_mouse: u8,
}

#[derive(Debug, Clone)]
pub struct Misc {
    pub float_dialogs: bool,
    pub float_unresizable: bool,
    pub notify_errors: bool,
}

pub const BAR_MODULES: &[&str] = &[
    "launcher", "workspaces", "apps", "title", "clock", "cpu", "memory", "temperature", "volume", "wifi",
    "bluetooth", "battery", "wallpaper", "power",
];

#[derive(Debug, Clone, PartialEq)]
pub struct BarConfig {
    pub enabled: bool,
    pub height: i32,
    pub all_monitors: bool,
    pub hide_taskbar: bool,
    /// Empty = system UI font.
    pub font: String,
    pub font_size: f32,
    pub background: Rgba,
    pub foreground: Rgba,
    pub accent: Rgba,
    pub dim: Rgba,
    pub warning: Rgba,
    pub critical: Rgba,
    pub clock_format: String,
    pub modules_left: Vec<String>,
    pub modules_center: Vec<String>,
    pub modules_right: Vec<String>,
}

impl Default for BarConfig {
    fn default() -> Self {
        let m = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        Self {
            enabled: false,
            height: 34,
            all_monitors: true,
            hide_taskbar: false,
            font: String::new(),
            font_size: 13.0,
            background: rgba(0x2e, 0x34, 0x40, 0xe6),
            foreground: rgba(0xd8, 0xde, 0xe9, 0xff),
            accent: rgba(0x88, 0xc0, 0xd0, 0xff),
            dim: rgba(0x4c, 0x56, 0x6a, 0xff),
            warning: rgba(0xeb, 0xcb, 0x8b, 0xff),
            critical: rgba(0xbf, 0x61, 0x6a, 0xff),
            clock_format: "%a %d %b  %H:%M".into(),
            modules_left: m(&["launcher", "workspaces", "apps"]),
            modules_center: m(&["clock"]),
            modules_right: m(&["cpu", "memory", "temperature", "volume", "wifi", "bluetooth", "battery", "power"]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallpaperConfig {
    /// grow | fade | wipe | none
    pub transition: String,
    /// Seconds.
    pub duration: f32,
    pub bezier: String,
    pub query: String,
    pub sorting: String,
    pub top_range: String,
    pub categories: String,
    pub purity: String,
    /// Minimum resolution, e.g. 1920x1080; empty = primary monitor size.
    pub atleast: String,
    pub apikey: String,
    /// Empty = %APPDATA%\deadbeef\wallpapers.
    pub directory: PathBuf,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            transition: "grow".into(),
            duration: 1.0,
            bezier: "default".into(),
            query: String::new(),
            sorting: "toplist".into(),
            top_range: "1M".into(),
            categories: "110".into(),
            purity: "100".into(),
            atleast: String::new(),
            apikey: String::new(),
            directory: PathBuf::new(),
        }
    }
}

fn modules(v: &str) -> Result<Vec<String>, String> {
    let list: Vec<String> =
        v.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(str::to_string).collect();
    for m in &list {
        if !BAR_MODULES.contains(&m.as_str()) {
            return Err(format!("unknown bar module '{m}' (one of: {})", BAR_MODULES.join(" ")));
        }
    }
    Ok(list)
}

fn binary_flags(v: &str, key: &str) -> Result<String, String> {
    if v.len() == 3 && v.chars().all(|c| c == '0' || c == '1') {
        Ok(v.to_string())
    } else {
        Err(format!("{key} must be three 0/1 flags, e.g. 110"))
    }
}

#[derive(Debug, Clone)]
pub struct Bind {
    pub mods: u32,
    pub vk: u32,
    pub combo: String,
    pub dispatcher: String,
    pub arg: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MonitorMode {
    Preferred,
    Mode { width: u32, height: u32, refresh: Option<u32> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum MonitorPos {
    Auto,
    At(i32, i32),
}

#[derive(Debug, Clone)]
pub struct MonitorRule {
    pub index: usize,
    pub mode: MonitorMode,
    pub position: MonitorPos,
    pub scale: f32,
    /// top, bottom, left, right
    pub reserved: [i32; 4],
    pub disabled: bool,
}

impl MonitorRule {
    fn new(index: usize) -> Self {
        Self { index, mode: MonitorMode::Preferred, position: MonitorPos::Auto, scale: 1.0, reserved: [0; 4], disabled: false }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceRule {
    pub id: u32,
    pub monitor: Option<usize>,
    pub layout: Option<LayoutKind>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub decoration: Decoration,
    pub animations: Animations,
    pub dwindle: Dwindle,
    pub master: Master,
    pub input: Input,
    pub misc: Misc,
    pub bar: BarConfig,
    pub wallpaper: WallpaperConfig,
    pub binds: Vec<Bind>,
    pub rules: Vec<WindowRule>,
    pub exec_once: Vec<String>,
    pub exec: Vec<String>,
    pub env: Vec<(String, String)>,
    pub monitors: Vec<MonitorRule>,
    pub workspaces: Vec<WorkspaceRule>,
    pub scripts: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub errors: Vec<String>,
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba {
    Rgba { r, g, b, a }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General {
                gaps_in: 5,
                gaps_out: 20,
                border_size: 1,
                active_border: Gradient::solid(rgba(0x33, 0xcc, 0xff, 0xee)),
                inactive_border: Gradient::solid(rgba(0x59, 0x59, 0x59, 0xaa)),
                no_border_on_floating: false,
                border_overlay: Tristate::Auto,
                layout: LayoutKind::Dwindle,
            },
            decoration: Decoration {
                rounding: 0,
                active_opacity: 1.0,
                inactive_opacity: 1.0,
                blur_enabled: false,
                backdrop: Backdrop::Acrylic,
            },
            animations: Animations { enabled: true, beziers: HashMap::new(), entries: HashMap::new() },
            dwindle: Dwindle { default_split_ratio: 1.0, spiral: false },
            master: Master { mfact: 0.55, new_on_top: false, orientation: Orientation::Left },
            input: Input { hotkey_fallback: true, follow_mouse: 1 },
            misc: Misc { float_dialogs: true, float_unresizable: true, notify_errors: true },
            bar: BarConfig::default(),
            wallpaper: WallpaperConfig::default(),
            binds: Vec::new(),
            rules: Vec::new(),
            exec_once: Vec::new(),
            exec: Vec::new(),
            env: Vec::new(),
            monitors: Vec::new(),
            workspaces: Vec::new(),
            scripts: Vec::new(),
            files: Vec::new(),
            errors: Vec::new(),
        }
    }
}

impl Config {
    pub fn from_document(doc: Document) -> Self {
        let mut cfg = Config { errors: doc.errors, files: doc.files, ..Config::default() };
        for entry in &doc.entries {
            if let Err(e) = cfg.set(&entry.key, &entry.value) {
                cfg.errors.push(format!("{}: {e}", entry.location()));
            }
        }
        cfg
    }

    pub fn monitor_rule(&self, index: usize) -> Option<&MonitorRule> {
        self.monitors.iter().find(|m| m.index == index)
    }

    pub fn workspace_rule(&self, id: u32) -> Option<&WorkspaceRule> {
        self.workspaces.iter().find(|w| w.id == id)
    }

    /// Applies one keyword. Used for both file entries and `dbctl keyword`.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let v = value.trim();
        match key {
            "general:gaps_in" => self.general.gaps_in = int(v)?.max(0),
            "general:gaps_out" => self.general.gaps_out = int(v)?.max(0),
            "general:border_size" => self.general.border_size = int(v)?.clamp(0, 32),
            "general:col.active_border" => self.general.active_border = parse_gradient(v)?,
            "general:col.inactive_border" => self.general.inactive_border = parse_gradient(v)?,
            "general:no_border_on_floating" => self.general.no_border_on_floating = boolean(v)?,
            "general:border_overlay" => {
                self.general.border_overlay = match v.to_ascii_lowercase().as_str() {
                    "auto" => Tristate::Auto,
                    _ if boolean(v)? => Tristate::On,
                    _ => Tristate::Off,
                }
            }
            "general:layout" => self.general.layout = LayoutKind::parse(v)?,

            "decoration:rounding" => self.decoration.rounding = int(v)?.max(0),
            "decoration:active_opacity" => self.decoration.active_opacity = float(v)?.clamp(0.0, 1.0),
            "decoration:inactive_opacity" => self.decoration.inactive_opacity = float(v)?.clamp(0.0, 1.0),
            "decoration:blur:enabled" => self.decoration.blur_enabled = boolean(v)?,
            "decoration:blur:backdrop" => {
                self.decoration.backdrop = match v.to_ascii_lowercase().as_str() {
                    "mica" => Backdrop::Mica,
                    "acrylic" => Backdrop::Acrylic,
                    "tabbed" => Backdrop::Tabbed,
                    o => return Err(format!("unknown backdrop '{o}' (mica | acrylic | tabbed)")),
                }
            }

            "animations:enabled" => self.animations.enabled = boolean(v)?,
            "animations:bezier" => {
                let parts = split_args(v, 5);
                if parts.len() != 5 {
                    return Err("bezier = NAME, X0, Y0, X1, Y1".into());
                }
                let mut p = [0f32; 4];
                for (i, s) in parts[1..].iter().enumerate() {
                    p[i] = float(s)?;
                }
                if !(0.0..=1.0).contains(&p[0]) || !(0.0..=1.0).contains(&p[2]) {
                    return Err("bezier X values must be within 0..1".into());
                }
                self.animations.beziers.insert(parts[0].to_string(), p);
            }
            "animations:animation" => {
                let parts = split_args(v, 5);
                if parts.len() < 2 {
                    return Err("animation = NAME, ON/OFF, SPEED, CURVE[, STYLE]".into());
                }
                let name = parts[0].to_string();
                const KNOWN: &[&str] = &[
                    "global", "windows", "windowsIn", "windowsOut", "windowsMove", "border", "borderangle",
                    "fade", "fadeIn", "fadeOut", "workspaces",
                ];
                if !KNOWN.contains(&name.as_str()) {
                    return Err(format!("unknown animation '{name}'"));
                }
                let enabled = boolean(parts[1])?;
                let speed = if parts.len() > 2 { float(parts[2])? } else { 0.0 };
                if enabled && parts.len() < 4 {
                    return Err("animation = NAME, ON/OFF, SPEED, CURVE[, STYLE]".into());
                }
                let curve = parts.get(3).map(|s| s.to_string()).unwrap_or_else(|| "default".into());
                if curve != "default" && !self.animations.beziers.contains_key(&curve) {
                    return Err(format!("undefined bezier '{curve}'"));
                }
                let style = parts.get(4).map(|s| s.to_string()).unwrap_or_default();
                self.animations.entries.insert(name, AnimEntry { enabled, speed, curve, style });
            }

            "dwindle:default_split_ratio" => self.dwindle.default_split_ratio = float(v)?.clamp(0.1, 1.9),
            "dwindle:spiral" => self.dwindle.spiral = boolean(v)?,
            // Accepted for Hyprland familiarity; the list-based dwindle always preserves order.
            "dwindle:preserve_split" | "dwindle:pseudotile" | "dwindle:force_split" => {
                boolean(v).or_else(|_| int(v).map(|_| true))?;
            }

            "master:mfact" => self.master.mfact = float(v)?.clamp(0.05, 0.95),
            "master:new_status" => {
                self.master.new_on_top = match v {
                    "master" => true,
                    "slave" | "inherit" => false,
                    o => return Err(format!("unknown new_status '{o}' (master | slave)")),
                }
            }
            "master:new_on_top" => self.master.new_on_top = boolean(v)?,
            "master:orientation" => {
                self.master.orientation = match v {
                    "left" => Orientation::Left,
                    "right" => Orientation::Right,
                    "top" => Orientation::Top,
                    "bottom" => Orientation::Bottom,
                    o => return Err(format!("unknown orientation '{o}'")),
                }
            }

            "input:hotkey_fallback" => self.input.hotkey_fallback = boolean(v)?,
            "input:follow_mouse" => {
                self.input.follow_mouse = match int(v)? {
                    n @ 0..=1 => n as u8,
                    n => return Err(format!("input:follow_mouse must be 0 or 1, got {n}")),
                }
            }

            "misc:float_dialogs" => self.misc.float_dialogs = boolean(v)?,
            "misc:float_unresizable" => self.misc.float_unresizable = boolean(v)?,
            "misc:notify_errors" => self.misc.notify_errors = boolean(v)?,

            "bar:enabled" => self.bar.enabled = boolean(v)?,
            "bar:height" => self.bar.height = int(v)?.clamp(20, 80),
            "bar:monitors" => {
                self.bar.all_monitors = match v {
                    "all" => true,
                    "primary" => false,
                    o => return Err(format!("bar:monitors must be all | primary, got '{o}'")),
                }
            }
            "bar:hide_taskbar" => self.bar.hide_taskbar = boolean(v)?,
            "bar:font" => self.bar.font = v.to_string(),
            "bar:font_size" => self.bar.font_size = float(v)?.clamp(6.0, 32.0),
            "bar:col.background" => self.bar.background = parse_color(v)?,
            "bar:col.foreground" => self.bar.foreground = parse_color(v)?,
            "bar:col.accent" => self.bar.accent = parse_color(v)?,
            "bar:col.dim" => self.bar.dim = parse_color(v)?,
            "bar:col.warning" => self.bar.warning = parse_color(v)?,
            "bar:col.critical" => self.bar.critical = parse_color(v)?,
            "bar:clock_format" => self.bar.clock_format = v.to_string(),
            "bar:modules-left" => self.bar.modules_left = modules(v)?,
            "bar:modules-center" => self.bar.modules_center = modules(v)?,
            "bar:modules-right" => self.bar.modules_right = modules(v)?,

            "wallpaper:transition" => {
                if !matches!(v, "grow" | "fade" | "wipe" | "none") {
                    return Err(format!("wallpaper:transition must be grow | fade | wipe | none, got '{v}'"));
                }
                self.wallpaper.transition = v.to_string();
            }
            "wallpaper:duration" => self.wallpaper.duration = float(v)?.clamp(0.0, 10.0),
            "wallpaper:bezier" => {
                if v != "default" && !self.animations.beziers.contains_key(v) {
                    return Err(format!("undefined bezier '{v}'"));
                }
                self.wallpaper.bezier = v.to_string();
            }
            "wallpaper:query" => self.wallpaper.query = v.to_string(),
            "wallpaper:sorting" => {
                const S: &[&str] = &["toplist", "date_added", "relevance", "random", "views", "favorites", "hot"];
                if !S.contains(&v) {
                    return Err(format!("wallpaper:sorting must be one of {}", S.join(" | ")));
                }
                self.wallpaper.sorting = v.to_string();
            }
            "wallpaper:top_range" => {
                const R: &[&str] = &["1d", "3d", "1w", "1M", "3M", "6M", "1y"];
                if !R.contains(&v) {
                    return Err(format!("wallpaper:top_range must be one of {}", R.join(" | ")));
                }
                self.wallpaper.top_range = v.to_string();
            }
            "wallpaper:categories" => self.wallpaper.categories = binary_flags(v, key)?,
            "wallpaper:purity" => self.wallpaper.purity = binary_flags(v, key)?,
            "wallpaper:atleast" => {
                let ok = v.is_empty()
                    || v.split_once('x').is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok());
                if !ok {
                    return Err("wallpaper:atleast must look like 1920x1080".into());
                }
                self.wallpaper.atleast = v.to_string();
            }
            "wallpaper:apikey" => self.wallpaper.apikey = v.to_string(),
            "wallpaper:directory" => self.wallpaper.directory = PathBuf::from(super::parser::expand_env(v)),

            "bind" => self.binds.push(parse_bind(v)?),
            "windowrule" => self.rules.push(rules::parse_rule(v)?),
            "exec-once" => self.exec_once.push(v.to_string()),
            "exec" => self.exec.push(v.to_string()),
            "env" => {
                let (name, val) = v.split_once(',').ok_or("env = NAME, VALUE")?;
                self.env.push((name.trim().to_string(), val.trim().to_string()));
            }
            "monitor" => self.parse_monitor(v)?,
            "workspace" => self.workspaces.push(parse_workspace(v)?),
            "script" => self.scripts.push(PathBuf::from(super::parser::expand_env(v))),

            k if k.starts_with("monitor:") || k.contains("kb_") || k.contains("touchpad") => {
                return Err(format!("'{k}' is not supported on Windows (the OS handles input devices)"));
            }
            k => return Err(format!("unknown option '{k}'")),
        }
        Ok(())
    }

    /// Current value of a scalar option, formatted like the config file.
    pub fn get(&self, key: &str) -> Option<String> {
        Some(match key {
            "general:gaps_in" => self.general.gaps_in.to_string(),
            "general:gaps_out" => self.general.gaps_out.to_string(),
            "general:border_size" => self.general.border_size.to_string(),
            "general:col.active_border" => self.general.active_border.to_config_string(),
            "general:col.inactive_border" => self.general.inactive_border.to_config_string(),
            "general:no_border_on_floating" => self.general.no_border_on_floating.to_string(),
            "general:border_overlay" => format!("{:?}", self.general.border_overlay).to_lowercase(),
            "general:layout" => self.general.layout.name().into(),
            "decoration:rounding" => self.decoration.rounding.to_string(),
            "decoration:active_opacity" => self.decoration.active_opacity.to_string(),
            "decoration:inactive_opacity" => self.decoration.inactive_opacity.to_string(),
            "decoration:blur:enabled" => self.decoration.blur_enabled.to_string(),
            "decoration:blur:backdrop" => format!("{:?}", self.decoration.backdrop).to_lowercase(),
            "animations:enabled" => self.animations.enabled.to_string(),
            "dwindle:default_split_ratio" => self.dwindle.default_split_ratio.to_string(),
            "dwindle:spiral" => self.dwindle.spiral.to_string(),
            "master:mfact" => self.master.mfact.to_string(),
            "master:orientation" => format!("{:?}", self.master.orientation).to_lowercase(),
            "input:hotkey_fallback" => self.input.hotkey_fallback.to_string(),
            "input:follow_mouse" => self.input.follow_mouse.to_string(),
            "misc:float_dialogs" => self.misc.float_dialogs.to_string(),
            "misc:float_unresizable" => self.misc.float_unresizable.to_string(),
            "misc:notify_errors" => self.misc.notify_errors.to_string(),
            "bar:enabled" => self.bar.enabled.to_string(),
            "bar:height" => self.bar.height.to_string(),
            "bar:hide_taskbar" => self.bar.hide_taskbar.to_string(),
            "wallpaper:transition" => self.wallpaper.transition.clone(),
            "wallpaper:duration" => self.wallpaper.duration.to_string(),
            "wallpaper:query" => self.wallpaper.query.clone(),
            _ => return None,
        })
    }

    fn parse_monitor(&mut self, v: &str) -> Result<(), String> {
        let parts = split_args(v, 8);
        if parts.len() < 2 {
            return Err("monitor = INDEX, RESOLUTION, POSITION, SCALE".into());
        }
        if parts[0].starts_with("desc:") || parts[0].chars().any(|c| c.is_ascii_alphabetic()) {
            return Err(format!(
                "monitor '{}' — use a numeric index (0 = primary, see `dbctl monitors`)",
                parts[0]
            ));
        }
        let index: usize = parts[0].parse().map_err(|_| format!("invalid monitor index '{}'", parts[0]))?;
        let pos = match self.monitors.iter().position(|m| m.index == index) {
            Some(p) => p,
            None => {
                self.monitors.push(MonitorRule::new(index));
                self.monitors.len() - 1
            }
        };
        let rule = &mut self.monitors[pos];
        match parts[1] {
            "disable" | "disabled" => rule.disabled = true,
            "addreserved" => {
                if parts.len() != 6 {
                    return Err("monitor = INDEX, addreserved, TOP, BOTTOM, LEFT, RIGHT".into());
                }
                for i in 0..4 {
                    rule.reserved[i] = int(parts[i + 2])?;
                }
            }
            mode => {
                rule.mode = parse_mode(mode)?;
                if let Some(p) = parts.get(2) {
                    rule.position = match *p {
                        "auto" | "auto-right" => MonitorPos::Auto,
                        p => {
                            let (x, y) = p.split_once('x').ok_or_else(|| format!("invalid position '{p}' (XxY | auto)"))?;
                            MonitorPos::At(int(x)?, int(y)?)
                        }
                    };
                }
                if let Some(s) = parts.get(3) {
                    rule.scale = if *s == "auto" { 1.0 } else { float(s)? };
                }
            }
        }
        Ok(())
    }
}

fn parse_mode(s: &str) -> Result<MonitorMode, String> {
    match s {
        "preferred" | "highres" | "highrr" => Ok(MonitorMode::Preferred),
        _ => {
            let (res, hz) = match s.split_once('@') {
                Some((r, h)) => (r, Some(float(h)?.round() as u32)),
                None => (s, None),
            };
            let (w, h) = res.split_once('x').ok_or_else(|| format!("invalid resolution '{s}' (preferred | WxH[@HZ])"))?;
            Ok(MonitorMode::Mode {
                width: w.trim().parse().map_err(|_| format!("invalid width in '{s}'"))?,
                height: h.trim().parse().map_err(|_| format!("invalid height in '{s}'"))?,
                refresh: hz,
            })
        }
    }
}

fn parse_workspace(v: &str) -> Result<WorkspaceRule, String> {
    let parts = split_args(v, 8);
    let id: u32 = parts[0].parse().map_err(|_| format!("invalid workspace id '{}'", parts[0]))?;
    if id == 0 {
        return Err("workspace ids start at 1".into());
    }
    let mut rule = WorkspaceRule { id, monitor: None, layout: None };
    for p in &parts[1..] {
        let (k, val) = p.split_once(':').ok_or_else(|| format!("expected key:value, got '{p}'"))?;
        match k.trim() {
            "monitor" => rule.monitor = Some(val.trim().parse().map_err(|_| format!("invalid monitor index '{val}'"))?),
            "layout" => rule.layout = Some(LayoutKind::parse(val)?),
            "default" | "persistent" => {}
            o => return Err(format!("unknown workspace rule '{o}'")),
        }
    }
    Ok(rule)
}

/// Splits on commas into at most `n` trimmed parts (the last keeps any commas).
pub fn split_args(v: &str, n: usize) -> Vec<&str> {
    v.splitn(n, ',').map(str::trim).collect()
}

pub fn int(s: &str) -> Result<i32, String> {
    s.trim().parse().map_err(|_| format!("expected an integer, got '{s}'"))
}

pub fn float(s: &str) -> Result<f32, String> {
    s.trim().parse().map_err(|_| format!("expected a number, got '{s}'"))
}

pub fn boolean(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        o => Err(format!("expected a boolean, got '{o}'")),
    }
}

fn hex_byte(s: &str) -> Result<u8, String> {
    u8::from_str_radix(s, 16).map_err(|_| format!("invalid hex '{s}'"))
}

/// `rgba(RRGGBBAA)`, `rgba(r, g, b, a)`, `rgb(RRGGBB)`, `rgb(r, g, b)`, `0xAARRGGBB`.
pub fn parse_color(s: &str) -> Result<Rgba, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        if hex.len() != 8 {
            return Err(format!("expected 0xAARRGGBB, got '{s}'"));
        }
        return Ok(rgba(hex_byte(&hex[2..4])?, hex_byte(&hex[4..6])?, hex_byte(&hex[6..8])?, hex_byte(&hex[0..2])?));
    }
    let (body, has_alpha) = if let Some(b) = lower.strip_prefix("rgba(") {
        (b, true)
    } else if let Some(b) = lower.strip_prefix("rgb(") {
        (b, false)
    } else {
        return Err(format!("invalid colour '{s}'"));
    };
    let body = body.strip_suffix(')').ok_or_else(|| format!("missing ')' in '{s}'"))?.trim();
    if body.contains(',') {
        let parts: Vec<&str> = body.split(',').map(str::trim).collect();
        let want = if has_alpha { 4 } else { 3 };
        if parts.len() != want {
            return Err(format!("expected {want} components in '{s}'"));
        }
        let ch = |p: &str| p.parse::<u8>().map_err(|_| format!("invalid channel '{p}' in '{s}'"));
        let a = if has_alpha {
            let f = float(parts[3])?;
            (f.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            255
        };
        return Ok(rgba(ch(parts[0])?, ch(parts[1])?, ch(parts[2])?, a));
    }
    match (body.len(), has_alpha) {
        (8, true) => Ok(rgba(hex_byte(&body[0..2])?, hex_byte(&body[2..4])?, hex_byte(&body[4..6])?, hex_byte(&body[6..8])?)),
        (6, _) => Ok(rgba(hex_byte(&body[0..2])?, hex_byte(&body[2..4])?, hex_byte(&body[4..6])?, 255)),
        _ => Err(format!("invalid colour '{s}'")),
    }
}

/// One or more colours followed by an optional `NNdeg` angle.
pub fn parse_gradient(s: &str) -> Result<Gradient, String> {
    let mut colors = Vec::new();
    let mut angle = 0.0;
    let mut depth = 0;
    let mut tok = String::new();
    let mut tokens = Vec::new();
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c.is_whitespace() && depth == 0 {
            if !tok.is_empty() {
                tokens.push(std::mem::take(&mut tok));
            }
        } else {
            tok.push(c);
        }
    }
    if !tok.is_empty() {
        tokens.push(tok);
    }
    for t in tokens {
        if let Some(deg) = t.strip_suffix("deg") {
            angle = float(deg)?;
        } else {
            colors.push(parse_color(&t)?);
        }
    }
    if colors.is_empty() {
        return Err(format!("no colours in '{s}'"));
    }
    if colors.len() > 10 {
        return Err("at most 10 gradient colours".into());
    }
    Ok(Gradient { colors, angle_deg: angle })
}

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

pub fn parse_mods(s: &str) -> Result<u32, String> {
    let mut mods = 0;
    for m in s.split(|c: char| c.is_whitespace() || c == '_' || c == '+').filter(|m| !m.is_empty()) {
        mods |= match m.to_ascii_uppercase().as_str() {
            "SUPER" | "WIN" | "MOD4" | "LOGO" => MOD_WIN,
            "SHIFT" => MOD_SHIFT,
            "CTRL" | "CONTROL" => MOD_CONTROL,
            "ALT" | "MOD1" => MOD_ALT,
            o => return Err(format!("unknown modifier '{o}' (SUPER | ALT | CTRL | SHIFT)")),
        };
    }
    Ok(mods)
}

pub fn mods_to_string(mods: u32) -> String {
    let mut v = Vec::new();
    if mods & MOD_WIN != 0 {
        v.push("SUPER");
    }
    if mods & MOD_CONTROL != 0 {
        v.push("CTRL");
    }
    if mods & MOD_ALT != 0 {
        v.push("ALT");
    }
    if mods & MOD_SHIFT != 0 {
        v.push("SHIFT");
    }
    v.join(" ")
}

/// Maps a key name to a Win32 virtual-key code.
pub fn parse_key(name: &str) -> Result<u32, String> {
    let n = name.trim();
    if let Some(code) = n.strip_prefix("code:") {
        return code.parse().map_err(|_| format!("invalid key code '{code}'"));
    }
    if n.len() == 1 {
        let c = n.chars().next().unwrap().to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Ok(c as u32);
        }
    }
    let lower = n.to_ascii_lowercase();
    if let Some(f) = lower.strip_prefix('f') {
        if let Ok(i) = f.parse::<u32>() {
            if (1..=24).contains(&i) {
                return Ok(0x70 + i - 1);
            }
        }
    }
    if let Some(d) = lower.strip_prefix("numpad").or_else(|| lower.strip_prefix("kp_")) {
        if let Ok(i) = d.parse::<u32>() {
            if i <= 9 {
                return Ok(0x60 + i);
            }
        }
    }
    Ok(match lower.as_str() {
        "return" | "enter" => 0x0D,
        "space" => 0x20,
        "tab" => 0x09,
        "escape" | "esc" => 0x1B,
        "backspace" => 0x08,
        "delete" => 0x2E,
        "insert" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "prior" | "pageup" | "page_up" => 0x21,
        "next" | "pagedown" | "page_down" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "print" | "printscreen" => 0x2C,
        "pause" => 0x13,
        "comma" => 0xBC,
        "period" => 0xBE,
        "minus" => 0xBD,
        "equal" | "plus" => 0xBB,
        "grave" => 0xC0,
        "slash" => 0xBF,
        "backslash" => 0xDC,
        "semicolon" => 0xBA,
        "apostrophe" => 0xDE,
        "bracketleft" => 0xDB,
        "bracketright" => 0xDD,
        "xf86audiomute" | "volumemute" => 0xAD,
        "xf86audiolowervolume" | "volumedown" => 0xAE,
        "xf86audioraisevolume" | "volumeup" => 0xAF,
        "xf86audionext" | "medianext" => 0xB0,
        "xf86audioprev" | "mediaprev" => 0xB1,
        "xf86audiostop" | "mediastop" => 0xB2,
        "xf86audioplay" | "xf86audiopause" | "mediaplay" => 0xB3,
        _ => return Err(format!("unknown key '{n}'")),
    })
}

pub fn parse_bind(v: &str) -> Result<Bind, String> {
    let parts = split_args(v, 4);
    if parts.len() < 3 {
        return Err("bind = MODS, KEY, DISPATCHER[, ARGS]".into());
    }
    let mods = parse_mods(parts[0])?;
    let vk = parse_key(parts[1])?;
    let dispatcher = parts[2].to_string();
    if !crate::dispatch::is_known(&dispatcher) {
        return Err(format!("unknown dispatcher '{dispatcher}'"));
    }
    let combo = if mods == 0 { parts[1].to_string() } else { format!("{} + {}", mods_to_string(mods), parts[1]) };
    Ok(Bind { mods, vk, combo, dispatcher, arg: parts.get(3).map(|s| s.to_string()).unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("rgba(88c0d0ff)").unwrap(), rgba(0x88, 0xc0, 0xd0, 0xff));
        assert_eq!(parse_color("rgb(2e3440)").unwrap(), rgba(0x2e, 0x34, 0x40, 255));
        assert_eq!(parse_color("0xff382D2E").unwrap(), rgba(0x38, 0x2d, 0x2e, 0xff));
        assert_eq!(parse_color("rgba(10, 20, 30, 0.5)").unwrap(), rgba(10, 20, 30, 128));
        assert_eq!(rgba(0x88, 0xc0, 0xd0, 0xff).colorref(), 0x00d0c088);
        let g = parse_gradient("rgba(88c0d0ff) rgba(81a1c1ff) 45deg").unwrap();
        assert_eq!(g.colors.len(), 2);
        assert_eq!(g.angle_deg, 45.0);
        assert!(parse_color("#fff").is_err());
    }

    #[test]
    fn binds() {
        let b = parse_bind("SUPER SHIFT, 1, movetoworkspace, 1").unwrap();
        assert_eq!(b.mods, MOD_WIN | MOD_SHIFT);
        assert_eq!(b.vk, '1' as u32);
        let b = parse_bind("SUPER, Print, exec, explorer.exe ms-screenclip:, extra").unwrap();
        assert_eq!(b.arg, "explorer.exe ms-screenclip:, extra");
        assert!(parse_bind("HYPER, X, exec, wt").is_err());
        assert!(parse_bind("SUPER, X, frobnicate").is_err());
    }

    #[test]
    fn monitors_reject_linux_edid() {
        let mut c = Config::default();
        assert!(c.set("monitor", "desc:BOE 0x0BCA, preferred, 0x0, 1").is_err());
        c.set("monitor", "0, preferred, 0x0, 1").unwrap();
        c.set("monitor", "1, 2560x1440@144, 1920x0, 1").unwrap();
        c.set("monitor", "0, addreserved, 32, 0, 0, 0").unwrap();
        assert_eq!(c.monitor_rule(0).unwrap().reserved, [32, 0, 0, 0]);
        assert_eq!(
            c.monitor_rule(1).unwrap().mode,
            MonitorMode::Mode { width: 2560, height: 1440, refresh: Some(144) }
        );
    }

    #[test]
    fn linux_only_keys_rejected() {
        let mut c = Config::default();
        assert!(c.set("input:kb_layout", "us").is_err());
        assert!(c.set("input:touchpad:natural_scroll", "false").is_err());
    }
}
