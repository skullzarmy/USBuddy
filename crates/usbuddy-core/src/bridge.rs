//! Editor bridge — shared bits for the OpenAI-compatible `/v1` surface the
//! runtime exposes to coding assistants (Continue, Cline, Zed, VS Code BYOK).
//!
//! Only the pieces that aren't HTTP plumbing live here: the bearer token's
//! lifecycle and the context-length defaults. The handlers themselves stay in
//! `usbuddy-runtime` alongside the existing llama-server proxy.
//!
//! See `docs/EDITOR-INTEGRATION.md` for why the protocol is OpenAI `/v1` and
//! not MCP.

use std::{fs, path::Path};

use crate::{atomic::atomic_write_string, error::Result};

/// Default context window for bridge-initiated launches. The chat UI's 4096 is
/// far too small for coding work — Cline and Roo Code routinely send 20k+
/// tokens of file context — but the value is still capped at load time to the
/// model's trained context length, and the RAM advisor prices the resulting KV
/// cache before anything is allowed to start.
pub const DEFAULT_BRIDGE_CTX_TOKENS: u32 = 16_384;

/// Lower bound accepted from the UI. Below this the bridge is useless for
/// editors and the failures look like model bugs rather than configuration.
pub const MIN_BRIDGE_CTX_TOKENS: u32 = 2_048;

/// Recognizable prefix so a token found in a config file, a shell history, or
/// a bug report is obviously a USBuddy bridge key and not a cloud provider's.
const TOKEN_PREFIX: &str = "usb-";

/// Bytes of CSPRNG material behind each token (256 bits, hex-encoded).
const TOKEN_ENTROPY_BYTES: usize = 32;

/// Generates a fresh bridge token from the OS CSPRNG.
pub fn generate_token() -> Result<String> {
    let mut raw = [0u8; TOKEN_ENTROPY_BYTES];
    getrandom::fill(&mut raw).map_err(|e| {
        crate::error::UsbBuddyError::InvalidState(format!("OS random source unavailable: {e}"))
    })?;
    let mut out = String::with_capacity(TOKEN_PREFIX.len() + TOKEN_ENTROPY_BYTES * 2);
    out.push_str(TOKEN_PREFIX);
    for byte in raw {
        use std::fmt::Write as _;
        // Writing to a String is infallible; the Result is an artifact of the
        // fmt::Write trait.
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// Reads the token at `path`, creating one if it's missing or unusable.
///
/// Persisting across sessions is deliberate: an editor configured once should
/// keep working the next time the stick is plugged in. [`rotate_token`] is the
/// escape hatch when a token leaks.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    if let Ok(existing) = fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    rotate_token(path)
}

/// Replaces the stored token with a fresh one, invalidating every editor
/// currently configured against the old value.
pub fn rotate_token(path: &Path) -> Result<String> {
    let token = generate_token()?;
    atomic_write_string(path, &format!("{token}\n"))?;
    Ok(token)
}

/// Constant-time-ish comparison of a presented token against the stored one.
///
/// Timing attacks against a loopback-only endpoint that also requires the
/// attacker to already be running code on the machine are not a realistic
/// threat, but the comparison is cheap to do properly and the alternative
/// invites a "why is this `==`?" question at every future review.
pub fn token_matches(presented: &str, stored: &str) -> bool {
    let (a, b) = (presented.as_bytes(), stored.as_bytes());
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extracts the bearer token from an `Authorization: Bearer …` value, or from
/// a bare `x-api-key` style value. Returns `None` for anything unparseable.
pub fn parse_bearer(header_value: &str) -> Option<&str> {
    let v = header_value.trim();
    if v.is_empty() {
        return None;
    }
    if let Some((scheme, rest)) = v.split_once(' ') {
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None; // Basic, Digest, … — not a scheme we accept.
        }
        let token = rest.trim();
        return (!token.is_empty()).then_some(token);
    }
    // No space at all — an `x-api-key`-style raw token. Except when the whole
    // value is just the scheme word: `Bearer ` trims down to `Bearer`, and
    // returning that as the token would be nonsense.
    if v.eq_ignore_ascii_case("bearer") {
        return None;
    }
    Some(v)
}

/// Clamps a user-supplied context length into the range the bridge supports.
/// `trained_cap` is the model's own context length when the GGUF header could
/// be read; `None` leaves only the floor applied.
pub fn clamp_ctx_tokens(requested: u32, trained_cap: Option<u32>) -> u32 {
    let floored = requested.max(MIN_BRIDGE_CTX_TOKENS);
    match trained_cap {
        // A model trained below our floor still can't be pushed past its cap.
        Some(cap) if cap > 0 => floored.min(cap),
        _ => floored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_prefixed_and_unique() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert!(a.starts_with(TOKEN_PREFIX));
        assert_eq!(a.len(), TOKEN_PREFIX.len() + TOKEN_ENTROPY_BYTES * 2);
        assert!(
            a[TOKEN_PREFIX.len()..]
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
        );
        assert_ne!(a, b, "CSPRNG returned the same token twice");
    }

    #[test]
    fn token_persists_then_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-token");

        let first = load_or_create_token(&path).unwrap();
        assert_eq!(load_or_create_token(&path).unwrap(), first, "must persist");

        let rotated = rotate_token(&path).unwrap();
        assert_ne!(rotated, first);
        assert_eq!(load_or_create_token(&path).unwrap(), rotated);
    }

    #[test]
    fn empty_or_whitespace_token_file_is_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-token");
        std::fs::write(&path, "   \n").unwrap();
        let token = load_or_create_token(&path).unwrap();
        assert!(token.starts_with(TOKEN_PREFIX));
    }

    #[test]
    fn matching_rejects_mismatches_and_empties() {
        assert!(token_matches("usb-abc", "usb-abc"));
        assert!(!token_matches("usb-abc", "usb-abd"));
        assert!(!token_matches("usb-ab", "usb-abc"));
        assert!(!token_matches("", ""));
    }

    #[test]
    fn bearer_parsing() {
        assert_eq!(parse_bearer("Bearer usb-abc"), Some("usb-abc"));
        assert_eq!(parse_bearer("bearer   usb-abc  "), Some("usb-abc"));
        assert_eq!(parse_bearer("usb-abc"), Some("usb-abc"));
        assert_eq!(parse_bearer("Basic dXNlcjpwdw=="), None);
        assert_eq!(parse_bearer("Bearer "), None);
        assert_eq!(parse_bearer("Bearer"), None);
        assert_eq!(parse_bearer("bearer  "), None);
        assert_eq!(parse_bearer("   "), None);
    }

    #[test]
    fn ctx_clamping_respects_floor_and_trained_cap() {
        assert_eq!(clamp_ctx_tokens(512, None), MIN_BRIDGE_CTX_TOKENS);
        assert_eq!(clamp_ctx_tokens(16_384, Some(8_192)), 8_192);
        assert_eq!(clamp_ctx_tokens(16_384, Some(131_072)), 16_384);
        assert_eq!(clamp_ctx_tokens(16_384, Some(0)), 16_384);
        assert_eq!(clamp_ctx_tokens(16_384, None), 16_384);
    }
}
