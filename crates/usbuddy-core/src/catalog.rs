use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{Result, UsbBuddyError};

pub const CATALOG_SCHEMA_V1: &str = "usbuddy.catalog/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub schema: String,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    pub runtime: RuntimeCompatibility,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    #[serde(default)]
    pub advisories: Vec<Advisory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCompatibility {
    pub min: Version,
    pub max: Version,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub family_id: String,
    pub display_name: String,
    pub version: String,
    pub file_name: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub prompt_template: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub profile: String,
    pub license: LicenseInfo,
    pub source: CatalogSource,
    #[serde(default)]
    pub auth: Option<ModelAuth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseInfo {
    pub spdx: String,
    pub title: String,
    pub url: String,
    pub sha256: String,
    pub requires_attribution: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSource {
    pub kind: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAuth {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub gate_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Advisory {
    pub id: String,
    pub severity: String,
    pub summary: String,
    pub recommended_action: String,
    pub affects: AdvisoryAffects,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdvisoryAffects {
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub runtime_versions: Vec<String>,
    #[serde(default, rename = "llama_server")]
    pub llama_server: Vec<String>,
}

impl Catalog {
    pub fn validate(&self) -> Result<()> {
        if self.schema != CATALOG_SCHEMA_V1 {
            return Err(UsbBuddyError::UnsupportedSchema {
                kind: "catalog",
                expected: CATALOG_SCHEMA_V1,
                found: self.schema.clone(),
            });
        }

        let mut ids = BTreeSet::new();
        let mut aliases = BTreeSet::new();
        for model in &self.models {
            if !ids.insert(model.id.clone()) {
                return Err(UsbBuddyError::InvalidState(format!(
                    "duplicate model id '{}'",
                    model.id
                )));
            }
            if aliases.contains(&model.id) {
                return Err(UsbBuddyError::InvalidState(format!(
                    "alias collision detected for '{}'",
                    model.id
                )));
            }
            if model.family_id.trim().is_empty() {
                return Err(UsbBuddyError::InvalidState(format!(
                    "model '{}' is missing family_id",
                    model.id
                )));
            }
            if model.sha256.len() != 64 {
                return Err(UsbBuddyError::InvalidState(format!(
                    "model '{}' does not have a 64-character sha256",
                    model.id
                )));
            }
            for alias in &model.aliases {
                if ids.contains(alias) || !aliases.insert(alias.clone()) {
                    return Err(UsbBuddyError::InvalidState(format!(
                        "alias collision detected for '{}'",
                        alias
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn supports_runtime(&self, runtime_version: &Version) -> bool {
        runtime_version >= &self.runtime.min && runtime_version <= &self.runtime.max
    }

    pub fn group_by_family(&self) -> BTreeMap<&str, Vec<&ModelEntry>> {
        let mut grouped: BTreeMap<&str, Vec<&ModelEntry>> = BTreeMap::new();
        for model in &self.models {
            grouped.entry(&model.family_id).or_default().push(model);
        }
        grouped
    }
}

pub fn load_catalog(path: &Path) -> Result<Catalog> {
    let catalog: Catalog = serde_json::from_str(&fs::read_to_string(path)?)?;
    catalog.validate()?;
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::load_catalog;

    #[test]
    fn loads_fixture_catalog() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/catalog/official.catalog.json");
        let catalog = load_catalog(&root).unwrap();
        assert!(
            !catalog.models.is_empty(),
            "fixture catalog should have at least one model"
        );
        assert!(!catalog.group_by_family().is_empty());
    }
}
