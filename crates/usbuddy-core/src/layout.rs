use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{
    atomic::atomic_write_json,
    error::{Result, UsbBuddyError},
    license::{LicensePrefs, LicenseScope},
};

pub const CURRENT_SCHEMA_V1: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CurrentVersionPointer {
    pub schema: u32,
    pub active: String,
    pub previous: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionMetadata {
    pub schema: u32,
    pub version: String,
    pub released: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DropInModel {
    pub path: PathBuf,
    pub display_name: String,
    pub profile: String,
}

#[derive(Debug, Clone)]
pub struct DriveLayout {
    root: PathBuf,
}

impl DriveLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn current_path(&self) -> PathBuf {
        self.root.join("current.json")
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.root.join("catalog.json")
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn version_dir(&self, version: &str) -> PathBuf {
        self.versions_dir().join(version)
    }

    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn local_catalog_path(&self) -> PathBuf {
        self.models_dir().join("catalog.local.json")
    }

    pub fn usbuddy_dir(&self) -> PathBuf {
        self.root.join(".usbuddy")
    }

    pub fn trust_dir(&self) -> PathBuf {
        self.usbuddy_dir().join("trust")
    }

    pub fn license_prefs_path(&self) -> PathBuf {
        self.usbuddy_dir().join("license-prefs.toml")
    }

    pub fn advisories_seen_path(&self) -> PathBuf {
        self.usbuddy_dir().join("advisories-seen.json")
    }

    pub fn license_acceptance_log(&self) -> PathBuf {
        self.usbuddy_dir().join("license-acceptance.jsonl")
    }

    pub fn hf_token_path(&self) -> PathBuf {
        self.usbuddy_dir().join("hf-token")
    }

    pub fn initialize_structure(&self, active_version: &str) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(self.versions_dir())?;
        fs::create_dir_all(self.models_dir())?;
        fs::create_dir_all(self.trust_dir())?;
        fs::create_dir_all(self.version_dir(active_version))?;
        if !self.catalog_path().exists() {
            fs::write(
                self.catalog_path(),
                "{\n  \"schema\": \"usbuddy.catalog/v1\",\n  \"runtime\": { \"min\": \"0.1.0\", \"max\": \"0.1.99\" },\n  \"models\": [],\n  \"advisories\": []\n}\n",
            )?;
        }
        if !self.local_catalog_path().exists() {
            fs::write(self.local_catalog_path(), "[]\n")?;
        }
        if !self.advisories_seen_path().exists() {
            fs::write(self.advisories_seen_path(), "[]\n")?;
        }
        if !self.license_prefs_path().exists() {
            LicensePrefs {
                scope: LicenseScope::None,
            }
            .write_to(&self.license_prefs_path())?;
        }
        if !self.root.join("README.txt").exists() {
            fs::write(
                self.root.join("README.txt"),
                "USBuddy portable runtime root. Launch from the platform launcher and keep this drive inserted while the runtime is active.\n",
            )?;
        }
        self.write_current(&CurrentVersionPointer {
            schema: CURRENT_SCHEMA_V1,
            active: active_version.into(),
            previous: None,
        })?;
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.current_path().exists() && self.versions_dir().exists() && self.models_dir().exists()
    }

    pub fn read_current(&self) -> Result<CurrentVersionPointer> {
        if !self.current_path().exists() {
            return Err(UsbBuddyError::MissingPath(self.current_path()));
        }
        Ok(serde_json::from_str(&fs::read_to_string(
            self.current_path(),
        )?)?)
    }

    pub fn write_current(&self, pointer: &CurrentVersionPointer) -> Result<()> {
        atomic_write_json(&self.current_path(), pointer)
    }

    pub fn rollback(&self) -> Result<CurrentVersionPointer> {
        let current = self.read_current()?;
        let previous = current.previous.clone().ok_or_else(|| {
            UsbBuddyError::InvalidState(
                "rollback requested but no previous version recorded".into(),
            )
        })?;
        let next = CurrentVersionPointer {
            schema: CURRENT_SCHEMA_V1,
            active: previous,
            previous: Some(current.active),
        };
        self.write_current(&next)?;
        Ok(next)
    }

    pub fn discover_drop_in_models(&self) -> Result<Vec<DropInModel>> {
        if !self.models_dir().exists() {
            return Ok(Vec::new());
        }
        let mut discovered = Vec::new();
        for entry in WalkDir::new(self.models_dir()).max_depth(2) {
            let entry = entry.map_err(|error| UsbBuddyError::InvalidState(error.to_string()))?;
            if entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
            {
                let display_name = entry
                    .path()
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown-model")
                    .replace('-', " ");
                discovered.push(DropInModel {
                    path: entry.path().to_path_buf(),
                    display_name,
                    profile: "community-unverified".into(),
                });
            }
        }
        Ok(discovered)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::DriveLayout;

    #[test]
    fn creates_expected_layout_and_rolls_back() {
        let dir = tempdir().unwrap();
        let layout = DriveLayout::new(dir.path());
        layout.initialize_structure("0.1.0").unwrap();
        assert!(layout.is_initialized());

        let mut current = layout.read_current().unwrap();
        current.previous = Some("0.0.9".into());
        layout.write_current(&current).unwrap();
        let rolled = layout.rollback().unwrap();
        assert_eq!(rolled.active, "0.0.9");
        assert_eq!(rolled.previous.as_deref(), Some("0.1.0"));
    }
}
