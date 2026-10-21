//! Lua (5.4, via mlua) macros and window rules.
//!
//! Scripts never touch window-manager state directly: `deadbeef.dispatch`
//! and `deadbeef.keyword` queue actions that the WM runs after the Lua call
//! returns, and `deadbeef.clients` / `deadbeef.option` read a snapshot taken
//! just before the call. Every call runs under an instruction-count hook with
//! a wall-clock budget, so a runaway script cannot freeze window management.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{Function, HookTriggers, Lua, Table, Value, VmState};
use serde::Serialize;

use crate::config::Launcher;
use crate::wm::rules::{Dim, Effects};

const BUDGET: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub enum Pending {
    Dispatch(String, String),
    Keyword(String, String),
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ClientView {
    pub hwnd: i64,
    pub exe: String,
    pub class: String,
    pub title: String,
    pub floating: bool,
    pub monitor: usize,
}

#[derive(Default)]
struct Shared {
    pending: Vec<Pending>,
    clients: Vec<ClientView>,
    options: HashMap<String, String>,
}

pub struct Script {
    lua: Lua,
    shared: Rc<RefCell<Shared>>,
    deadline: Rc<Cell<Instant>>,
}

impl Script {
    pub fn load(files: &[PathBuf], launcher: &Launcher) -> (Option<Script>, Vec<String>) {
        let mut errors = Vec::new();
        let existing: Vec<&PathBuf> = files
            .iter()
            .filter(|f| {
                let ok = f.exists();
                if !ok {
                    errors.push(format!("script {}: file not found", f.display()));
                }
                ok
            })
            .collect();
        if existing.is_empty() {
            return (None, errors);
        }
        let script = match Script::new(launcher) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("lua init: {e}"));
                return (None, errors);
            }
        };
        for f in existing {
            let r = std::fs::read_to_string(f)
                .map_err(|e| e.to_string())
                .and_then(|src| script.guarded(|lua| lua.load(&src).set_name(f.display().to_string()).exec()).map_err(|e| e.to_string()));
            if let Err(e) = r {
                errors.push(format!("script {}: {e}", f.display()));
            }
        }
        (Some(script), errors)
    }

    fn new(launcher: &Launcher) -> mlua::Result<Script> {
        let lua = Lua::new();
        let shared = Rc::new(RefCell::new(Shared::default()));
        let deadline = Rc::new(Cell::new(Instant::now() + BUDGET));

        let api = lua.create_table()?;
        let s = shared.clone();
        api.set(
            "dispatch",
            lua.create_function(move |_, (name, arg): (String, Option<Value>)| {
                let arg = match arg {
                    Some(Value::String(s)) => s.to_str()?.to_string(),
                    Some(Value::Integer(i)) => i.to_string(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                s.borrow_mut().pending.push(Pending::Dispatch(name, arg));
                Ok(())
            })?,
        )?;
        let s = shared.clone();
        api.set(
            "keyword",
            lua.create_function(move |_, (key, value): (String, Value)| {
                let v = match value {
                    Value::String(s) => s.to_str()?.to_string(),
                    Value::Integer(i) => i.to_string(),
                    Value::Number(n) => n.to_string(),
                    Value::Boolean(b) => b.to_string(),
                    _ => return Err(mlua::Error::runtime("keyword value must be a string, number or boolean")),
                };
                let mut sh = s.borrow_mut();
                sh.options.insert(key.clone(), v.clone());
                sh.pending.push(Pending::Keyword(key, v));
                Ok(())
            })?,
        )?;
        let s = shared.clone();
        api.set(
            "option",
            lua.create_function(move |lua, key: String| {
                let sh = s.borrow();
                Ok(match sh.options.get(&key) {
                    Some(v) => match v.parse::<i64>() {
                        Ok(i) => Value::Integer(i),
                        Err(_) => match v.parse::<f64>() {
                            Ok(f) => Value::Number(f),
                            Err(_) => Value::String(lua.create_string(v)?),
                        },
                    },
                    None => Value::Nil,
                })
            })?,
        )?;
        let s = shared.clone();
        api.set(
            "clients",
            lua.create_function(move |lua, ()| {
                let t = lua.create_table()?;
                for (i, c) in s.borrow().clients.iter().enumerate() {
                    t.set(i + 1, client_table(lua, c)?)?;
                }
                Ok(t)
            })?,
        )?;
        api.set(
            "log",
            lua.create_function(|_, msg: String| {
                crate::log_info!("[lua] {msg}");
                Ok(())
            })?,
        )?;
        let handlers = lua.create_table()?;
        api.set("_handlers", handlers)?;
        api.set(
            "on",
            lua.create_function(|lua, (event, f): (String, Function)| {
                if !matches!(event.as_str(), "windowopen" | "focus") {
                    return Err(mlua::Error::runtime(format!("unknown event '{event}' (windowopen | focus)")));
                }
                let api: Table = lua.globals().get("deadbeef")?;
                let handlers: Table = api.get("_handlers")?;
                let list: Table = match handlers.get::<Option<Table>>(event.as_str())? {
                    Some(t) => t,
                    None => {
                        let t = lua.create_table()?;
                        handlers.set(event.as_str(), &t)?;
                        t
                    }
                };
                list.push(f)?;
                Ok(())
            })?,
        )?;

        let launcher_t = lua.create_table()?;
        for (section, map) in &launcher.sections {
            let t = lua.create_table()?;
            for (k, v) in map {
                t.set(k.as_str(), v.as_str())?;
            }
            launcher_t.set(section.as_str(), t)?;
        }
        api.set("launcher", launcher_t)?;
        lua.globals().set("deadbeef", api)?;

        let d = deadline.clone();
        lua.set_hook(HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
            if Instant::now() > d.get() {
                Err(mlua::Error::runtime("script exceeded its 250ms budget"))
            } else {
                Ok(VmState::Continue)
            }
        });

        Ok(Script { lua, shared, deadline })
    }

    fn guarded<R>(&self, f: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        self.deadline.set(Instant::now() + BUDGET);
        f(&self.lua)
    }

    /// Refreshes the read-only snapshot scripts see.
    pub fn set_snapshot(&self, clients: Vec<ClientView>, options: HashMap<String, String>) {
        let mut s = self.shared.borrow_mut();
        s.clients = clients;
        s.options = options;
    }

    /// Actions queued by the last call(s).
    pub fn take_pending(&self) -> Vec<Pending> {
        std::mem::take(&mut self.shared.borrow_mut().pending)
    }

    /// Calls a global function by name (`bind = ..., lua, NAME`).
    pub fn call(&self, name: &str) -> Result<(), String> {
        self.guarded(|lua| {
            let f: Function = lua
                .globals()
                .get::<Option<Function>>(name)?
                .ok_or_else(|| mlua::Error::runtime(format!("no Lua function named '{name}'")))?;
            f.call::<()>(())
        })
        .map_err(|e| e.to_string())
    }

    fn handlers(&self, event: &str) -> Vec<Function> {
        let r: mlua::Result<Vec<Function>> = (|| {
            let api: Table = self.lua.globals().get("deadbeef")?;
            let handlers: Table = api.get("_handlers")?;
            match handlers.get::<Option<Table>>(event)? {
                Some(list) => list.sequence_values::<Function>().collect(),
                None => Ok(Vec::new()),
            }
        })();
        r.unwrap_or_default()
    }

    /// Runs `windowopen` handlers; merges every returned rule table into `fx`.
    pub fn window_open(&self, client: &ClientView, fx: &mut Effects) {
        for f in self.handlers("windowopen") {
            let r = self.guarded(|lua| {
                let v: Value = f.call(client_table(lua, client)?)?;
                if let Value::Table(t) = v {
                    merge_effects(&t, fx)?;
                }
                Ok(())
            });
            if let Err(e) = r {
                crate::log_warn!("[lua] windowopen handler: {e}");
            }
        }
    }

    pub fn focus(&self, client: &ClientView) {
        for f in self.handlers("focus") {
            if let Err(e) = self.guarded(|lua| f.call::<()>(client_table(lua, client)?)) {
                crate::log_warn!("[lua] focus handler: {e}");
            }
        }
    }
}

fn client_table(lua: &Lua, c: &ClientView) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("hwnd", c.hwnd)?;
    t.set("exe", c.exe.as_str())?;
    t.set("class", c.class.as_str())?;
    t.set("title", c.title.as_str())?;
    t.set("floating", c.floating)?;
    t.set("monitor", c.monitor)?;
    Ok(t)
}

fn dim(v: Value) -> mlua::Result<Dim> {
    match v {
        Value::Integer(i) => Ok(Dim::Px(i as i32)),
        Value::Number(n) => Ok(Dim::Px(n as i32)),
        Value::String(s) => Dim::parse(&s.to_str()?).map_err(mlua::Error::runtime),
        _ => Err(mlua::Error::runtime("size/move values must be numbers or \"N%\" strings")),
    }
}

fn merge_effects(t: &Table, fx: &mut Effects) -> mlua::Result<()> {
    if let Some(b) = t.get::<Option<bool>>("float")? {
        fx.float = Some(b);
    }
    if t.get::<Option<bool>>("tile")?.unwrap_or(false) {
        fx.float = Some(false);
    }
    let flag = |k: &str| -> mlua::Result<bool> { Ok(t.get::<Option<bool>>(k)?.unwrap_or(false)) };
    fx.center |= flag("center")?;
    fx.pin |= flag("pin")?;
    fx.fullscreen |= flag("fullscreen")?;
    fx.maximize |= flag("maximize")?;
    fx.no_border |= flag("noborder")?;
    fx.ignore |= flag("ignore")?;
    if let Some(ws) = t.get::<Option<u32>>("workspace")? {
        fx.workspace = Some((ws.max(1), flag("silent")?));
    }
    if let Some(m) = t.get::<Option<usize>>("monitor")? {
        fx.monitor = Some(m);
    }
    if let Some(o) = t.get::<Option<f32>>("opacity")? {
        fx.opacity = Some((o.clamp(0.0, 1.0), None));
    }
    for (key, slot) in [("size", &mut fx.size), ("move", &mut fx.position)] {
        if let Some(st) = t.get::<Option<Table>>(key)? {
            *slot = Some((dim(st.get(1)?)?, dim(st.get(2)?)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(src: &str) -> Script {
        let dir = std::env::temp_dir().join(format!("deadbeef-lua-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.lua", src.len()));
        std::fs::write(&path, src).unwrap();
        let (s, errs) = Script::load(std::slice::from_ref(&path), &Launcher::default());
        let _ = std::fs::remove_file(&path);
        assert!(errs.is_empty(), "{errs:?}");
        s.unwrap()
    }

    #[test]
    fn template_script_loads_and_toggles_gaps() {
        let s = script(include_str!("../../templates/nerdy-deadbeef/deadbeef.lua"));
        let mut opts = HashMap::new();
        opts.insert("general:gaps_in".to_string(), "5".to_string());
        opts.insert("general:gaps_out".to_string(), "10".to_string());
        s.set_snapshot(vec![], opts);
        s.call("toggle_gaps").unwrap();
        let p = s.take_pending();
        assert!(matches!(&p[0], Pending::Keyword(k, v) if k == "general:gaps_in" && v == "0"));
        s.call("toggle_gaps").unwrap();
        let p = s.take_pending();
        assert!(matches!(&p[0], Pending::Keyword(k, v) if k == "general:gaps_in" && v == "5"));

        let mut fx = Effects::default();
        s.window_open(&ClientView { exe: "SystemSettings.exe".into(), ..Default::default() }, &mut fx);
        assert_eq!(fx.float, Some(true));
        assert_eq!(fx.size, Some((Dim::Pct(0.6), Dim::Pct(0.7))));
    }

    #[test]
    fn runaway_scripts_are_stopped() {
        let s = script("function spin() while true do end end");
        let t = Instant::now();
        let err = s.call("spin").unwrap_err();
        assert!(err.contains("budget"), "{err}");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn dispatch_is_queued() {
        let s = script("function go() deadbeef.dispatch('workspace', 3) end");
        s.call("go").unwrap();
        assert!(matches!(&s.take_pending()[..], [Pending::Dispatch(n, a)] if n == "workspace" && a == "3"));
    }
}
