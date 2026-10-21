//! Global keybinds.
//!
//! Every bind is first registered with `RegisterHotKey`. Explorer already
//! owns most `Win+<key>` combos (Win+E, Win+R, Win+1..9, Win+arrows, …), and
//! for those RegisterHotKey fails with ERROR_HOTKEY_ALREADY_REGISTERED. When
//! `input:hotkey_fallback` is on, such binds are served by a `WH_KEYBOARD_LL`
//! hook on a dedicated thread instead (the approach whkd/AutoHotkey use). The
//! hook swallows the key and taps an unassigned VK so releasing Win does not
//! open the Start menu.
//!
//! On reload everything is unregistered and registered again from the new config.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::config::types::{Bind, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use crate::win::{hwnd, INJECT_MARK};

#[derive(Debug, Clone, Serialize)]
pub struct BindStatus {
    pub combo: String,
    pub dispatcher: String,
    pub arg: String,
    pub method: &'static str,
    pub error: Option<String>,
}

pub struct Hotkeys {
    main: isize,
    registered: Vec<i32>,
    ll_thread: Option<u32>,
    pub status: Vec<BindStatus>,
}

static LL_TABLE: OnceLock<Mutex<HashMap<(u32, u32), usize>>> = OnceLock::new();
static LL_SWALLOWED: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
static LL_MAIN: AtomicIsize = AtomicIsize::new(0);
static LL_MSG: AtomicU32 = AtomicU32::new(0);
/// Physical modifier keys currently held (one bit per left/right key).
static LL_HELD: AtomicU32 = AtomicU32::new(0);

fn table() -> &'static Mutex<HashMap<(u32, u32), usize>> {
    LL_TABLE.get_or_init(Default::default)
}

fn swallowed() -> &'static Mutex<HashSet<u32>> {
    LL_SWALLOWED.get_or_init(Default::default)
}

impl Hotkeys {
    /// `ll_msg` is posted to `main` with wParam = bind index for hook-served binds.
    pub fn new(main: isize, ll_msg: u32) -> Self {
        LL_MAIN.store(main, Ordering::SeqCst);
        LL_MSG.store(ll_msg, Ordering::SeqCst);
        Self { main, registered: Vec::new(), ll_thread: None, status: Vec::new() }
    }

    /// WM_HOTKEY ids are `bind index + 1`.
    pub fn bind_index_from_hotkey_id(id: usize) -> usize {
        id - 1
    }

    pub fn unregister_all(&mut self) {
        for id in self.registered.drain(..) {
            unsafe {
                let _ = UnregisterHotKey(hwnd(self.main), id);
            }
        }
        table().lock().unwrap().clear();
        self.status.clear();
    }

    /// Registers `binds`, replacing whatever was registered before.
    pub fn register_all(&mut self, binds: &[Bind], fallback: bool) {
        self.unregister_all();
        let mut seen = HashSet::new();
        let mut ll = HashMap::new();
        for (i, b) in binds.iter().enumerate() {
            let mut st = BindStatus {
                combo: b.combo.clone(),
                dispatcher: b.dispatcher.clone(),
                arg: b.arg.clone(),
                method: "none",
                error: None,
            };
            if !seen.insert((b.mods, b.vk)) {
                st.error = Some("duplicate bind; earlier definition wins".into());
                self.status.push(st);
                continue;
            }
            let id = i as i32 + 1;
            let r = unsafe { RegisterHotKey(hwnd(self.main), id, HOT_KEY_MODIFIERS(b.mods) | MOD_NOREPEAT, b.vk) };
            match r {
                Ok(()) => {
                    self.registered.push(id);
                    st.method = "RegisterHotKey";
                }
                Err(e) if fallback => {
                    ll.insert((b.mods, b.vk), i);
                    st.method = "llhook";
                    st.error = Some(format!("RegisterHotKey: {}", e.message()));
                }
                Err(e) => st.error = Some(format!("RegisterHotKey: {} (enable input:hotkey_fallback)", e.message())),
            }
            self.status.push(st);
        }
        let need_hook = !ll.is_empty();
        *table().lock().unwrap() = ll;
        if need_hook && self.ll_thread.is_none() {
            self.ll_thread = spawn_ll_thread();
        }
        let failed = self.status.iter().filter(|s| s.method == "none").count();
        crate::log_info!(
            "keybinds: {} via RegisterHotKey, {} via keyboard hook, {} failed",
            self.registered.len(),
            self.status.iter().filter(|s| s.method == "llhook").count(),
            failed
        );
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.unregister_all();
        if let Some(tid) = self.ll_thread.take() {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn spawn_ll_thread() -> Option<u32> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("deadbeef-kbhook".into())
        .spawn(move || unsafe {
            let module = GetModuleHandleW(None).unwrap_or_default();
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), HINSTANCE(module.0), 0) {
                Ok(h) => h,
                Err(e) => {
                    crate::log_error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}");
                    let _ = tx.send(None);
                    return;
                }
            };
            let _ = tx.send(Some(GetCurrentThreadId()));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);
        })
        .ok()?;
    rx.recv().ok().flatten()
}

fn held_bit(vk: u32) -> Option<u32> {
    Some(match vk {
        0x5B => 1 << 0, // LWIN
        0x5C => 1 << 1, // RWIN
        0xA0 | 0x10 => 1 << 2, // LSHIFT / SHIFT
        0xA1 => 1 << 3,
        0xA2 | 0x11 => 1 << 4, // LCONTROL / CONTROL
        0xA3 => 1 << 5,
        0xA4 | 0x12 => 1 << 6, // LMENU / MENU
        0xA5 => 1 << 7,
        _ => return None,
    })
}

fn held_mods(held: u32) -> u32 {
    let mut m = 0;
    if held & 0b11 != 0 {
        m |= MOD_WIN;
    }
    if held & 0b1100 != 0 {
        m |= MOD_SHIFT;
    }
    if held & 0b11_0000 != 0 {
        m |= MOD_CONTROL;
    }
    if held & 0b1100_0000 != 0 {
        m |= MOD_ALT;
    }
    m
}

unsafe extern "system" fn ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if kb.dwExtraInfo != INJECT_MARK {
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            let vk = kb.vkCode;
            if let Some(bit) = held_bit(vk) {
                if down {
                    LL_HELD.fetch_or(bit, Ordering::SeqCst);
                } else if up {
                    LL_HELD.fetch_and(!bit, Ordering::SeqCst);
                }
            } else if down {
                if swallowed().lock().unwrap().contains(&vk) {
                    return LRESULT(1); // auto-repeat of a bind we already fired
                }
                let mods = held_mods(LL_HELD.load(Ordering::SeqCst));
                let hit = table().lock().unwrap().get(&(mods, vk)).copied();
                if let Some(index) = hit {
                    swallowed().lock().unwrap().insert(vk);
                    let _ = PostMessageW(
                        hwnd(LL_MAIN.load(Ordering::SeqCst)),
                        LL_MSG.load(Ordering::SeqCst),
                        WPARAM(index),
                        LPARAM(0),
                    );
                    if mods & (MOD_WIN | MOD_ALT) != 0 {
                        // Keep Start (Win) / the menu bar (Alt) from activating on release.
                        crate::win::tap_mask_key();
                    }
                    return LRESULT(1);
                }
            } else if up && swallowed().lock().unwrap().remove(&vk) {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
