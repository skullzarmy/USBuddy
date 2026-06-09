use std::{fs::File, io::Read, path::Path};

use sha2::{Digest, Sha256};

use crate::error::Result;

pub(crate) fn encode_hex(bytes: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    encode_hex(hasher.finalize())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(encode_hex(hasher.finalize()))
}

pub fn verify_file_hash(path: &Path, expected: &str) -> Result<bool> {
    Ok(sha256_file(path)? == expected.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::sha256_bytes;

    #[test]
    fn hashes_known_bytes() {
        assert_eq!(
            sha256_bytes(b"usbuddy"),
            "b9b77e0c4aa0ea5f0d0fb9a6a268a128c68c14f17dba7e95b7ef1789a947e691"
        );
    }
}
