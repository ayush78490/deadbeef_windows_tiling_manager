//! `launcher.ini` — launcher command plus the shared launcher palette.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct Launcher {
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl Launcher {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut current = "main".to_string();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = name.trim().to_string();
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| format!("line {}: expected key = value", i + 1))?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(v);
            sections.entry(current.clone()).or_default().insert(k.trim().to_string(), v.to_string());
        }
        Ok(Self { sections })
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.get(key).map(String::as_str)
    }

    pub fn command(&self) -> Option<String> {
        self.get("main", "command").map(super::parser::expand_env)
    }

    /// Config variables: `$launcher_<key>` for every [colors] entry and
    /// `$launcher_border_<key>` for [border].
    pub fn vars(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (section, prefix) in [("colors", "launcher_"), ("border", "launcher_border_"), ("main", "launcher_main_")] {
            if let Some(map) = self.sections.get(section) {
                for (k, v) in map {
                    out.push((format!("{prefix}{}", k.replace('-', "_")), v.clone()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_template() {
        let l = Launcher::parse(include_str!("../../templates/nerdy-deadbeef/launcher.ini")).unwrap();
        assert_eq!(l.get("colors", "background"), Some("2e3440ff"));
        assert_eq!(l.get("main", "prompt"), Some("> "));
        assert!(l.vars().iter().any(|(k, v)| k == "launcher_selection_text" && v == "e5e9f0ff"));
    }
}
