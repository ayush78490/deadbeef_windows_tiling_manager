//! dbctl — command-line client for deadbeef over `\\.\pipe\deadbeef`.
//!
//! Sends the arguments as a JSON array, prints the reply, exits non-zero on error.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const PIPE: &str = r"\\.\pipe\deadbeef";
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_BROKEN_PIPE: i32 = 109;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;

#[derive(serde::Deserialize)]
struct Response {
    ok: bool,
    out: String,
}

const USAGE: &str = "\
usage: dbctl <command> [args]

  reload                          reload the active profile
  dispatch <name> [args]          e.g. dbctl dispatch exec wt
  keyword <key> <value>           e.g. dbctl keyword general:gaps_in 8
  clients | monitors | workspaces | binds | activewindow   [-j for JSON]
  profile list | apply <template> | save <name> | switch <name>
  errors | dispatchers | getoption <key> | version | help";

fn connect() -> Result<File, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match OpenOptions::new().read(true).write(true).open(PIPE) {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                return Err("deadbeef is not running (no \\\\.\\pipe\\deadbeef)".into())
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("cannot connect to deadbeef: {e}")),
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let mut pipe = match connect() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("dbctl: {e}");
            return ExitCode::from(2);
        }
    };
    let request = serde_json::to_vec(&args).expect("serialize args");
    if let Err(e) = pipe.write_all(&request) {
        eprintln!("dbctl: write failed: {e}");
        return ExitCode::from(2);
    }
    // The server disconnects right after its reply; that surfaces as
    // BROKEN_PIPE or PIPE_NOT_CONNECTED, both of which mean end-of-stream.
    let mut reply = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.raw_os_error(), Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED)) => break,
            Err(e) => {
                eprintln!("dbctl: read failed: {e}");
                return ExitCode::from(2);
            }
        }
    }
    match serde_json::from_slice::<Response>(&reply) {
        Ok(r) if r.ok => {
            if !r.out.is_empty() {
                println!("{}", r.out);
            }
            ExitCode::SUCCESS
        }
        Ok(r) => {
            eprintln!("dbctl: {}", r.out);
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("dbctl: malformed reply: {e}");
            ExitCode::from(2)
        }
    }
}
