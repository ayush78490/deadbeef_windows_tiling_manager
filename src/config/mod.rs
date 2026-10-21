pub mod launcher;
pub mod parser;
pub mod profile;
pub mod types;

use std::collections::HashMap;

pub use launcher::Launcher;
pub use profile::Paths;
pub use types::Config;

/// Loads the active profile: launcher.ini first (its palette becomes
/// `$launcher_*` variables), then deadbeef.conf and everything it sources.
pub fn load(paths: &Paths) -> (Config, Launcher) {
    let active = paths.active();
    let mut errors = Vec::new();
    let launcher_path = active.join("launcher.ini");
    let launcher = if launcher_path.exists() {
        Launcher::load(&launcher_path).unwrap_or_else(|e| {
            errors.push(e);
            Launcher::default()
        })
    } else {
        Launcher::default()
    };
    let vars: HashMap<String, String> = launcher.vars().into_iter().collect();
    let doc = parser::parse_file(&paths.active_conf(), vars);
    let mut cfg = Config::from_document(doc);
    cfg.errors.splice(0..0, errors);
    // Relative script paths are relative to the profile, not the cwd.
    for s in &mut cfg.scripts {
        if s.is_relative() {
            *s = active.join(&*s);
        }
    }
    (cfg, launcher)
}

/// Test helper: the template sources `%APPDATA%\deadbeef\profiles\active\style.conf`;
/// rewrite that to the temp root so tests never touch the real profile.
#[cfg(test)]
pub fn load_with_style_override(paths: &Paths) -> (Config, Launcher) {
    let conf = std::fs::read_to_string(paths.active_conf()).unwrap();
    let conf = conf.replace("%APPDATA%\\deadbeef", &paths.root.display().to_string());
    std::fs::write(paths.active_conf(), conf).unwrap();
    load(paths)
}
