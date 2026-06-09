use std::{fs, io::Write, path::Path};

use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

use crate::{
    error::{Result, UsbBuddyError},
    hash::encode_hex,
};

/// Download `url` to `dest`, streaming the body through a SHA256 hasher.
/// After a successful download the computed hex digest is returned.
/// If `expected_sha256` is supplied the download is rejected if it does not match.
pub fn download_verified(url: &str, dest: &Path, expected_sha256: Option<&str>) -> Result<String> {
    let client = Client::builder()
        .user_agent(concat!("usbuddy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| UsbBuddyError::Network(e.to_string()))?;

    let mut response = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| UsbBuddyError::Network(e.to_string()))?;

    // Write to a temp file in the same directory so the final rename is atomic.
    let parent = dest.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    let mut file = tmp.as_file();

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        use std::io::Read;
        let n = response
            .read(&mut buf)
            .map_err(|e| UsbBuddyError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }

    let digest = encode_hex(hasher.finalize());

    if let Some(expected) = expected_sha256
        && !expected.eq_ignore_ascii_case(&digest)
    {
        return Err(UsbBuddyError::HashMismatch {
            expected: expected.to_string(),
            actual: digest,
        });
    if dest.exists() {
        fs::remove_file(dest)?;
    }

    tmp.persist(dest)
        .map_err(|e| UsbBuddyError::Io(std::io::Error::other(e.to_string())))?;
    Ok(digest)
}
