//! The options the settings app edits, and how they are saved.
//!
//! Everything the app changes is written to `settings.conf` in the active
//! profile, which `deadbeef.conf` sources last so its values win over
//! `style.conf`. The user's own files are never rewritten beyond appending
//! that one `source` line, and deleting `settings.conf` drops every change.

use std::fs;
use std::io;
use std::path::Path;

use crate::config::parser;
use crate::config::profile::{Paths, DEFAULT_TEMPLATE};
use crate::config::types::{Backdrop, Config, Gradient, LayoutKind, Orientation, Rgba, Tristate, BAR_MODULES};

pub const SETTINGS_FILE: &str = "settings.conf";
const SOURCE_LINE: &str = "source = settings.conf";

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub layout: LayoutKind,
    pub gaps_in: i32,
    pub gaps_out: i32,
    pub split_ratio: f32,
    pub spiral: bool,
    pub mfact: f32,
    pub new_master: bool,
    pub orientation: Orientation,

    pub border_size: i32,
    pub active_border: Gradient,
    pub inactive_border: Rgba,
    pub border_overlay: Tristate,
    pub rounding: i32,
    pub active_opacity: f32,
    pub inactive_opacity: f32,
    pub blur: bool,
    pub backdrop: Backdrop,
    pub animations: bool,

    pub bar: bool,
    pub hide_taskbar: bool,
    pub bar_all_monitors: bool,
    pub bar_height: i32,
    pub bar_font_size: f32,
    pub bar_background: Rgba,
    pub bar_foreground: Rgba,
    pub bar_accent: Rgba,
    pub bar_dim: Rgba,
    pub clock_format: String,
    pub modules_left: Vec<String>,
    pub modules_center: Vec<String>,
    pub modules_right: Vec<String>,

    pub follow_mouse: bool,
    pub hotkey_fallback: bool,
    pub float_dialogs: bool,
    pub float_unresizable: bool,
    pub notify_errors: bool,
}

/// Slider-backed values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Num {
    GapsIn,
    GapsOut,
    SplitRatio,
    Mfact,
    BorderSize,
    ActiveOpacity,
    InactiveOpacity,
    BarHeight,
    BarFontSize,
}

impl Num {
    /// (min, max, step)
    pub fn range(self) -> (f32, f32, f32) {
        match self {
            Num::GapsIn => (0.0, 40.0, 1.0),
            Num::GapsOut => (0.0, 60.0, 1.0),
            Num::SplitRatio => (0.2, 1.8, 0.05),
            Num::Mfact => (0.2, 0.8, 0.01),
            Num::BorderSize => (0.0, 8.0, 1.0),
            Num::ActiveOpacity => (0.5, 1.0, 0.01),
            Num::InactiveOpacity => (0.3, 1.0, 0.01),
            Num::BarHeight => (24.0, 60.0, 1.0),
            Num::BarFontSize => (9.0, 20.0, 1.0),
        }
    }

    pub fn format(self, v: f32) -> String {
        match self {
            Num::GapsIn | Num::GapsOut | Num::BorderSize | Num::BarHeight => format!("{} px", v as i32),
            Num::BarFontSize => format!("{} pt", v as i32),
            Num::ActiveOpacity | Num::InactiveOpacity | Num::Mfact => format!("{}%", (v * 100.0).round() as i32),
            Num::SplitRatio => {
                let first = (v / 2.0 * 100.0).round() as i32;
                format!("{first} / {}", 100 - first)
            }
        }
    }
}

/// Toggle-backed values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Spiral,
    NewMaster,
    Overlay,
    Blur,
    Animations,
    Bar,
    HideTaskbar,
    FollowMouse,
    HotkeyFallback,
    FloatDialogs,
    FloatUnresizable,
    NotifyErrors,
}

/// Segmented-control values: each has a fixed list of options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Layout,
    Orientation,
    Corners,
    Backdrop,
    BarMonitors,
    Clock,
    Spacing,
}

/// Gap presets offered on the overview page: (label, gaps_in, gaps_out).
pub const SPACING: &[(&str, i32, i32)] = &[("None", 0, 0), ("Compact", 3, 6), ("Comfortable", 5, 10), ("Spacious", 10, 20)];

impl Pick {
    pub fn options(self) -> &'static [&'static str] {
        match self {
            Pick::Layout => &["Dwindle", "Master"],
            Pick::Orientation => &["Left", "Right", "Top", "Bottom"],
            Pick::Corners => &["Square", "Small", "Round"],
            Pick::Backdrop => &["Mica", "Acrylic", "Tabbed"],
            Pick::BarMonitors => &["All monitors", "Primary only"],
            Pick::Clock => &["24-hour", "12-hour"],
            Pick::Spacing => &["None", "Compact", "Comfortable", "Spacious"],
        }
    }
}

const ORIENTATIONS: [Orientation; 4] = [Orientation::Left, Orientation::Right, Orientation::Top, Orientation::Bottom];
const BACKDROPS: [Backdrop; 3] = [Backdrop::Mica, Backdrop::Acrylic, Backdrop::Tabbed];
/// `decoration:rounding` for Square / Small / Round (Windows 11 corner preferences).
const CORNERS: [i32; 3] = [0, 4, 10];

/// A colour theme: borders plus the top bar palette.
pub struct Preset {
    pub name: &'static str,
    pub active: [u32; 2],
    pub inactive: u32,
    pub bg: u32,
    pub fg: u32,
    pub accent: u32,
    pub dim: u32,
}

pub const PRESETS: &[Preset] = &[
    Preset { name: "Nord", active: [0x88c0d0ff, 0x81a1c1ff], inactive: 0x4c566aaa, bg: 0x2e3440e6, fg: 0xd8dee9ff, accent: 0x88c0d0ff, dim: 0x4c566aff },
    Preset { name: "Catppuccin", active: [0xcba6f7ff, 0x89b4faff], inactive: 0x45475aaa, bg: 0x1e1e2ee6, fg: 0xcdd6f4ff, accent: 0xcba6f7ff, dim: 0x6c7086ff },
    Preset { name: "Tokyo Night", active: [0x7aa2f7ff, 0xbb9af7ff], inactive: 0x414868aa, bg: 0x1a1b26e6, fg: 0xc0caf5ff, accent: 0x7aa2f7ff, dim: 0x565f89ff },
    Preset { name: "Gruvbox", active: [0xfabd2fff, 0xfe8019ff], inactive: 0x504945aa, bg: 0x282828e6, fg: 0xebdbb2ff, accent: 0xfabd2fff, dim: 0x665c54ff },
    Preset { name: "Dracula", active: [0xbd93f9ff, 0xff79c6ff], inactive: 0x44475aaa, bg: 0x282a36e6, fg: 0xf8f8f2ff, accent: 0xbd93f9ff, dim: 0x6272a4ff },
    Preset { name: "Rosé Pine", active: [0xebbcbaff, 0xc4a7e7ff], inactive: 0x403d52aa, bg: 0x191724e6, fg: 0xe0def4ff, accent: 0xebbcbaff, dim: 0x6e6a86ff },
    Preset { name: "Everforest", active: [0xa7c080ff, 0x83c092ff], inactive: 0x4f585eaa, bg: 0x2d353be6, fg: 0xd3c6aaff, accent: 0xa7c080ff, dim: 0x7a8478ff },
    Preset { name: "Daylight", active: [0x0078d4ff, 0x6b69d6ff], inactive: 0xb4b4b4aa, bg: 0xf3f3f3e6, fg: 0x1b1b1bff, accent: 0x0078d4ff, dim: 0x9a9a9aff },
];

/// Swatches for the border colour pickers.
pub const ACCENTS: &[u32] = &[
    0x88c0d0ff, 0x81a1c1ff, 0x5e81acff, 0x7aa2f7ff, 0x89b4faff, 0xcba6f7ff, 0xbd93f9ff, 0xff79c6ff, 0xbf616aff, 0xfe8019ff,
    0xfabd2fff, 0xebcb8bff, 0xa3be8cff, 0x8fbcbbff, 0xeceff4ff, 0x0078d4ff,
];
pub const INACTIVE: &[u32] = &[0x4c566aaa, 0x3b4252aa, 0x45475aaa, 0x595959aa, 0x808080aa, 0xb4b4b4aa, 0x00000000];

pub fn rgba(v: u32) -> Rgba {
    Rgba { r: (v >> 24) as u8, g: (v >> 16) as u8, b: (v >> 8) as u8, a: v as u8 }
}

/// Which bar section a module lives in when it is switched on.
fn home_section(module: &str) -> usize {
    match module {
        "launcher" | "workspaces" | "apps" => 0,
        "clock" | "title" => 1,
        _ => 2,
    }
}

fn fmt_f(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn snap(n: Num, v: f32) -> f32 {
    let (lo, hi, step) = n.range();
    let v = ((v - lo) / step).round() * step + lo;
    // Round away float noise so equal values compare equal.
    (v.clamp(lo, hi) * 1000.0).round() / 1000.0
}

impl Model {
    pub fn from_config(c: &Config) -> Self {
        Model {
            layout: c.general.layout,
            gaps_in: c.general.gaps_in,
            gaps_out: c.general.gaps_out,
            split_ratio: c.dwindle.default_split_ratio,
            spiral: c.dwindle.spiral,
            mfact: c.master.mfact,
            new_master: c.master.new_on_top,
            orientation: c.master.orientation,
            border_size: c.general.border_size,
            active_border: c.general.active_border.clone(),
            inactive_border: c.general.inactive_border.primary(),
            border_overlay: c.general.border_overlay,
            rounding: c.decoration.rounding,
            active_opacity: c.decoration.active_opacity,
            inactive_opacity: c.decoration.inactive_opacity,
            blur: c.decoration.blur_enabled,
            backdrop: c.decoration.backdrop,
            animations: c.animations.enabled,
            bar: c.bar.enabled,
            hide_taskbar: c.bar.hide_taskbar,
            bar_all_monitors: c.bar.all_monitors,
            bar_height: c.bar.height,
            bar_font_size: c.bar.font_size,
            bar_background: c.bar.background,
            bar_foreground: c.bar.foreground,
            bar_accent: c.bar.accent,
            bar_dim: c.bar.dim,
            clock_format: c.bar.clock_format.clone(),
            modules_left: c.bar.modules_left.clone(),
            modules_center: c.bar.modules_center.clone(),
            modules_right: c.bar.modules_right.clone(),
            follow_mouse: c.input.follow_mouse == 1,
            hotkey_fallback: c.input.hotkey_fallback,
            float_dialogs: c.misc.float_dialogs,
            float_unresizable: c.misc.float_unresizable,
            notify_errors: c.misc.notify_errors,
        }
    }

    /// What the active profile currently applies (including settings.conf).
    pub fn load(paths: &Paths) -> Self {
        Self::from_config(&crate::config::load(paths).0)
    }

    /// The defaults of the bundled template (its style.conf holds every
    /// option the app edits).
    pub fn defaults(paths: &Paths) -> Self {
        let style = paths.template(DEFAULT_TEMPLATE).join("style.conf");
        Self::from_config(&Config::from_document(parser::parse_file(&style, Default::default())))
    }

    pub fn num(&self, n: Num) -> f32 {
        match n {
            Num::GapsIn => self.gaps_in as f32,
            Num::GapsOut => self.gaps_out as f32,
            Num::SplitRatio => self.split_ratio,
            Num::Mfact => self.mfact,
            Num::BorderSize => self.border_size as f32,
            Num::ActiveOpacity => self.active_opacity,
            Num::InactiveOpacity => self.inactive_opacity,
            Num::BarHeight => self.bar_height as f32,
            Num::BarFontSize => self.bar_font_size,
        }
    }

    pub fn set_num(&mut self, n: Num, v: f32) {
        let v = snap(n, v);
        match n {
            Num::GapsIn => self.gaps_in = v as i32,
            Num::GapsOut => self.gaps_out = v as i32,
            Num::SplitRatio => self.split_ratio = v,
            Num::Mfact => self.mfact = v,
            Num::BorderSize => self.border_size = v as i32,
            Num::ActiveOpacity => self.active_opacity = v,
            Num::InactiveOpacity => self.inactive_opacity = v,
            Num::BarHeight => self.bar_height = v as i32,
            Num::BarFontSize => self.bar_font_size = v,
        }
    }

    pub fn flag(&self, f: Flag) -> bool {
        match f {
            Flag::Spiral => self.spiral,
            Flag::NewMaster => self.new_master,
            Flag::Overlay => self.border_overlay == Tristate::On,
            Flag::Blur => self.blur,
            Flag::Animations => self.animations,
            Flag::Bar => self.bar,
            Flag::HideTaskbar => self.hide_taskbar,
            Flag::FollowMouse => self.follow_mouse,
            Flag::HotkeyFallback => self.hotkey_fallback,
            Flag::FloatDialogs => self.float_dialogs,
            Flag::FloatUnresizable => self.float_unresizable,
            Flag::NotifyErrors => self.notify_errors,
        }
    }

    pub fn toggle(&mut self, f: Flag) {
        let on = !self.flag(f);
        match f {
            Flag::Spiral => self.spiral = on,
            Flag::NewMaster => self.new_master = on,
            Flag::Overlay => self.border_overlay = if on { Tristate::On } else { Tristate::Auto },
            Flag::Blur => self.blur = on,
            Flag::Animations => self.animations = on,
            Flag::Bar => self.bar = on,
            Flag::HideTaskbar => self.hide_taskbar = on,
            Flag::FollowMouse => self.follow_mouse = on,
            Flag::HotkeyFallback => self.hotkey_fallback = on,
            Flag::FloatDialogs => self.float_dialogs = on,
            Flag::FloatUnresizable => self.float_unresizable = on,
            Flag::NotifyErrors => self.notify_errors = on,
        }
    }

    /// The selected option index, or None when the value matches no option
    /// (e.g. hand-written gaps that are not one of the spacing presets).
    pub fn pick(&self, p: Pick) -> Option<usize> {
        match p {
            Pick::Layout => Some(if self.layout == LayoutKind::Master { 1 } else { 0 }),
            Pick::Orientation => ORIENTATIONS.iter().position(|o| *o == self.orientation),
            Pick::Corners => Some(match self.rounding {
                0 => 0,
                1..=4 => 1,
                _ => 2,
            }),
            Pick::Backdrop => BACKDROPS.iter().position(|b| *b == self.backdrop),
            Pick::BarMonitors => Some(if self.bar_all_monitors { 0 } else { 1 }),
            Pick::Clock => Some(if self.clock_format.contains("%I") { 1 } else { 0 }),
            Pick::Spacing => SPACING.iter().position(|(_, i, o)| *i == self.gaps_in && *o == self.gaps_out),
        }
    }

    pub fn set_pick(&mut self, p: Pick, i: usize) {
        match p {
            Pick::Layout => self.layout = if i == 1 { LayoutKind::Master } else { LayoutKind::Dwindle },
            Pick::Orientation => self.orientation = ORIENTATIONS[i.min(3)],
            Pick::Corners => {
                if self.pick(Pick::Corners) != Some(i) {
                    self.rounding = CORNERS[i.min(2)];
                }
            }
            Pick::Backdrop => self.backdrop = BACKDROPS[i.min(2)],
            Pick::BarMonitors => self.bar_all_monitors = i == 0,
            Pick::Clock => {
                let f = &self.clock_format;
                self.clock_format = if i == 1 {
                    let f = f.replace("%H", "%I");
                    if f.contains("%p") {
                        f
                    } else {
                        format!("{f} %p")
                    }
                } else {
                    f.replace(" %p", "").replace("%p", "").replace("%I", "%H")
                };
            }
            Pick::Spacing => {
                let (_, gi, go) = SPACING[i.min(SPACING.len() - 1)];
                self.gaps_in = gi;
                self.gaps_out = go;
            }
        }
    }

    pub fn apply_preset(&mut self, p: &Preset) {
        self.active_border = Gradient { colors: vec![rgba(p.active[0]), rgba(p.active[1])], angle_deg: 45.0 };
        self.inactive_border = rgba(p.inactive);
        self.bar_background = rgba(p.bg);
        self.bar_foreground = rgba(p.fg);
        self.bar_accent = rgba(p.accent);
        self.bar_dim = rgba(p.dim);
    }

    /// The preset whose palette this model uses, if any.
    pub fn preset(&self) -> Option<usize> {
        PRESETS.iter().position(|p| {
            self.active_border.primary() == rgba(p.active[0])
                && self.bar_background == rgba(p.bg)
                && self.inactive_border == rgba(p.inactive)
        })
    }

    pub fn has_module(&self, m: &str) -> bool {
        [&self.modules_left, &self.modules_center, &self.modules_right].iter().any(|l| l.iter().any(|x| x == m))
    }

    pub fn toggle_module(&mut self, m: &str) {
        if self.has_module(m) {
            for l in [&mut self.modules_left, &mut self.modules_center, &mut self.modules_right] {
                l.retain(|x| x != m);
            }
            return;
        }
        let list = match home_section(m) {
            0 => &mut self.modules_left,
            1 => &mut self.modules_center,
            _ => &mut self.modules_right,
        };
        // Keep the canonical module order inside the section.
        let rank = |x: &str| BAR_MODULES.iter().position(|b| *b == x).unwrap_or(usize::MAX);
        let at = list.iter().position(|x| rank(x) > rank(m)).unwrap_or(list.len());
        list.insert(at, m.to_string());
    }

    /// Every option as `key = value`, in the config grammar.
    pub fn entries(&self) -> Vec<(&'static str, String)> {
        let b = |v: bool| v.to_string();
        let col = |c: Rgba| format!("rgba({})", c.hex());
        vec![
            ("general:layout", self.layout.name().into()),
            ("general:gaps_in", self.gaps_in.to_string()),
            ("general:gaps_out", self.gaps_out.to_string()),
            ("general:border_size", self.border_size.to_string()),
            ("general:col.active_border", self.active_border.to_config_string()),
            ("general:col.inactive_border", col(self.inactive_border)),
            ("general:border_overlay", format!("{:?}", self.border_overlay).to_lowercase()),
            ("decoration:rounding", self.rounding.to_string()),
            ("decoration:active_opacity", fmt_f(self.active_opacity)),
            ("decoration:inactive_opacity", fmt_f(self.inactive_opacity)),
            ("decoration:blur:enabled", b(self.blur)),
            ("decoration:blur:backdrop", format!("{:?}", self.backdrop).to_lowercase()),
            ("animations:enabled", b(self.animations)),
            ("dwindle:default_split_ratio", fmt_f(self.split_ratio)),
            ("dwindle:spiral", b(self.spiral)),
            ("master:mfact", fmt_f(self.mfact)),
            ("master:new_status", if self.new_master { "master" } else { "slave" }.into()),
            ("master:orientation", format!("{:?}", self.orientation).to_lowercase()),
            ("input:follow_mouse", (self.follow_mouse as u8).to_string()),
            ("input:hotkey_fallback", b(self.hotkey_fallback)),
            ("misc:float_dialogs", b(self.float_dialogs)),
            ("misc:float_unresizable", b(self.float_unresizable)),
            ("misc:notify_errors", b(self.notify_errors)),
            ("bar:enabled", b(self.bar)),
            ("bar:hide_taskbar", b(self.hide_taskbar)),
            ("bar:monitors", if self.bar_all_monitors { "all" } else { "primary" }.into()),
            ("bar:height", self.bar_height.to_string()),
            ("bar:font_size", fmt_f(self.bar_font_size)),
            ("bar:col.background", col(self.bar_background)),
            ("bar:col.foreground", col(self.bar_foreground)),
            ("bar:col.accent", col(self.bar_accent)),
            ("bar:col.dim", col(self.bar_dim)),
            ("bar:clock_format", self.clock_format.clone()),
            ("bar:modules-left", self.modules_left.join(" ")),
            ("bar:modules-center", self.modules_center.join(" ")),
            ("bar:modules-right", self.modules_right.join(" ")),
        ]
    }

    pub fn to_conf(&self) -> String {
        let mut s = String::from(
            "# Written by deadbeef Settings — changes made here are overwritten the next\n\
             # time you press Apply. deadbeef.conf sources this file last, so these\n\
             # values win over style.conf. Delete it to go back to the profile's own values.\n\n",
        );
        for (k, v) in self.entries() {
            s += &format!("{k} = {v}\n");
        }
        s
    }

    /// Writes settings.conf and makes sure deadbeef.conf sources it.
    pub fn save(&self, paths: &Paths) -> io::Result<()> {
        fs::write(paths.active().join(SETTINGS_FILE), self.to_conf())?;
        ensure_sourced(&paths.active_conf())
    }
}

/// Appends `source = settings.conf` to deadbeef.conf unless it is already there.
pub fn ensure_sourced(conf: &Path) -> io::Result<()> {
    let text = fs::read_to_string(conf).unwrap_or_default();
    let present = text.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#')
            && l.split_once('=').is_some_and(|(k, v)| k.trim() == "source" && v.trim().ends_with(SETTINGS_FILE))
    });
    if present {
        return Ok(());
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out += &format!("\n# deadbeef Settings — keep this line last so its values win.\n{SOURCE_LINE}\n");
    fs::write(conf, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile;

    fn temp_paths(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!("deadbeef-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Paths { root }
    }

    fn cleanup(p: &Paths) {
        for (name, files) in profile::TEMPLATES {
            for (f, _) in *files {
                let path = p.template(name).join(f);
                if let Ok(meta) = fs::metadata(&path) {
                    let mut perms = meta.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = fs::set_permissions(&path, perms);
                }
            }
        }
        let _ = fs::remove_dir_all(&p.root);
    }

    #[test]
    fn round_trips_through_the_parser() {
        let p = temp_paths("roundtrip");
        profile::bootstrap(&p).unwrap();
        let (cfg, _) = crate::config::load_with_style_override(&p);
        let mut m = Model::from_config(&cfg);
        assert_eq!(m, Model::defaults(&p));

        m.set_num(Num::GapsIn, 13.4);
        m.set_num(Num::InactiveOpacity, 0.7333);
        m.set_pick(Pick::Layout, 1);
        m.set_pick(Pick::Orientation, 2);
        m.set_pick(Pick::Clock, 1);
        m.apply_preset(&PRESETS[3]);
        m.toggle(Flag::Overlay);
        m.toggle(Flag::Bar);
        m.toggle_module("title");
        m.toggle_module("cpu");
        m.save(&p).unwrap();
        // Saving twice must not add a second source line.
        m.save(&p).unwrap();
        let conf = fs::read_to_string(p.active_conf()).unwrap();
        assert_eq!(conf.matches(SOURCE_LINE).count(), 1);

        let (cfg, _) = crate::config::load(&p);
        assert!(cfg.errors.is_empty(), "{:#?}", cfg.errors);
        let back = Model::from_config(&cfg);
        assert_eq!(back, m);
        assert_eq!(back.gaps_in, 13);
        assert_eq!(back.inactive_opacity, 0.73);
        assert_eq!(back.preset(), Some(3));
        assert!(back.clock_format.contains("%I") && back.clock_format.ends_with("%p"));
        assert!(back.modules_center.contains(&"title".to_string()));
        assert!(!back.has_module("cpu"));
        cleanup(&p);
    }

    #[test]
    fn picks_and_modules() {
        let mut m = Model::from_config(&Config::default());
        m.set_pick(Pick::Spacing, 3);
        assert_eq!((m.gaps_in, m.gaps_out), (10, 20));
        assert_eq!(m.pick(Pick::Spacing), Some(3));
        m.set_num(Num::GapsIn, 7.0);
        assert_eq!(m.pick(Pick::Spacing), None);

        m.rounding = 12;
        m.set_pick(Pick::Corners, 2);
        assert_eq!(m.rounding, 12, "re-picking the same bucket keeps a custom radius");
        m.set_pick(Pick::Corners, 0);
        assert_eq!(m.rounding, 0);

        m.set_pick(Pick::Clock, 1);
        m.set_pick(Pick::Clock, 0);
        assert_eq!(m.clock_format, Config::default().bar.clock_format);

        m.modules_right = vec!["cpu".into(), "power".into()];
        m.toggle_module("volume");
        assert_eq!(m.modules_right, vec!["cpu", "volume", "power"]);
        assert_eq!(Num::SplitRatio.format(1.0), "50 / 50");
    }
}
