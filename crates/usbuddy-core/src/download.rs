use std::{fs, io::Write, path::Path};

use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

use crate::{
    error::{Result, UsbBuddyError},
    hash::encode_hex,
};

/// Progress report emitted by [`download_verified_with_progress`].
///
/// `bytes_total` is `None` when the server does not send a `Content-Length`
/// (rare for static asset CDNs, common for chunked APIs). Callers should
/// render an indeterminate bar in that case.
#[derive(Debug, Clone, Copy)]
pub struct DownloadProgress {
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

/// Caller decision polled between chunks by
/// [`download_verified_with_progress_controlled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadControl {
    Continue,
    Cancel,
}

/// Download `url` to `dest`, streaming the body through a SHA256 hasher.
/// After a successful download the computed hex digest is returned.
/// If `expected_sha256` is supplied the download is rejected if it does not match.
///
/// This is a thin wrapper around [`download_verified_with_progress`] that
/// discards progress updates — preserved for call sites that don't need
/// progress reporting.
pub fn download_verified(url: &str, dest: &Path, expected_sha256: Option<&str>) -> Result<String> {
    download_verified_with_progress(url, dest, expected_sha256, |_| {})
}

/// Same as [`download_verified`] but invokes `progress` periodically with the
/// cumulative number of bytes written so far (and the total content length
/// when known). The callback fires at least once per ~256 KiB chunk so the
/// caller can drive a determinate progress bar at sub-second granularity
/// even on multi-gigabyte downloads.
///
/// The callback runs on the calling thread inside the read loop, so it
/// must not block — keep it to a cheap mutex/atomic write that the UI
/// thread polls.
pub fn download_verified_with_progress(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    progress: impl FnMut(DownloadProgress),
) -> Result<String> {
    download_verified_with_progress_controlled(url, dest, expected_sha256, progress, || {
        DownloadControl::Continue
    })
}

/// Same as [`download_verified_with_progress`] but also polls `control`
/// between chunks so the caller can cancel mid-flight. On
/// [`DownloadControl::Cancel`] the partial temp file is discarded and
/// [`UsbBuddyError::Canceled`] is returned; nothing is written to `dest`.
pub fn download_verified_with_progress_controlled(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    mut progress: impl FnMut(DownloadProgress),
    mut control: impl FnMut() -> DownloadControl,
) -> Result<String> {
    let client = Client::builder()
        .user_agent(concat!("usbuddy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| UsbBuddyError::Network(e.to_string()))?;

    let mut response = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| UsbBuddyError::Network(e.to_string()))?;

    let bytes_total = response.content_length();

    // Write to a temp file in the same directory so the final rename is atomic.
    let parent = dest.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    let mut file = tmp.as_file();

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    // Emit a 0-bytes ping so consumers can render the bar immediately
    // instead of waiting for the first chunk on slow servers.
    progress(DownloadProgress {
        bytes_done: 0,
        bytes_total,
    });
    let mut bytes_done: u64 = 0;
    // Throttle callback frequency to roughly every 256 KiB so we don't
    // spam the UI thread with millions of mpsc messages for large files.
    const PROGRESS_INTERVAL_BYTES: u64 = 256 * 1024;
    let mut bytes_since_last_emit: u64 = 0;

    loop {
        use std::io::Read;
        match control() {
            DownloadControl::Continue => {}
            DownloadControl::Cancel => return Err(UsbBuddyError::Canceled),
        }
        let n = response
            .read(&mut buf)
            .map_err(|e| UsbBuddyError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        bytes_done += n as u64;
        bytes_since_last_emit += n as u64;
        if bytes_since_last_emit >= PROGRESS_INTERVAL_BYTES {
            progress(DownloadProgress {
                bytes_done,
                bytes_total,
            });
            bytes_since_last_emit = 0;
        }
    }
    // Final emit so consumers always see the 100% / final-count tick.
    progress(DownloadProgress {
        bytes_done,
        bytes_total,
    });

    let digest = encode_hex(hasher.finalize());

    if let Some(expected) = expected_sha256
        && !expected.eq_ignore_ascii_case(&digest)
    {
        return Err(UsbBuddyError::HashMismatch {
            expected: expected.to_string(),
            actual: digest,
        });
    }
    if dest.exists() {
        fs::remove_file(dest)?;
    }

    tmp.persist(dest)
        .map_err(|e| UsbBuddyError::Io(std::io::Error::other(e.to_string())))?;
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// Spin up a one-shot HTTP/1.0 server that serves `body` with
    /// `Content-Length`, returns its `http://127.0.0.1:<port>/` URL.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut req = [0u8; 1024];
                let _ = sock.read(&mut req);
                let header = format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes());
                let _ = sock.write_all(&body);
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    #[test]
    fn progress_callback_reports_total_and_final_byte_count() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("blob.bin");
        // 800 KiB — enough to cross the 256 KiB throttle threshold a few times.
        let body = vec![0xABu8; 800 * 1024];
        let url = serve_once(body.clone());

        let calls: Arc<Mutex<Vec<DownloadProgress>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        download_verified_with_progress(&url, &dest, None, move |p| {
            calls_clone.lock().unwrap().push(p);
        })
        .unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert!(
            recorded.len() >= 2,
            "expected at least an initial + final progress call, got {}",
            recorded.len()
        );
        assert_eq!(recorded.first().unwrap().bytes_done, 0);
        assert_eq!(recorded.last().unwrap().bytes_done, body.len() as u64);
        assert_eq!(
            recorded.last().unwrap().bytes_total,
            Some(body.len() as u64)
        );
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), body.len() as u64);
    }
}
