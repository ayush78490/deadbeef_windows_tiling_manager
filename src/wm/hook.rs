//! System-wide window events via `SetWinEventHook` (out-of-context).
//!
//! Out-of-context hooks are delivered on the thread that installed them,
//! during its message loop. The callback does the minimum — filter, enqueue,
//! post one wake-up message — so hook latency stays in the microseconds; the
//! main loop drains the queue in a batch and retiles once.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
    EVENT_SYSTEM_MOVESIZESTART, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use crate::win::{hwnd, raw};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Create,
    Destroy,
    Show,
    Hide,
    Cloaked,
    Uncloaked,
    Foreground,
    MinimizeStart,
    MinimizeEnd,
    MoveSizeStart,
    MoveSizeEnd,
    LocationChange,
    NameChange,
}

impl Event {
    fn from_raw(e: u32) -> Option<Event> {
        Some(match e {
            EVENT_OBJECT_CREATE => Event::Create,
            EVENT_OBJECT_DESTROY => Event::Destroy,
            EVENT_OBJECT_SHOW => Event::Show,
            EVENT_OBJECT_HIDE => Event::Hide,
            EVENT_OBJECT_CLOAKED => Event::Cloaked,
            EVENT_OBJECT_UNCLOAKED => Event::Uncloaked,
            EVENT_SYSTEM_FOREGROUND => Event::Foreground,
            EVENT_SYSTEM_MINIMIZESTART => Event::MinimizeStart,
            EVENT_SYSTEM_MINIMIZEEND => Event::MinimizeEnd,
            EVENT_SYSTEM_MOVESIZESTART => Event::MoveSizeStart,
            EVENT_SYSTEM_MOVESIZEEND => Event::MoveSizeEnd,
            EVENT_OBJECT_LOCATIONCHANGE => Event::LocationChange,
            EVENT_OBJECT_NAMECHANGE => Event::NameChange,
            _ => return None,
        })
    }
}

thread_local! {
    static QUEUE: RefCell<Vec<(Event, isize)>> = const { RefCell::new(Vec::new()) };
    static POSTED: Cell<bool> = const { Cell::new(false) };
    static TARGET: Cell<isize> = const { Cell::new(0) };
}

pub struct Hooks {
    handles: Vec<HWINEVENTHOOK>,
}

/// Event ranges hooked. Kept tight: a full EVENT_MIN..EVENT_MAX hook would
/// wake us for every caret blink and focus change inside every app.
const RANGES: &[(u32, u32)] = &[
    (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
    (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MINIMIZEEND),
    (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
    (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE),
    (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
];

/// Installs the hooks; `notify` receives `wake_msg` when events are queued.
pub fn install(notify: isize, wake_msg: u32) -> Hooks {
    TARGET.with(|t| t.set(notify));
    WAKE_MSG.with(|m| m.set(wake_msg));
    let handles = RANGES
        .iter()
        .filter_map(|&(lo, hi)| {
            let h = unsafe {
                SetWinEventHook(lo, hi, None, Some(callback), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
            };
            if h.is_invalid() {
                crate::log_error!("SetWinEventHook({lo:#x}..{hi:#x}) failed");
                None
            } else {
                Some(h)
            }
        })
        .collect();
    Hooks { handles }
}

thread_local! {
    static WAKE_MSG: Cell<u32> = const { Cell::new(0) };
}

impl Drop for Hooks {
    fn drop(&mut self) {
        for h in self.handles.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
    }
}

unsafe extern "system" fn callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    h: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // OBJID_WINDOW / CHILDID_SELF: the window itself, not a caret, cursor,
    // scrollbar or accessible child.
    if h.is_invalid() || id_object != 0 || id_child != 0 {
        return;
    }
    let Some(ev) = Event::from_raw(event) else { return };
    let h = raw(h);
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        // Location/name changes arrive in floods; one per window per batch is enough.
        if matches!(ev, Event::LocationChange | Event::NameChange) && q.iter().any(|&(e, w)| e == ev && w == h) {
            return;
        }
        q.push((ev, h));
    });
    if !POSTED.with(|p| p.replace(true)) {
        let target = TARGET.with(|t| t.get());
        let msg = WAKE_MSG.with(|m| m.get());
        let _ = PostMessageW(hwnd(target), msg, WPARAM(0), LPARAM(0));
    }
}

/// Takes all queued events (called from the main loop).
pub fn drain() -> Vec<(Event, isize)> {
    POSTED.with(|p| p.set(false));
    QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}
