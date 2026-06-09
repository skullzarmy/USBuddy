use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{atomic::atomic_write_string, error::Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LicenseScope {
    All,
    PermissiveOnly,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LicensePrefs {
    pub scope: LicenseScope,
}

impl Default for LicensePrefs {
    fn default() -> Self {
        Self {
            scope: LicenseScope::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LicenseAcceptanceRecord {
    pub model_id: String,
    pub license_sha256: String,
    pub timestamp: String,
    pub host_at_accept: String,
}

impl LicensePrefs {
    pub fn read_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        atomic_write_string(path, &toml::to_string_pretty(self)?)
    }
}

pub fn append_acceptance_record(path: &Path, record: &LicenseAcceptanceRecord) -> Result<()> {
    let mut existing = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    existing.push_str(&serde_json::to_string(record)?);
    existing.push('\n');
    atomic_write_string(path, &existing)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{LicenseAcceptanceRecord, LicensePrefs, LicenseScope, append_acceptance_record};

    #[test]
    fn round_trips_license_prefs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        let prefs = LicensePrefs {
            scope: LicenseScope::PermissiveOnly,
        };
        prefs.write_to(&path).unwrap();
        assert_eq!(LicensePrefs::read_from(&path).unwrap(), prefs);
    }

    #[test]
    fn appends_acceptance_records_as_jsonl() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("acceptance.jsonl");
        append_acceptance_record(
            &path,
            &LicenseAcceptanceRecord {
                model_id: "test".into(),
                license_sha256: "a".repeat(64),
                timestamp: "now".into(),
                host_at_accept: "host".into(),
            },
        )
        .unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("\"model_id\":\"test\""));
    }
}
