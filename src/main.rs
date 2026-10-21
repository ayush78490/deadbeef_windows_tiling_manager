//! deadbeef — a Hyprland-inspired tiling window manager for Windows 10/11.
//!
//! Threading model: all window-manager state lives on the main thread and is
//! only touched from its message loop. Other threads (IPC pipe server, file
//! watcher, keyboard hook, animation ticker) talk to it by posting messages
//! to the hidden main window.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod animation;
mod bar;
mod config;
mod decoration;
mod dispatch;
mod input;
mod ipc;
mod launcher;
mod layout;
mod logging;
mod script;
mod settings;
mod shell;
mod state;
mod tray;
mod ui;
mod wallpaper;
mod win;
mod wm;

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Receiver;

use notify::{RecursiveMode, Watcher};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{profile, Paths};
use input::hotkey::Hotkeys;
use state::State;
use win::{hwnd, raw};

const WM_APP_EVENTS: u32 = WM_APP + 1;
const WM_APP_IPC: u32 = WM_APP + 2;
const WM_APP_RELOAD: u32 = WM_APP + 3;
const WM_APP_TRAY: u32 = WM_APP + 4;
const WM_APP_LLBIND: u32 = WM_APP + 5;
const WM_APP_BARCMD: u32 = WM_APP + 6;

const TIMER_RELOAD: usize = 1;
const TIMER_RETILE: usize = 2;
const TIMER_RECONCILE: usize = 3;
const TIMER_HOVER: usize = 4;
/// Editors write files in several steps; reload once things settle.
const RELOAD_DEBOUNCE_MS: u32 = 60;
const SPI_SETWORKAREA: usize = 0x002F;

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static IPC_RX: RefCell<Option<Receiver<ipc::Request>>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

/// Runs `f` against the state unless it is already borrowed (re-entrancy
/// through a modal loop); returns None in that case so the caller can retry.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| {
        let mut guard = s.try_borrow_mut().ok()?;
        let st = guard.as_mut()?;
        Some(f(st))
    })
}

fn repost(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
    unsafe {
        let _ = PostMessageW(h, msg, wp, lp);
    }
}

/// Common tail after any user action: keep the border in place, honour `exit`.
fn after_action(st: &mut State) {
    st.update_overlay();
    st.push_bar();
    if st.quit_requested {
        unsafe { PostQuitMessage(0) };
    }
}

fn message_box(text: &str, warn: bool) {
    let t = win::wide(text);
    let flags = if warn { MB_OK | MB_ICONWARNING } else { MB_OK | MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), w!("deadbeef"), flags);
    }
}

/// What a launch of deadbeef.exe does.
enum Mode {
    Wm,
    Settings,
    Install,
    Uninstall,
}

/// `--wm` tiles (autostart uses it), `--settings` opens the settings app,
/// `--uninstall` is the Apps & features entry. A plain launch opens the
/// settings app on first run, after "Reset everything", or when the window
/// manager is already running; otherwise it tiles.
fn launch_mode(paths: &Paths) -> Mode {
    match std::env::args().nth(1).as_deref() {
        Some("--wm") => Mode::Wm,
        Some("--settings") => Mode::Settings,
        Some("--install") => Mode::Install,
        Some("--uninstall") => Mode::Uninstall,
        _ if settings::system::wm_running()
            || settings::system::is_disabled(paths)
            || !paths.active_conf().exists() =>
        {
            Mode::Settings
        }
        _ => Mode::Wm,
    }
}

/// Opens the settings app in a separate process.
fn open_settings() {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = std::process::Command::new(exe).arg("--settings").spawn() {
                log_warn!("open settings: {e}");
            }
        }
        Err(e) => log_warn!("open settings: {e}"),
    }
}

fn main() {
    let paths = Paths::discover();
    logging::init(&paths.log_file());
    std::panic::set_hook(Box::new(|info| log_error!("panic: {info}")));

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    match launch_mode(&paths) {
        Mode::Settings => return settings::run(paths),
        // Installs this build, then opens the installed copy's settings app.
        Mode::Install => {
            match settings::system::install() {
                Ok(_) => {
                    let exe = settings::system::installed_exe();
                    if let Err(e) = std::process::Command::new(&exe).arg("--settings").spawn() {
                        message_box(&format!("Installed, but could not open {}:\n{e}", exe.display()), true);
                    }
                }
                Err(e) => message_box(&format!("Install failed:\n{e}"), true),
            }
            return;
        }
        Mode::Uninstall => {
            match settings::system::uninstall(&paths) {
                Ok(()) => message_box(
                    &format!(
                        "deadbeef was uninstalled and Windows is back to normal.\n\nYour settings are kept in {}.",
                        paths.root.display()
                    ),
                    false,
                ),
                Err(e) => message_box(&format!("Uninstall finished with a problem:\n{e}"), true),
            }
            return;
        }
        Mode::Wm => {}
    }

    log_info!("deadbeef {} starting (Windows build {})", env!("CARGO_PKG_VERSION"), win::os_build());

    // Single instance per session.
    let _instance = unsafe { CreateMutexW(None, true, w!("Local\\deadbeef-wm-instance")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        message_box("deadbeef is already running.\n\nUse the tray icon or `dbctl` to control it.", false);
        return;
    }

    let first_launch = match profile::bootstrap(&paths) {
        Ok(f) => f,
        Err(e) => {
            log_error!("profile bootstrap failed: {e}");
            message_box(&format!("Could not prepare {}:\n{e}", paths.root.display()), true);
            return;
        }
    };

    let main = match create_main_window() {
        Ok(h) => h,
        Err(e) => {
            log_error!("main window: {e}");
            return;
        }
    };
    TASKBAR_CREATED.with(|t| t.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }));
    tray::add(main, WM_APP_TRAY);

    STATE.with(|s| *s.borrow_mut() = Some(State::new(paths.clone(), main, WM_APP_LLBIND, WM_APP_BARCMD)));
    with_state(|st| {
        st.reload();
        // Adopt windows that already exist, most recently used first.
        for h in win::top_level_windows() {
            st.manage(h);
        }
        st.focus_changed(win::foreground());
        st.retile();
        st.refresh_borders();
        log_info!("managing {} existing windows", st.clients.len());
    });

    let _hooks = wm::hook::install(main, WM_APP_EVENTS);
    unsafe {
        SetTimer(hwnd(main), TIMER_RECONCILE, 400, None);
        SetTimer(hwnd(main), TIMER_HOVER, 50, None);
    }

    match ipc::spawn(main, WM_APP_IPC) {
        Ok(rx) => IPC_RX.with(|r| *r.borrow_mut() = Some(rx)),
        Err(e) => {
            log_error!("IPC disabled: {e}");
            tray::balloon(main, "deadbeef", &format!("dbctl unavailable: {e}"), true);
        }
    }

    let _watcher = watch_profile(&paths, main);

    if first_launch {
        tray::balloon(
            main,
            "deadbeef is running",
            &format!("Created your profile from nerdy-deadbeef in\n{}", paths.active().display()),
            false,
        );
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    log_info!("shutting down");
    drop(_hooks);
    STATE.with(|s| {
        if let Some(mut st) = s.borrow_mut().take() {
            st.restore_all();
        }
    });
    tray::remove(main);
    unsafe {
        let _ = DestroyWindow(hwnd(main));
    }
}

fn create_main_window() -> windows::core::Result<isize> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("deadbeef_main");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        // A real (hidden) top-level window rather than HWND_MESSAGE: the tray
        // menu needs a window that can become foreground.
        let h = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("deadbeef"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )?;
        Ok(raw(h))
    }
}

/// Watches the active profile; any change posts WM_APP_RELOAD (debounced).
fn watch_profile(paths: &Paths, main: isize) -> Option<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if ev.kind.is_modify() || ev.kind.is_create() || ev.kind.is_remove() {
                unsafe {
                    let _ = PostMessageW(hwnd(main), WM_APP_RELOAD, WPARAM(0), LPARAM(0));
                }
            }
        }
    })
    .map_err(|e| log_error!("file watcher: {e}"))
    .ok()?;
    watcher
        .watch(&paths.active(), RecursiveMode::Recursive)
        .map_err(|e| log_error!("watch {}: {e}", paths.active().display()))
        .ok()?;
    Some(watcher)
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP_EVENTS => {
            if with_state(|st| {
                st.handle_events(wm::hook::drain());
                after_action(st);
            })
            .is_none()
            {
                repost(h, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            let index = Hotkeys::bind_index_from_hotkey_id(wp.0);
            if with_state(|st| {
                st.run_bind(index);
                st.mark_bar_dirty();
                after_action(st);
            })
            .is_none()
            {
                repost(h, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_APP_LLBIND => {
            if with_state(|st| {
                st.run_bind(wp.0);
                st.mark_bar_dirty();
                after_action(st);
            })
            .is_none()
            {
                repost(h, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_APP_IPC => {
            let handled = with_state(|st| {
                IPC_RX.with(|rx| {
                    if let Some(rx) = rx.borrow().as_ref() {
                        while let Ok(req) = rx.try_recv() {
                            let resp = ipc::commands::handle(st, &req.args);
                            let _ = req.reply.send(resp);
                        }
                    }
                });
                st.mark_bar_dirty();
                after_action(st);
            });
            if handled.is_none() {
                repost(h, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_APP_BARCMD => {
            if with_state(|st| {
                let cmds: Vec<bar::BarCmd> = st.bar.as_ref().map(|b| b.commands.try_iter().collect()).unwrap_or_default();
                for bar::BarCmd::Dispatch(name, arg) in cmds {
                    if let Err(e) = st.dispatch(&name, &arg) {
                        log_warn!("bar → {name} {arg}: {e}");
                    }
                }
                st.mark_bar_dirty();
                after_action(st);
            })
            .is_none()
            {
                repost(h, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_APP_RELOAD => {
            SetTimer(h, TIMER_RELOAD, RELOAD_DEBOUNCE_MS, None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_RELOAD => {
            let _ = KillTimer(h, TIMER_RELOAD);
            if with_state(|st| {
                if st.profile_changed() {
                    log_info!("active profile changed on disk; reloading");
                    st.reload();
                }
            })
            .is_none()
            {
                SetTimer(h, TIMER_RELOAD, RELOAD_DEBOUNCE_MS, None);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_HOVER => {
            with_state(|st| {
                let before = st.focused;
                st.follow_mouse_tick();
                if st.focused != before {
                    st.mark_bar_dirty();
                    after_action(st);
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_RECONCILE => {
            with_state(|st| {
                st.reconcile_focus();
                st.check_min_sizes();
                after_action(st);
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_RETILE => {
            let _ = KillTimer(h, TIMER_RETILE);
            if with_state(|st| {
                st.monitors = win::monitors();
                st.retile();
                st.update_overlay();
            })
            .is_none()
            {
                SetTimer(h, TIMER_RETILE, 100, None);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            SetTimer(h, TIMER_RETILE, 250, None);
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wp.0 == SPI_SETWORKAREA => {
            SetTimer(h, TIMER_RETILE, 100, None);
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let event = (lp.0 & 0xFFFF) as u32;
            match event {
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(h),
                WM_LBUTTONDBLCLK => open_settings(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_ENDSESSION if wp.0 != 0 => {
            with_state(|st| st.restore_all());
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ if msg != 0 && msg == TASKBAR_CREATED.with(|t| t.get()) => {
            // Explorer restarted: the notification area is new and empty, the
            // taskbar is back, and every AppBar registration was lost.
            tray::add(raw(h), WM_APP_TRAY);
            with_state(|st| {
                st.taskbar.enforce();
                if let Some(b) = &st.bar {
                    b.rebuild();
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

fn open_profile_folder(st: &State) {
    let dir = st.paths.active();
    if let Err(e) = win::shell_exec(&format!("explorer.exe \"{}\"", dir.display())) {
        log_warn!("open profile folder: {e}");
    }
}

fn tray_menu(h: HWND) {
    // Build data with a short borrow; TrackPopupMenu runs a modal loop and
    // must not hold the state.
    let Some(list) = with_state(|st| profile::list(&st.paths)) else { return };
    let Some(choice) = tray::show_menu(raw(h), &list) else { return };
    with_state(|st| {
        let r: Result<String, String> = match choice {
            tray::MenuChoice::Settings => {
                open_settings();
                Ok(String::new())
            }
            tray::MenuChoice::Reload => {
                st.reload();
                Ok("reloaded".into())
            }
            tray::MenuChoice::OpenFolder => {
                open_profile_folder(st);
                Ok(String::new())
            }
            tray::MenuChoice::OpenLog => win::shell_exec(&format!("notepad.exe \"{}\"", st.paths.log_file().display())).map(|_| String::new()),
            tray::MenuChoice::Reset => profile::apply(&st.paths, profile::DEFAULT_TEMPLATE).inspect(|_| {
                st.reload();
            }),
            tray::MenuChoice::Switch(name) => profile::switch(&st.paths, &name).inspect(|_| {
                st.reload();
            }),
            tray::MenuChoice::Quit => {
                st.quit_requested = true;
                Ok(String::new())
            }
        };
        match r {
            Ok(m) if !m.is_empty() => log_info!("tray: {m}"),
            Err(e) => {
                log_warn!("tray: {e}");
                tray::balloon(st.main, "deadbeef", &e, true);
            }
            _ => {}
        }
        after_action(st);
    });
}
