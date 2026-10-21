//! Minimal blocking HTTPS GET over WinHTTP (the OS TLS stack — no extra
//! crates, honours the system proxy configuration).

use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

use crate::win::wide;

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn open(h: *mut c_void, what: &str) -> Result<Handle, String> {
    if h.is_null() {
        Err(format!("{what}: {}", windows::core::Error::from_win32()))
    } else {
        Ok(Handle(h))
    }
}

/// Splits `https://host[:port]/path?query` into (secure, host, port, path).
pub fn split_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("unsupported URL '{url}'"));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| format!("bad port in '{url}'"))?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("missing host in '{url}'"));
    }
    Ok((secure, host.to_string(), port, path.to_string()))
}

/// Downloads `url`. `progress(received, total)` is called as data arrives;
/// returning `false` cancels.
pub fn get(url: &str, mut progress: impl FnMut(u64, Option<u64>) -> bool) -> Result<Vec<u8>, String> {
    let (secure, host, port, path) = split_url(url)?;
    unsafe {
        let session = open(
            WinHttpOpen(
                w!("deadbeef/0.1 (+https://github.com/; Windows tiling WM)"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let host_w = wide(&host);
        let conn = open(WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), port, 0), "WinHttpConnect")?;
        let path_w = wide(&path);
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let req = open(
            WinHttpOpenRequest(conn.0, w!("GET"), PCWSTR(path_w.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags),
            "WinHttpOpenRequest",
        )?;
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).map_err(|e| format!("request to {host} failed: {}", e.message()))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("no response from {host}: {}", e.message()))?;

        let mut status = 0u32;
        let mut size = 4u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| e.to_string())?;
        if status != 200 {
            return Err(format!("HTTP {status} from {host}"));
        }
        let mut length = 0u32;
        let mut size = 4u32;
        let total = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut length as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()
        .map(|_| length as u64);

        let mut body = Vec::with_capacity(total.unwrap_or(64 * 1024) as usize);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut avail).map_err(|e| format!("download interrupted: {}", e.message()))?;
            if avail == 0 {
                break;
            }
            let want = (avail as usize).min(buf.len());
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut c_void, want as u32, &mut read)
                .map_err(|e| format!("download interrupted: {}", e.message()))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if !progress(body.len() as u64, total) {
                return Err("cancelled".into());
            }
        }
        Ok(body)
    }
}

/// Percent-encodes a query-string value.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            split_url("https://wallhaven.cc/api/v1/search?q=a").unwrap(),
            (true, "wallhaven.cc".into(), 443, "/api/v1/search?q=a".into())
        );
        assert_eq!(split_url("http://localhost:8080").unwrap(), (false, "localhost".into(), 8080, "/".into()));
        assert!(split_url("ftp://x").is_err());
        assert_eq!(encode("nord theme/π"), "nord+theme%2F%CF%80");
    }
}
