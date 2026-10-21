//! `dbctl` command handling (runs on the main thread).

use serde::Serialize;

use super::Response;
use crate::config::profile;
use crate::state::State;

pub const HELP: &str = "\
dbctl — control deadbeef

  reload                          reload the active profile
  dispatch <name> [args]          run a dispatcher (dbctl dispatchers for the list)
  dispatchers                     list dispatchers
  keyword <key> <value>           set an option until the next reload
  getoption <key>                 print an option
  clients [-j]                    managed windows
  activewindow [-j]               the focused window
  monitors [-j]                   monitors (index used by monitor = and workspace =)
  workspaces [-j]                 virtual desktops
  binds [-j]                      keybinds and how each was registered
  errors                          config errors from the last load
  profile list [-j]               templates and saved profiles
  profile apply <template>        reset the active profile to a template
  profile save <name>             snapshot the active profile
  profile switch <name>           replace the active profile with a snapshot
  version";

fn ok(out: impl Into<String>) -> Response {
    Response { ok: true, out: out.into() }
}

fn err(out: impl Into<String>) -> Response {
    Response { ok: false, out: out.into() }
}

fn json<T: Serialize>(v: &T) -> Response {
    ok(serde_json::to_string_pretty(v).unwrap_or_default())
}

pub fn handle(state: &mut State, args: &[String]) -> Response {
    let want_json = args.iter().any(|a| a == "-j" || a == "--json");
    let args: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "-j" && *a != "--json").collect();
    let rest = |n: usize| args.get(n..).map(|r| r.join(" ")).unwrap_or_default();

    match args.first().copied().unwrap_or("help") {
        "help" | "--help" | "-h" => ok(HELP),
        "version" => ok(format!("deadbeef {} (Windows build {})", env!("CARGO_PKG_VERSION"), crate::win::os_build())),
        "reload" => {
            let errors = state.reload();
            if errors.is_empty() {
                ok("ok")
            } else {
                ok(format!("reloaded with {} error(s):\n{}", errors.len(), errors.join("\n")))
            }
        }
        "dispatch" => {
            let Some(name) = args.get(1) else { return err("usage: dispatch <name> [args]") };
            let r = state.dispatch(name, &rest(2));
            state.update_overlay();
            match r {
                Ok(s) => ok(s),
                Err(e) => err(e),
            }
        }
        "dispatchers" => ok(crate::dispatch::DISPATCHERS
            .iter()
            .map(|(n, d)| format!("  {n:<22} {d}"))
            .collect::<Vec<_>>()
            .join("\n")),
        "keyword" => {
            let (Some(key), value) = (args.get(1), rest(2)) else { return err("usage: keyword <key> <value>") };
            match state.keyword(key, &value) {
                Ok(s) => ok(s),
                Err(e) => err(e),
            }
        }
        "getoption" => match args.get(1).and_then(|k| state.config.get(k)) {
            Some(v) => ok(v),
            None => err("unknown option"),
        },
        "clients" => {
            let c = state.clients_json();
            if want_json {
                return json(&c);
            }
            ok(c.iter()
                .map(|c| {
                    format!(
                        "{} {:<24} ws:{} mon:{} {}{}\n    {}",
                        c.hwnd,
                        c.exe,
                        c.workspace.map(|w| w.to_string()).unwrap_or("?".into()),
                        c.monitor,
                        if c.floating { "floating" } else { "tiled" },
                        if c.focused { " *focused*" } else { "" },
                        c.title
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "activewindow" => {
            let c = state.clients_json().into_iter().find(|c| c.focused);
            match (c, want_json) {
                (Some(c), true) => json(&c),
                (Some(c), false) => ok(format!("{} {} — {}", c.hwnd, c.exe, c.title)),
                (None, _) => err("no managed window is focused"),
            }
        }
        "monitors" => {
            state.monitors = crate::win::monitors();
            if want_json {
                return json(&state.monitors);
            }
            ok(state
                .monitors
                .iter()
                .map(|m| {
                    format!(
                        "{} {}{} {}x{}@{}Hz at {}x{} scale {:.2} work {}x{}+{}+{}",
                        m.index,
                        m.name,
                        if m.primary { " (primary)" } else { "" },
                        m.rect.w,
                        m.rect.h,
                        m.refresh_hz,
                        m.rect.x,
                        m.rect.y,
                        m.scale,
                        m.work.w,
                        m.work.h,
                        m.work.x,
                        m.work.y
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "workspaces" => {
            #[derive(Serialize)]
            struct Ws {
                id: u32,
                current: bool,
                windows: usize,
            }
            let cur = state.workspaces.current();
            let clients = state.clients_json();
            let list: Vec<Ws> = (1..=state.workspaces.count())
                .map(|id| Ws {
                    id,
                    current: cur == Some(id),
                    windows: clients.iter().filter(|c| c.workspace == Some(id)).count(),
                })
                .collect();
            if want_json {
                return json(&list);
            }
            ok(list
                .iter()
                .map(|w| format!("{}{} — {} window(s)", w.id, if w.current { " (current)" } else { "" }, w.windows))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "binds" => {
            let st = &state.hotkeys.status;
            if want_json {
                return json(st);
            }
            ok(st
                .iter()
                .map(|b| {
                    format!(
                        "{:<22} {:<16} {} {}{}",
                        b.combo,
                        b.method,
                        b.dispatcher,
                        b.arg,
                        match (&b.error, b.method) {
                            (Some(e), "none") => format!("   !! {e}"),
                            _ => String::new(),
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "errors" => {
            if state.config.errors.is_empty() {
                ok("no errors")
            } else {
                ok(state.config.errors.join("\n"))
            }
        }
        "profile" => handle_profile(state, &args[1..], want_json),
        other => err(format!("unknown command '{other}'\n\n{HELP}")),
    }
}

fn handle_profile(state: &mut State, args: &[&str], want_json: bool) -> Response {
    let paths = state.paths.clone();
    let result = match (args.first().copied(), args.get(1).copied()) {
        (Some("list") | None, _) => {
            let l = profile::list(&paths);
            return if want_json { json(&l) } else { ok(profile::format_list(&l)) };
        }
        (Some("current"), _) => return ok(profile::origin(&paths).unwrap_or_else(|| "(unknown)".into())),
        (Some("apply"), Some(name)) => profile::apply(&paths, name),
        (Some("save"), Some(name)) => return to_response(profile::save(&paths, name)),
        (Some("switch"), Some(name)) => profile::switch(&paths, name),
        (Some(cmd @ ("apply" | "save" | "switch")), None) => return err(format!("usage: profile {cmd} <name>")),
        (Some(other), _) => return err(format!("unknown profile command '{other}'")),
    };
    match result {
        Ok(msg) => {
            // Reload now rather than waiting for the file watcher's debounce.
            let errors = state.reload();
            let suffix = if errors.is_empty() { String::new() } else { format!("\n{} config error(s)", errors.len()) };
            ok(format!("{msg}{suffix}"))
        }
        Err(e) => err(e),
    }
}

fn to_response(r: Result<String, String>) -> Response {
    match r {
        Ok(s) => ok(s),
        Err(e) => err(e),
    }
}
