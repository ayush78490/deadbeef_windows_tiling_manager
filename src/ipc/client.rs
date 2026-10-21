//! Client side of `\\.\pipe\deadbeef`, used by the settings app (same wire
//! format as `dbctl`).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use super::{Response, PIPE_NAME};

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_BROKEN_PIPE: i32 = 109;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;

fn connect() -> Result<File, String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => return Err("deadbeef is not running".into()),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("cannot connect to deadbeef: {e}")),
        }
    }
}

/// Sends one command and returns its output (`Err` if deadbeef reported failure).
pub fn send(args: &[&str]) -> Result<String, String> {
    let mut pipe = connect()?;
    let request = serde_json::to_vec(args).map_err(|e| e.to_string())?;
    pipe.write_all(&request).map_err(|e| format!("write failed: {e}"))?;
    let mut reply = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.raw_os_error(), Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED)) => break,
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    let r: Response = serde_json::from_slice(&reply).map_err(|e| format!("malformed reply: {e}"))?;
    if r.ok {
        Ok(r.out)
    } else {
        Err(r.out)
    }
}
