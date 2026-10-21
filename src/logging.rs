//! Minimal file logger: `%APPDATA%\deadbeef\deadbeef.log` + OutputDebugString.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

static LOG: Mutex<Option<File>> = Mutex::new(None);
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

pub fn init(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Start fresh once the log grows large.
    let truncate = std::fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false);
    let file = OpenOptions::new().create(true).append(!truncate).write(true).truncate(truncate).open(path).ok();
    *LOG.lock().unwrap() = file;
}

pub fn write(level: &str, args: fmt::Arguments) {
    let t = crate::win::local_time();
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {level:<5} {args}\n",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    );
    if let Ok(mut guard) = LOG.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }
    let w = crate::win::wide(&line);
    unsafe { windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(windows::core::PCWSTR(w.as_ptr())) };
}

#[macro_export]
macro_rules! log_info {
    ($($t:tt)*) => { $crate::logging::write("INFO", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($t:tt)*) => { $crate::logging::write("WARN", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($t:tt)*) => { $crate::logging::write("ERROR", format_args!($($t)*)) };
}
