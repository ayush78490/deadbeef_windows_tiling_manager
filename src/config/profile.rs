//! Template / profile management.
//!
//! ```text
//! %APPDATA%\deadbeef\
//!   templates\<name>\     read-only, shipped inside the binary, never edited
//!   profiles\active\      the live profile (watched, hot-reloaded)
//!   profiles\<name>\      user snapshots
//!   backups\              automatic copies of `active` taken before it is replaced
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub const DEFAULT_TEMPLATE: &str = "nerdy-deadbeef";
const ACTIVE: &str = "active";
const ORIGIN_FILE: &str = ".origin";
const MAX_BACKUPS: usize = 10;

/// Templates compiled into the binary: (name, [(file, contents)]).
pub const TEMPLATES: &[(&str, &[(&str, &str)])] = &[(
    "nerdy-deadbeef",
    &[
        ("deadbeef.conf", include_str!("../../templates/nerdy-deadbeef/deadbeef.conf")),
        ("style.conf", include_str!("../../templates/nerdy-deadbeef/style.conf")),
        ("launcher.ini", include_str!("../../templates/nerdy-deadbeef/launcher.ini")),
        ("deadbeef.lua", include_str!("../../templates/nerdy-deadbeef/deadbeef.lua")),
    ],
)];

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    /// `%APPDATA%\deadbeef` (FOLDERID_RoamingAppData). `DEADBEEF_HOME` overrides it.
    pub fn discover() -> Self {
        if let Ok(home) = std::env::var("DEADBEEF_HOME") {
            return Self { root: PathBuf::from(home) };
        }
        Self { root: crate::win::roaming_appdata().join("deadbeef") }
    }
    pub fn templates(&self) -> PathBuf {
        self.root.join("templates")
    }
    pub fn template(&self, name: &str) -> PathBuf {
        self.templates().join(name)
    }
    pub fn profiles(&self) -> PathBuf {
        self.root.join("profiles")
    }
    pub fn profile(&self, name: &str) -> PathBuf {
        self.profiles().join(name)
    }
    pub fn active(&self) -> PathBuf {
        self.profile(ACTIVE)
    }
    pub fn active_conf(&self) -> PathBuf {
        self.active().join("deadbeef.conf")
    }
    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }
    pub fn log_file(&self) -> PathBuf {
        self.root.join("deadbeef.log")
    }
}

fn is_template(name: &str) -> bool {
    TEMPLATES.iter().any(|(n, _)| *n == name)
}

/// Writes the embedded templates to disk (only when missing or outdated) and
/// marks them read-only. Creates the active profile from the default template
/// on first launch. Returns true when this was a first launch.
pub fn bootstrap(paths: &Paths) -> io::Result<bool> {
    for (name, files) in TEMPLATES {
        let dir = paths.template(name);
        fs::create_dir_all(&dir)?;
        for (file, contents) in *files {
            let path = dir.join(file);
            if fs::read_to_string(&path).ok().as_deref() != Some(*contents) {
                set_readonly(&path, false);
                fs::write(&path, contents)?;
            }
            set_readonly(&path, true);
        }
    }
    fs::create_dir_all(paths.profiles())?;

    let first = !paths.active_conf().exists();
    if first {
        fs::create_dir_all(paths.active())?;
        copy_contents(&paths.template(DEFAULT_TEMPLATE), &paths.active())?;
        fs::write(paths.active().join(ORIGIN_FILE), DEFAULT_TEMPLATE)?;
    }
    Ok(first)
}

#[derive(Debug, Serialize)]
pub struct ProfileList {
    pub templates: Vec<String>,
    pub profiles: Vec<String>,
    pub active_origin: Option<String>,
}

pub fn list(paths: &Paths) -> ProfileList {
    let mut profiles: Vec<String> = fs::read_dir(paths.profiles())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != ACTIVE && !n.starts_with('.'))
        .collect();
    profiles.sort();
    ProfileList {
        templates: TEMPLATES.iter().map(|(n, _)| n.to_string()).collect(),
        profiles,
        active_origin: origin(paths),
    }
}

pub fn origin(paths: &Paths) -> Option<String> {
    fs::read_to_string(paths.active().join(ORIGIN_FILE)).ok().map(|s| s.trim().to_string())
}

pub fn format_list(l: &ProfileList) -> String {
    let mut s = String::from("templates (read-only):\n");
    for t in &l.templates {
        s += &format!("  {t}{}\n", if l.active_origin.as_deref() == Some(t) { "  *" } else { "" });
    }
    s += "profiles:\n";
    if l.profiles.is_empty() {
        s += "  (none — create one with `dbctl profile save <name>`)\n";
    }
    for p in &l.profiles {
        s += &format!("  {p}{}\n", if l.active_origin.as_deref() == Some(p) { "  *" } else { "" });
    }
    s += &format!("active: based on {}", l.active_origin.as_deref().unwrap_or("(unknown)"));
    s
}

/// Resets the active profile to a template.
pub fn apply(paths: &Paths, template: &str) -> Result<String, String> {
    if !is_template(template) {
        return Err(format!("'{template}' is not a template (try `dbctl profile list`)"));
    }
    let backup = backup_active(paths).map_err(|e| format!("backup failed: {e}"))?;
    replace_active(paths, &paths.template(template), template).map_err(|e| e.to_string())?;
    Ok(format!("active profile reset to template '{template}' (previous copy saved to {})", backup.display()))
}

/// Snapshots the active profile as `profiles\<name>`.
pub fn save(paths: &Paths, name: &str) -> Result<String, String> {
    validate_name(name)?;
    if is_template(name) {
        return Err(format!("'{name}' is a template name; pick another"));
    }
    let dst = paths.profile(name);
    if dst.exists() {
        clear_dir(&dst).map_err(|e| e.to_string())?;
    } else {
        fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
    }
    copy_contents(&paths.active(), &dst).map_err(|e| e.to_string())?;
    // A saved profile remembers its own name as origin once switched to.
    let _ = fs::write(dst.join(ORIGIN_FILE), name);
    Ok(format!("saved active profile as '{name}'"))
}

/// Replaces the active profile with a saved profile (or a template).
pub fn switch(paths: &Paths, name: &str) -> Result<String, String> {
    if is_template(name) {
        return apply(paths, name);
    }
    validate_name(name)?;
    let src = paths.profile(name);
    if !src.join("deadbeef.conf").exists() {
        return Err(format!("no saved profile '{name}' (try `dbctl profile list`)"));
    }
    let backup = backup_active(paths).map_err(|e| format!("backup failed: {e}"))?;
    replace_active(paths, &src, name).map_err(|e| e.to_string())?;
    Ok(format!("switched to profile '{name}' (previous copy saved to {})", backup.display()))
}

fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        return Err(format!("invalid profile name '{name}' (letters, digits, - _ . only)"));
    }
    if name.eq_ignore_ascii_case(ACTIVE) {
        return Err("'active' is reserved".into());
    }
    Ok(())
}

fn replace_active(paths: &Paths, src: &Path, origin: &str) -> io::Result<()> {
    let active = paths.active();
    fs::create_dir_all(&active)?;
    // Clear in place (rather than delete + rename) so the file watcher's
    // directory handle stays valid.
    clear_dir(&active)?;
    copy_contents(src, &active)?;
    fs::write(active.join(ORIGIN_FILE), origin)
}

fn backup_active(paths: &Paths) -> io::Result<PathBuf> {
    let t = crate::win::local_time();
    let dst = paths.backups().join(format!(
        "active-{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    ));
    fs::create_dir_all(&dst)?;
    copy_contents(&paths.active(), &dst)?;

    let mut backups: Vec<PathBuf> =
        fs::read_dir(paths.backups())?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    backups.sort();
    while backups.len() > MAX_BACKUPS {
        let _ = fs::remove_dir_all(backups.remove(0));
    }
    Ok(dst)
}

fn clear_dir(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            set_readonly(&path, false);
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Recursively copies the contents of `src` into `dst`. Copies are always
/// writable, even when the source (a template) is read-only.
fn copy_contents(src: &Path, dst: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            fs::create_dir_all(&to)?;
            copy_contents(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
            set_readonly(&to, false);
        }
    }
    Ok(())
}

fn set_readonly(path: &Path, ro: bool) {
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        if perms.readonly() != ro {
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(ro);
            let _ = fs::set_permissions(path, perms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!("deadbeef-profile-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Paths { root }
    }

    fn cleanup(p: &Paths) {
        // Templates are read-only; clear the flag so the temp tree can be removed.
        for (name, files) in TEMPLATES {
            for (f, _) in *files {
                set_readonly(&p.template(name).join(f), false);
            }
        }
        let _ = fs::remove_dir_all(&p.root);
    }

    #[test]
    fn lifecycle() {
        let p = temp_paths("life");
        assert!(bootstrap(&p).unwrap());
        assert!(!bootstrap(&p).unwrap());
        let tpl = p.template(DEFAULT_TEMPLATE).join("deadbeef.conf");
        assert!(fs::metadata(&tpl).unwrap().permissions().readonly());
        assert!(!fs::metadata(p.active_conf()).unwrap().permissions().readonly());

        fs::write(p.active_conf(), "# edited\n").unwrap();
        save(&p, "mine").unwrap();
        apply(&p, DEFAULT_TEMPLATE).unwrap();
        assert_ne!(fs::read_to_string(p.active_conf()).unwrap(), "# edited\n");
        // Template untouched by edits to active.
        assert_eq!(fs::read_to_string(&tpl).unwrap(), TEMPLATES[0].1[0].1);

        switch(&p, "mine").unwrap();
        assert_eq!(fs::read_to_string(p.active_conf()).unwrap(), "# edited\n");
        assert_eq!(origin(&p).as_deref(), Some("mine"));

        let l = list(&p);
        assert_eq!(l.profiles, vec!["mine".to_string()]);
        assert!(save(&p, "active").is_err());
        assert!(save(&p, "..\\evil").is_err());
        assert!(save(&p, DEFAULT_TEMPLATE).is_err());
        assert!(switch(&p, "nope").is_err());
        cleanup(&p);
    }

    #[test]
    fn template_parses_cleanly() {
        let p = temp_paths("parse");
        bootstrap(&p).unwrap();
        // The template sources style.conf via %APPDATA%; point a scratch env at our temp root.
        let (cfg, _) = crate::config::load_with_style_override(&p);
        assert!(cfg.errors.is_empty(), "template errors: {:#?}", cfg.errors);
        assert!(cfg.binds.len() > 30);
        assert_eq!(cfg.general.active_border.colors.len(), 2);
        assert_eq!(cfg.decoration.rounding, 10);
        cleanup(&p);
    }
}
