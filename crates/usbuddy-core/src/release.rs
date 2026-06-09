use std::{fs, path::Path};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{Result, UsbBuddyError};

pub const RELEASE_SCHEMA_V1: &str = "usbuddy.release/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub schema: String,
    pub version: Version,
    pub released_at: String,
    pub channel: String,
    #[serde(default)]
    pub changelog: String,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub sha256: String,
    pub url: String,
}

impl ReleaseManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema != RELEASE_SCHEMA_V1 {
            return Err(UsbBuddyError::UnsupportedSchema {
                kind: "release-manifest",
                expected: RELEASE_SCHEMA_V1,
                found: self.schema.clone(),
            });
        }
        if self.assets.is_empty() {
            return Err(UsbBuddyError::InvalidState(
                "release manifest must contain at least one asset".into(),
            ));
        }
        Ok(())
    }

    pub fn is_newer_than(&self, current: &Version) -> bool {
        self.version > *current
    }
}

pub fn load_release_manifest(path: &Path) -> Result<ReleaseManifest> {
    let manifest: ReleaseManifest = serde_json::from_str(&fs::read_to_string(path)?)?;
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::{RELEASE_SCHEMA_V1, ReleaseAsset, ReleaseManifest};

    #[test]
    fn compares_versions() {
        let manifest = ReleaseManifest {
            schema: RELEASE_SCHEMA_V1.into(),
            version: semver::Version::parse("0.2.0").unwrap(),
            released_at: "2026-06-09T00:00:00Z".into(),
            channel: "stable".into(),
            changelog: String::new(),
            assets: vec![ReleaseAsset {
                name: "bundle".into(),
                sha256: "a".repeat(64),
                url: "https://example.invalid".into(),
            }],
        };

        assert!(manifest.is_newer_than(&semver::Version::parse("0.1.0").unwrap()));
    }
}
