//! Named-pipe IPC server (`\\.\pipe\deadbeef`) used by `dbctl`.
//!
//! Wire format: the client writes one message containing a JSON array of
//! argument strings; the server answers with one JSON object
//! `{"ok": bool, "out": "..."}` and disconnects. Requests are executed on
//! the main thread (posted via a window message) so no state is shared
//! across threads.

pub mod client;
pub mod commands;

use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_PIPE_CONNECTED, HANDLE, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::win::{hwnd, wide};

pub const PIPE_NAME: &str = r"\\.\pipe\deadbeef";
const BUF: u32 = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub out: String,
}

pub struct Request {
    pub args: Vec<String>,
    pub reply: mpsc::Sender<Response>,
}

/// Starts the server thread. Each request is pushed to the returned receiver
/// and `wake_msg` is posted to `main`.
pub fn spawn(main: isize, wake_msg: u32) -> Result<mpsc::Receiver<Request>, String> {
    let name = wide(PIPE_NAME);
    // FIRST_PIPE_INSTANCE: refuse to start if another process already owns
    // the name (prevents pipe squatting).
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUF,
            BUF,
            0,
            None,
        )
    };
    if pipe.is_invalid() {
        return Err(format!("CreateNamedPipeW({PIPE_NAME}): {}", windows::core::Error::from_win32()));
    }
    let pipe_raw = pipe.0 as isize;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("deadbeef-ipc".into())
        .spawn(move || serve(HANDLE(pipe_raw as *mut _), main, wake_msg, tx))
        .map_err(|e| e.to_string())?;
    Ok(rx)
}

fn serve(pipe: HANDLE, main: isize, wake_msg: u32, tx: mpsc::Sender<Request>) {
    loop {
        let connected = unsafe { ConnectNamedPipe(pipe, None) };
        if let Err(e) = connected {
            if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                crate::log_warn!("ConnectNamedPipe: {e}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        }
        let response = match read_message(pipe) {
            Ok(bytes) => handle_bytes(&bytes, main, wake_msg, &tx),
            Err(e) => Response { ok: false, out: format!("read failed: {e}") },
        };
        let body = serde_json::to_vec(&response).unwrap_or_default();
        unsafe {
            let mut written = 0;
            let _ = WriteFile(pipe, Some(&body), Some(&mut written), None);
            let _ = FlushFileBuffers(pipe);
            let _ = DisconnectNamedPipe(pipe);
        }
    }
}


fn read_message(pipe: HANDLE) -> windows::core::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 4096];
    loop {
        let mut read = 0u32;
        let r = unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut read), None) };
        out.extend_from_slice(&buf[..read as usize]);
        match r {
            Ok(()) => return Ok(out),
            Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => continue,
            Err(e) => return Err(e),
        }
    }
}

fn handle_bytes(bytes: &[u8], main: isize, wake_msg: u32, tx: &mpsc::Sender<Request>) -> Response {
    let args: Vec<String> = match serde_json::from_slice(bytes) {
        Ok(a) => a,
        Err(e) => return Response { ok: false, out: format!("malformed request: {e}") },
    };
    if args.is_empty() {
        return Response { ok: false, out: "empty request".into() };
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx.send(Request { args, reply: reply_tx }).is_err() {
        return Response { ok: false, out: "deadbeef is shutting down".into() };
    }
    unsafe {
        let _ = PostMessageW(hwnd(main), wake_msg, WPARAM(0), LPARAM(0));
    }
    reply_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or(Response { ok: false, out: "timed out waiting for the window manager".into() })
}
