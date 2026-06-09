use std::{fs, io::Write, path::Path};

use serde::Serialize;
use tempfile::NamedTempFile;

use crate::error::Result;

pub fn atomic_write_string(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    temp.flush()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    atomic_write_string(path, &format!("{content}\n"))
}
