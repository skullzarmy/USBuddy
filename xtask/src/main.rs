//! USBuddy maintainer tasks.
//!
//! Today this is just `catalog-fetch`: given a TOML seed file describing
//! `(model_id, hf_repo, gguf_path, …)` entries, the task queries Hugging
//! Face for each file's Git LFS pointer (`https://huggingface.co/<repo>/raw/<ref>/<path>`)
//! to harvest the SHA256 + size without downloading a single GGUF byte,
//! then emits a fully-populated `official.catalog.json` matching the
//! `usbuddy.catalog/v1` schema.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "USBuddy maintainer tasks")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Populate a catalog by fetching SHA256 + size from HuggingFace LFS pointers.
    CatalogFetch {
        /// Path to the TOML seed file (see fixtures/catalog/seed.toml for an example).
        #[arg(long, default_value = "fixtures/catalog/seed.toml")]
        seed: PathBuf,
        /// Where to write the populated catalog JSON.
        #[arg(long, default_value = "fixtures/catalog/official.catalog.json")]
        out: PathBuf,
        /// HuggingFace Hub base URL (override for staging or a mirror).
        #[arg(long, default_value = "https://huggingface.co")]
        hf_base: String,
        /// Optional HF token, sent as `Authorization: Bearer …` for gated models.
        #[arg(long, env = "HF_TOKEN")]
        hf_token: Option<String>,
    },
}

// --------------------------------------------------------------------
// Seed file schema (TOML)
// --------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SeedFile {
    catalog: SeedCatalog,
    #[serde(default)]
    advisories: Vec<SeedAdvisory>,
    #[serde(default)]
    models: Vec<SeedModel>,
}

#[derive(Debug, Deserialize)]
struct SeedCatalog {
    runtime_min: String,
    runtime_max: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct SeedAdvisory {
    id: String,
    severity: String,
    summary: String,
    recommended_action: String,
    #[serde(default)]
    affects: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct SeedModel {
    id: String,
    family_id: String,
    display_name: String,
    version: String,
    /// HuggingFace repo, e.g. `bartowski/Meta-Llama-3.1-8B-Instruct-GGUF`.
    hf_repo: String,
    /// File path within the repo, e.g. `Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf`.
    hf_path: String,
    /// HF revision (branch / tag / commit). Defaults to `main`.
    #[serde(default = "default_ref")]
    hf_ref: String,
    prompt_template: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    profile: String,
    license: SeedLicense,
    /// If true the entry is emitted with `auth: { type: "hf_token", gate_url }`.
    #[serde(default)]
    gated: bool,
    #[serde(default)]
    gate_url: Option<String>,
}

fn default_ref() -> String {
    "main".into()
}

#[derive(Debug, Deserialize, Clone)]
struct SeedLicense {
    spdx: String,
    title: String,
    url: String,
    /// SHA256 of the license text. Required by the schema, but verifying it
    /// is out of scope here — the maintainer pastes the upstream-published
    /// hash. Pre-fill with zeros and fix up via a future `xtask
    /// license-fetch` if needed.
    sha256: String,
    #[serde(default)]
    requires_attribution: bool,
}

// --------------------------------------------------------------------
// Catalog JSON schema (mirrors `usbuddy-core::catalog`).
// --------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Catalog {
    schema: String,
    generated_at: String,
    source: String,
    runtime: RuntimeBounds,
    models: Vec<ModelEntry>,
    advisories: Vec<Advisory>,
}

#[derive(Debug, Serialize)]
struct RuntimeBounds {
    min: String,
    max: String,
}

#[derive(Debug, Serialize)]
struct ModelEntry {
    id: String,
    family_id: String,
    display_name: String,
    version: String,
    file_name: String,
    sha256: String,
    size_bytes: u64,
    prompt_template: String,
    capabilities: Vec<String>,
    aliases: Vec<String>,
    profile: String,
    license: LicenseInfo,
    source: CatalogSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<ModelAuth>,
}

#[derive(Debug, Serialize)]
struct LicenseInfo {
    spdx: String,
    title: String,
    url: String,
    sha256: String,
    requires_attribution: bool,
}

#[derive(Debug, Serialize)]
struct CatalogSource {
    kind: String,
    url: String,
}

#[derive(Debug, Serialize)]
struct ModelAuth {
    #[serde(rename = "type")]
    auth_type: String,
    gate_url: String,
}

#[derive(Debug, Serialize)]
struct Advisory {
    id: String,
    severity: String,
    summary: String,
    recommended_action: String,
    affects: Vec<String>,
}

// --------------------------------------------------------------------
// HF LFS pointer fetching
// --------------------------------------------------------------------

/// What we extract from an HF LFS pointer file.
#[derive(Debug)]
struct LfsPointer {
    sha256: String,
    size: u64,
}

fn fetch_lfs_pointer(
    client: &Client,
    hf_base: &str,
    repo: &str,
    refname: &str,
    path: &str,
    token: Option<&str>,
) -> Result<LfsPointer> {
    // The /raw/ endpoint returns the LFS pointer text (not the file contents)
    // for any LFS-tracked file.
    let url = format!(
        "{}/{}/raw/{}/{}",
        hf_base.trim_end_matches('/'),
        repo,
        refname,
        path
    );

    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().with_context(|| format!("read body of {url}"))?;

    if !status.is_success() {
        bail!("HTTP {status} fetching {url}\nbody: {body}");
    }

    parse_lfs_pointer(&body)
        .ok_or_else(|| anyhow!("file at {url} did not look like an LFS pointer:\n{body}"))
}

fn parse_lfs_pointer(text: &str) -> Option<LfsPointer> {
    // Pointer format:
    //   version https://git-lfs.github.com/spec/v1
    //   oid sha256:abc...
    //   size 1234
    let mut sha = None;
    let mut size = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("oid sha256:") {
            sha = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("size ") {
            size = rest.trim().parse::<u64>().ok();
        }
    }
    match (sha, size) {
        (Some(sha), Some(size)) => Some(LfsPointer { sha256: sha, size }),
        _ => None,
    }
}

// --------------------------------------------------------------------
// Catalog assembly
// --------------------------------------------------------------------

fn build_catalog(seed: SeedFile, fetched: BTreeMap<String, LfsPointer>) -> Catalog {
    let models = seed
        .models
        .into_iter()
        .map(|m| {
            let lfs = fetched
                .get(&m.id)
                .expect("every model must be fetched before build_catalog");
            let source_url = format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                m.hf_repo, m.hf_ref, m.hf_path
            );
            let auth = if m.gated {
                Some(ModelAuth {
                    auth_type: "hf_token".into(),
                    gate_url: m
                        .gate_url
                        .clone()
                        .unwrap_or_else(|| format!("https://huggingface.co/{}", m.hf_repo)),
                })
            } else {
                None
            };
            let file_name = Path::new(&m.hf_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&m.hf_path)
                .to_string();
            ModelEntry {
                id: m.id,
                family_id: m.family_id,
                display_name: m.display_name,
                version: m.version,
                file_name,
                sha256: lfs.sha256.clone(),
                size_bytes: lfs.size,
                prompt_template: m.prompt_template,
                capabilities: m.capabilities,
                aliases: m.aliases,
                profile: m.profile,
                license: LicenseInfo {
                    spdx: m.license.spdx,
                    title: m.license.title,
                    url: m.license.url,
                    sha256: m.license.sha256,
                    requires_attribution: m.license.requires_attribution,
                },
                source: CatalogSource {
                    kind: "official".into(),
                    url: source_url,
                },
                auth,
            }
        })
        .collect();

    let advisories = seed
        .advisories
        .into_iter()
        .map(|a| Advisory {
            id: a.id,
            severity: a.severity,
            summary: a.summary,
            recommended_action: a.recommended_action,
            affects: a.affects,
        })
        .collect();

    Catalog {
        schema: "usbuddy.catalog/v1".into(),
        generated_at: chrono_now_iso(),
        source: seed.catalog.source,
        runtime: RuntimeBounds {
            min: seed.catalog.runtime_min,
            max: seed.catalog.runtime_max,
        },
        models,
        advisories,
    }
}

/// We don't want to pull in chrono just for this; format current UTC time
/// using std + a tiny calendar implementation. Good enough for "machine
/// generated" provenance and is recomputed every run.
fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    // Days since 1970-01-01.
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (h, rem) = (secs_of_day / 3600, secs_of_day % 3600);
    let (m, s) = (rem / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mon = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if mon <= 2 { y + 1 } else { y };
    format!("{year:04}-{mon:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// --------------------------------------------------------------------
// Main
// --------------------------------------------------------------------

fn run_catalog_fetch(
    seed_path: PathBuf,
    out_path: PathBuf,
    hf_base: String,
    hf_token: Option<String>,
) -> Result<()> {
    let seed_text = fs::read_to_string(&seed_path)
        .with_context(|| format!("read seed file {}", seed_path.display()))?;
    let seed: SeedFile =
        toml::from_str(&seed_text).with_context(|| format!("parse {}", seed_path.display()))?;
    if seed.models.is_empty() {
        bail!("seed file lists no models");
    }

    let client = Client::builder()
        .user_agent(concat!("usbuddy-xtask/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut fetched = BTreeMap::new();
    for model in &seed.models {
        eprintln!(
            "→ fetching LFS pointer for {} from {}/{}@{}",
            model.id, model.hf_repo, model.hf_path, model.hf_ref
        );
        let lfs = fetch_lfs_pointer(
            &client,
            &hf_base,
            &model.hf_repo,
            &model.hf_ref,
            &model.hf_path,
            hf_token.as_deref(),
        )
        .with_context(|| format!("model '{}'", model.id))?;
        eprintln!("  sha256={} size={}", lfs.sha256, lfs.size);
        fetched.insert(model.id.clone(), lfs);
    }

    let catalog = build_catalog(seed, fetched);
    let json = serde_json::to_string_pretty(&catalog)? + "\n";
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out_path, json)?;
    eprintln!("✓ wrote {}", out_path.display());
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::CatalogFetch {
            seed,
            out,
            hf_base,
            hf_token,
        } => run_catalog_fetch(seed, out, hf_base, hf_token),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_lfs_pointer() {
        let text = "version https://git-lfs.github.com/spec/v1\noid sha256:deadbeef\nsize 1234\n";
        let p = parse_lfs_pointer(text).expect("should parse");
        assert_eq!(p.sha256, "deadbeef");
        assert_eq!(p.size, 1234);
    }

    #[test]
    fn rejects_non_lfs_response() {
        // A plain HTML page or git blob has neither oid nor size lines.
        assert!(parse_lfs_pointer("<html><body>not a pointer</body></html>").is_none());
    }
}
