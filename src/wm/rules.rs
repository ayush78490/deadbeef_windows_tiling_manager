//! `windowrule = ACTION, MATCHER[, MATCHER...]`
//!
//! Matchers: `exe:REGEX` (case-insensitive), `class:REGEX`, `title:REGEX`.
//! All matchers on a rule must match. Later rules override earlier ones.

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dim {
    Px(i32),
    Pct(f32),
}

impl Dim {
    pub fn parse(s: &str) -> Result<Dim, String> {
        let s = s.trim();
        if let Some(p) = s.strip_suffix('%') {
            let v: f32 = p.trim().parse().map_err(|_| format!("invalid percentage '{s}'"))?;
            return Ok(Dim::Pct(v / 100.0));
        }
        s.parse().map(Dim::Px).map_err(|_| format!("invalid size '{s}' (px or %)"))
    }
    pub fn resolve(self, total: i32) -> i32 {
        match self {
            Dim::Px(v) => v,
            Dim::Pct(f) => (total as f32 * f).round() as i32,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Float,
    Tile,
    Center,
    Pin,
    Fullscreen,
    Maximize,
    NoBorder,
    Ignore,
    Workspace(u32, bool),
    Monitor(usize),
    Opacity(f32, Option<f32>),
    Size(Dim, Dim),
    Move(Dim, Dim),
}

#[derive(Debug, Clone)]
pub enum Field {
    Exe,
    Class,
    Title,
}

#[derive(Debug, Clone)]
pub struct Matcher {
    pub field: Field,
    pub re: Regex,
}

#[derive(Debug, Clone)]
pub struct WindowRule {
    pub action: Action,
    pub matchers: Vec<Matcher>,
}

/// What a window looks like to the rule engine.
#[derive(Debug, Clone, Default)]
pub struct WindowInfo {
    pub exe: String,
    pub class: String,
    pub title: String,
}

/// Merged outcome of every matching rule (and Lua `windowopen` hooks).
#[derive(Debug, Clone, Default)]
pub struct Effects {
    pub float: Option<bool>,
    pub center: bool,
    pub pin: bool,
    pub fullscreen: bool,
    pub maximize: bool,
    pub no_border: bool,
    pub ignore: bool,
    pub workspace: Option<(u32, bool)>,
    pub monitor: Option<usize>,
    pub opacity: Option<(f32, Option<f32>)>,
    pub size: Option<(Dim, Dim)>,
    pub position: Option<(Dim, Dim)>,
}

impl Effects {
    pub fn apply(&mut self, a: &Action) {
        match a {
            Action::Float => self.float = Some(true),
            Action::Tile => self.float = Some(false),
            Action::Center => self.center = true,
            Action::Pin => self.pin = true,
            Action::Fullscreen => self.fullscreen = true,
            Action::Maximize => self.maximize = true,
            Action::NoBorder => self.no_border = true,
            Action::Ignore => self.ignore = true,
            Action::Workspace(n, s) => self.workspace = Some((*n, *s)),
            Action::Monitor(m) => self.monitor = Some(*m),
            Action::Opacity(a, i) => self.opacity = Some((*a, *i)),
            Action::Size(w, h) => self.size = Some((*w, *h)),
            Action::Move(x, y) => self.position = Some((*x, *y)),
        }
    }
}

pub fn parse_rule(v: &str) -> Result<WindowRule, String> {
    let mut parts = v.split(',');
    let action_s = parts.next().unwrap_or_default().trim();
    let action = parse_action(action_s)?;
    let mut matchers = Vec::new();
    // Regexes may themselves contain commas; re-join pieces that do not
    // start with a known field prefix onto the previous matcher.
    let mut raw_matchers: Vec<String> = Vec::new();
    for p in parts {
        let is_new = ["exe:", "class:", "title:"].iter().any(|f| p.trim_start().starts_with(f));
        match raw_matchers.last_mut() {
            Some(last) if !is_new => {
                last.push(',');
                last.push_str(p);
            }
            _ => raw_matchers.push(p.trim_start().to_string()),
        }
    }
    for m in raw_matchers {
        let (field, pattern) = m.split_once(':').ok_or_else(|| format!("invalid matcher '{m}'"))?;
        let field = match field {
            "exe" => Field::Exe,
            "class" => Field::Class,
            "title" => Field::Title,
            o => return Err(format!("unknown matcher '{o}:' (exe: | class: | title:)")),
        };
        let pattern = pattern.trim();
        let re = if matches!(field, Field::Exe) {
            Regex::new(&format!("(?i){pattern}"))
        } else {
            Regex::new(pattern)
        }
        .map_err(|e| format!("invalid regex '{pattern}': {e}"))?;
        matchers.push(Matcher { field, re });
    }
    if matchers.is_empty() {
        return Err("windowrule needs at least one matcher (exe:, class:, title:)".into());
    }
    Ok(WindowRule { action, matchers })
}

fn parse_action(s: &str) -> Result<Action, String> {
    let mut words = s.split_whitespace();
    let name = words.next().ok_or("missing rule action")?;
    let args: Vec<&str> = words.collect();
    let need = |n: usize| -> Result<(), String> {
        if args.len() < n {
            Err(format!("'{name}' needs {n} argument(s)"))
        } else {
            Ok(())
        }
    };
    Ok(match name {
        "float" => Action::Float,
        "tile" => Action::Tile,
        "center" => Action::Center,
        "pin" => Action::Pin,
        "fullscreen" => Action::Fullscreen,
        "maximize" => Action::Maximize,
        "noborder" => Action::NoBorder,
        "ignore" | "unmanage" => Action::Ignore,
        "workspace" => {
            need(1)?;
            let n: u32 = args[0].parse().map_err(|_| format!("invalid workspace '{}'", args[0]))?;
            if n == 0 {
                return Err("workspace ids start at 1".into());
            }
            Action::Workspace(n, args.get(1) == Some(&"silent"))
        }
        "monitor" => {
            need(1)?;
            Action::Monitor(args[0].parse().map_err(|_| format!("invalid monitor '{}'", args[0]))?)
        }
        "opacity" => {
            need(1)?;
            let a: f32 = args[0].parse().map_err(|_| format!("invalid opacity '{}'", args[0]))?;
            let i = args.get(1).map(|v| v.parse::<f32>()).transpose().map_err(|_| "invalid inactive opacity")?;
            Action::Opacity(a.clamp(0.0, 1.0), i.map(|v| v.clamp(0.0, 1.0)))
        }
        "size" => {
            need(2)?;
            Action::Size(Dim::parse(args[0])?, Dim::parse(args[1])?)
        }
        "move" => {
            need(2)?;
            Action::Move(Dim::parse(args[0])?, Dim::parse(args[1])?)
        }
        o => return Err(format!("unknown window rule action '{o}'")),
    })
}

impl WindowRule {
    pub fn matches(&self, w: &WindowInfo) -> bool {
        self.matchers.iter().all(|m| {
            let s = match m.field {
                Field::Exe => &w.exe,
                Field::Class => &w.class,
                Field::Title => &w.title,
            };
            m.re.is_match(s)
        })
    }
}

pub fn evaluate(rules: &[WindowRule], w: &WindowInfo) -> Effects {
    let mut fx = Effects::default();
    for r in rules.iter().filter(|r| r.matches(w)) {
        fx.apply(&r.action);
    }
    fx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(exe: &str, class: &str, title: &str) -> WindowInfo {
        WindowInfo { exe: exe.into(), class: class.into(), title: title.into() }
    }

    #[test]
    fn matching() {
        let rules = vec![
            parse_rule(r"float, exe:^(calc\.exe|CalculatorApp\.exe)$").unwrap(),
            parse_rule("workspace 4 silent, exe:^(steam\\.exe)$").unwrap(),
            parse_rule("size 50% 600, exe:^(steam\\.exe)$, title:^(Friends, List)$").unwrap(),
        ];
        let fx = evaluate(&rules, &info("CALC.EXE", "x", "Calculator"));
        assert_eq!(fx.float, Some(true));
        let fx = evaluate(&rules, &info("steam.exe", "x", "Friends, List"));
        assert_eq!(fx.workspace, Some((4, true)));
        assert_eq!(fx.size, Some((Dim::Pct(0.5), Dim::Px(600))));
        let fx = evaluate(&rules, &info("steam.exe", "x", "Store"));
        assert!(fx.size.is_none());
    }

    #[test]
    fn rejects_wayland_style() {
        assert!(parse_rule("float, initialClass:^(kitty)$").is_err());
        assert!(parse_rule("float").is_err());
        assert!(parse_rule("wiggle, exe:x").is_err());
    }
}
