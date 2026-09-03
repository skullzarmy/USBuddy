use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum UsbBuddyError {
    #[error("unsupported schema for {kind}: expected {expected}, got {found}")]
    UnsupportedSchema {
        kind: &'static str,
        expected: &'static str,
        found: String,
    },
    #[error("missing required path: {0}")]
    MissingPath(PathBuf),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("download canceled")]
    Canceled,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    TomlSerialize(#[from] toml::ser::Error),
    #[error(transparent)]
    TomlDeserialize(#[from] toml::de::Error),
    #[error(transparent)]
    Semver(#[from] semver::Error),
}

pub type Result<T> = std::result::Result<T, UsbBuddyError>;
