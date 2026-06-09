use std::{fs, path::PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use semver::Version;
use serde::Serialize;
use usbuddy_core::{
    catalog::load_catalog,
    compiled_version,
    download::download_verified,
    engine::{
        DEFAULT_LLAMA_TAG, EngineSelection, install_engines, report_status as engine_report_status,
    },
    layout::DriveLayout,
    license::{LicensePrefs, LicenseScope},
    platform::detect_platform,
    ram::{RamEstimateInput, assess_fit},
    release::load_release_manifest,
};

/// Default URL from which the catalog snapshot is fetched.
const DEFAULT_CATALOG_URL: &str =
    "https://github.com/skullzarmy/USBuddy/releases/latest/download/official.catalog.json";

/// GitHub Releases API endpoint for the USBuddy installer.
const RELEASE_API_URL: &str = "https://api.github.com/repos/skullzarmy/USBuddy/releases/latest";

#[derive(Debug, Parser)]
#[command(name = "usbuddy-installer-cli", version = compiled_version(), about = "USBuddy installer and maintenance CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Drive {
        #[command(subcommand)]
        command: DriveCommand,
    },
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
    Release {
        #[command(subcommand)]
        command: ReleaseCommand,
    },
    License {
        #[command(subcommand)]
        command: LicenseCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Update {
        #[command(subcommand)]
        command: UpdateCommand,
    },
    /// Manage the drive-local llama.cpp inference engine.
    Engine {
        #[command(subcommand)]
        command: EngineCommand,
    },
    /// Copy the locally-built usbuddy-runtime binary onto the drive for
    /// the current host platform. Needed because there is no published
    /// USBuddy release yet — once releases exist, prefer `update stage`.
    InstallRuntime {
        drive: PathBuf,
        /// Path to the usbuddy-runtime binary to copy. Defaults to the
        /// release-mode build from the current workspace.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    RamAssess {
        available_gb: f64,
        model_gb: f64,
        #[arg(long, default_value_t = 4096)]
        context_tokens: u32,
        #[arg(long, default_value_t = 131_072)]
        kv_bytes_per_token: u64,
        #[arg(long, default_value_t = 1.5)]
        overhead_gb: f64,
    },
}

#[derive(Debug, Subcommand)]
enum DriveCommand {
    /// Print the current state of a USBuddy drive.
    Inspect { drive: PathBuf },
    /// Initialise the shadow-tree layout on a drive for a given runtime version.
    Init {
        drive: PathBuf,
        version: String,
        #[arg(long)]
        catalog: Option<PathBuf>,
    },
    /// List drop-in .gguf model files discovered on the drive.
    DiscoverModels { drive: PathBuf },
    /// Roll back the active runtime to the previous version.
    Rollback { drive: PathBuf },
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    /// Validate a catalog file against the schema.
    Validate {
        path: PathBuf,
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Download a fresh catalog snapshot from the upstream URL and save it to the drive.
    Refresh {
        drive: PathBuf,
        /// Override the default catalog URL.
        #[arg(long, default_value = DEFAULT_CATALOG_URL)]
        url: String,
    },
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    /// Validate a release manifest file.
    Validate {
        path: PathBuf,
        #[arg(long)]
        current: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum LicenseCommand {
    /// Print license preferences stored on the drive.
    ShowPrefs { drive: PathBuf },
    /// Write license preferences to the drive.
    SetPrefs { drive: PathBuf, scope: ScopeArg },
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Download a model from the catalog onto the drive.
    Download {
        drive: PathBuf,
        /// Catalog model id (e.g. llama-3.1-8b-instruct-q4_k_m).
        model_id: String,
        /// Override the download URL (defaults to the catalog entry's source URL).
        #[arg(long)]
        url: Option<String>,
    },
    /// Remove a downloaded model from the drive.
    Remove { drive: PathBuf, model_id: String },
}

#[derive(Debug, Subcommand)]
enum UpdateCommand {
    /// Check whether a newer runtime is available.
    Check { drive: PathBuf },
    /// Download and stage a new runtime version (does not activate it yet).
    Stage {
        drive: PathBuf,
        /// Target version to stage. Defaults to latest.
        #[arg(long)]
        version: Option<String>,
        /// Base URL for runtime asset downloads.
        #[arg(
            long,
            default_value = "https://github.com/skullzarmy/USBuddy/releases/download"
        )]
        base_url: String,
    },
    /// Activate a staged runtime version.
    Activate { drive: PathBuf, version: String },
    /// Roll back to the previous runtime version.
    Rollback { drive: PathBuf },
}

#[derive(Debug, Subcommand)]
enum EngineCommand {
    /// Print install status for every supported engine target.
    Status { drive: PathBuf },
    /// Download and extract the llama.cpp inference engine onto the drive.
    ///
    /// `selection` is one of:
    ///   * `all`               — provision every supported target (recommended).
    ///   * `host`              — provision only the platform you're on right now.
    ///   * `<os>-<arch>`       — e.g. `linux-x64`, `windows-arm64`, `macos-arm64`.
    Install {
        drive: PathBuf,
        #[arg(default_value = "all")]
        selection: String,
        /// llama.cpp release tag to install (defaults to the version USBuddy ships against).
        #[arg(long, default_value = DEFAULT_LLAMA_TAG)]
        tag: String,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum ScopeArg {
    All,
    PermissiveOnly,
    None,
}

impl From<ScopeArg> for LicenseScope {
    fn from(value: ScopeArg) -> Self {
        match value {
            ScopeArg::All => LicenseScope::All,
            ScopeArg::PermissiveOnly => LicenseScope::PermissiveOnly,
            ScopeArg::None => LicenseScope::None,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Drive { command } => match command {
            DriveCommand::Inspect { drive } => {
                let layout = DriveLayout::new(drive);
                let current = layout.read_current().ok();
                print_json(&DriveInspection {
                    root: layout.root().display().to_string(),
                    initialized: layout.is_initialized(),
                    current,
                    platform: detect_platform(),
                })?;
            }
            DriveCommand::Init {
                drive,
                version,
                catalog,
            } => {
                let layout = DriveLayout::new(&drive);
                layout.initialize_structure(&version)?;
                if let Some(catalog) = catalog {
                    fs::copy(catalog, layout.catalog_path())?;
                }
                print_json(&layout.read_current()?)?;
            }
            DriveCommand::DiscoverModels { drive } => {
                let layout = DriveLayout::new(drive);
                print_json(&layout.discover_drop_in_models()?)?;
            }
            DriveCommand::Rollback { drive } => {
                let layout = DriveLayout::new(drive);
                print_json(&layout.rollback()?)?;
            }
        },

        Commands::Catalog { command } => match command {
            CatalogCommand::Validate { path, runtime } => {
                let catalog = load_catalog(&path)?;
                let supported = runtime
                    .as_deref()
                    .map(Version::parse)
                    .transpose()?
                    .map(|version| catalog.supports_runtime(&version));
                print_json(&serde_json::json!({
                    "models": catalog.models.len(),
                    "advisories": catalog.advisories.len(),
                    "family_groups": catalog.group_by_family().len(),
                    "runtime_supported": supported,
                }))?;
            }
            CatalogCommand::Refresh { drive, url } => {
                let layout = DriveLayout::new(&drive);
                let dest = layout.catalog_path();
                eprintln!("Fetching catalog from {url} …");
                let sha =
                    download_verified(&url, &dest, None).context("failed to download catalog")?;
                let catalog =
                    load_catalog(&dest).context("downloaded catalog failed validation")?;
                print_json(&serde_json::json!({
                    "sha256": sha,
                    "models": catalog.models.len(),
                    "advisories": catalog.advisories.len(),
                }))?;
            }
        },

        Commands::Release { command } => match command {
            ReleaseCommand::Validate { path, current } => {
                let manifest = load_release_manifest(&path)?;
                let newer_than = current
                    .as_deref()
                    .map(Version::parse)
                    .transpose()?
                    .map(|version| manifest.is_newer_than(&version));
                print_json(&serde_json::json!({
                    "version": manifest.version,
                    "assets": manifest.assets.len(),
                    "newer_than_current": newer_than,
                }))?;
            }
        },

        Commands::License { command } => match command {
            LicenseCommand::ShowPrefs { drive } => {
                let layout = DriveLayout::new(drive);
                print_json(&LicensePrefs::read_from(&layout.license_prefs_path())?)?;
            }
            LicenseCommand::SetPrefs { drive, scope } => {
                let layout = DriveLayout::new(drive);
                let prefs = LicensePrefs {
                    scope: scope.into(),
                };
                prefs.write_to(&layout.license_prefs_path())?;
                print_json(&prefs)?;
            }
        },

        Commands::Model { command } => match command {
            ModelCommand::Download {
                drive,
                model_id,
                url,
            } => {
                let layout = DriveLayout::new(&drive);
                let catalog_path = layout.catalog_path();
                if !catalog_path.exists() {
                    anyhow::bail!(
                        "No catalog found at {}. Run `catalog refresh` first.",
                        catalog_path.display()
                    );
                }
                let catalog = load_catalog(&catalog_path)?;
                let entry = catalog
                    .models
                    .iter()
                    .find(|m| m.id == model_id || m.aliases.contains(&model_id))
                    .ok_or_else(|| anyhow::anyhow!("model '{}' not found in catalog", model_id))?;

                let download_url = url.as_deref().unwrap_or(&entry.source.url);
                let dest = layout.models_dir().join(&entry.file_name);
                eprintln!(
                    "Downloading {} ({:.1} GiB) …",
                    entry.display_name,
                    entry.size_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
                );
                let sha = download_verified(download_url, &dest, Some(&entry.sha256))
                    .context("model download failed")?;
                print_json(&serde_json::json!({
                    "model_id": entry.id,
                    "file": dest.display().to_string(),
                    "sha256": sha,
                }))?;
            }
            ModelCommand::Remove { drive, model_id } => {
                let layout = DriveLayout::new(&drive);
                let catalog_path = layout.catalog_path();
                let file_name = if catalog_path.exists() {
                    let catalog = load_catalog(&catalog_path)?;
                    catalog
                        .models
                        .iter()
                        .find(|m| m.id == model_id || m.aliases.contains(&model_id))
                        .map(|e| e.file_name.clone())
                } else {
                    None
                };
                let file_name = file_name.unwrap_or_else(|| format!("{model_id}.gguf"));
                let target = layout.models_dir().join(&file_name);
                if target.exists() {
                    fs::remove_file(&target)?;
                    print_json(&serde_json::json!({ "removed": target.display().to_string() }))?;
                } else {
                    anyhow::bail!("Model file not found: {}", target.display());
                }
            }
        },

        Commands::Update { command } => match command {
            UpdateCommand::Check { drive } => {
                let layout = DriveLayout::new(&drive);
                let current = layout.read_current().ok();
                let active_version = current
                    .as_ref()
                    .map(|c| c.active.as_str())
                    .unwrap_or("none");

                eprintln!("Checking for updates (current: {active_version}) …");
                let release_info = fetch_latest_release_info()?;
                let latest = release_info["tag_name"]
                    .as_str()
                    .unwrap_or("")
                    .trim_start_matches('v');

                let is_newer = if active_version == "none" {
                    true
                } else {
                    let cur = Version::parse(active_version)?;
                    let lat = Version::parse(latest)?;
                    lat > cur
                };

                print_json(&serde_json::json!({
                    "current": active_version,
                    "latest": latest,
                    "update_available": is_newer,
                    "release_url": release_info["html_url"],
                }))?;
            }
            UpdateCommand::Stage {
                drive,
                version,
                base_url,
            } => {
                let layout = DriveLayout::new(&drive);

                let target_version = match version {
                    Some(v) => v,
                    None => {
                        let info = fetch_latest_release_info()?;
                        info["tag_name"]
                            .as_str()
                            .unwrap_or("")
                            .trim_start_matches('v')
                            .to_string()
                    }
                };

                let manifest_url = format!("{base_url}/v{target_version}/release-manifest.json");
                eprintln!("Fetching release manifest for v{target_version} …");
                let tmp_manifest_dir = tempfile::tempdir()?;
                let manifest_path = tmp_manifest_dir.path().join("release-manifest.json");
                download_verified(&manifest_url, &manifest_path, None)
                    .context("failed to download release manifest")?;
                let manifest = load_release_manifest(&manifest_path)?;

                let platform = detect_platform();
                let asset = manifest
                    .assets
                    .iter()
                    .find(|a| {
                        a.platform.as_deref() == Some(&platform.os)
                            || a.platform.as_deref() == Some("all")
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("No asset in manifest for platform '{}'", platform.os)
                    })?;

                let staged_dir = layout
                    .root()
                    .join("versions")
                    .join(format!("{}.tmp", target_version));
                let asset_url = format!("{base_url}/v{target_version}/{}", asset.file_name);
                let asset_dest = staged_dir.join(&asset.file_name);

                eprintln!("Downloading runtime asset {} …", asset.file_name);
                download_verified(&asset_url, &asset_dest, Some(&asset.sha256))
                    .context("runtime asset download failed")?;

                // Write the manifest into the staged tree.
                let manifest_dest = staged_dir.join("version.json");
                fs::copy(&manifest_path, &manifest_dest)?;

                print_json(&serde_json::json!({
                    "staged_version": target_version,
                    "staged_dir": staged_dir.display().to_string(),
                    "next_step": format!("run `update activate --drive {} {}`", drive.display(), target_version),
                }))?;
            }
            UpdateCommand::Activate { drive, version } => {
                let layout = DriveLayout::new(&drive);
                let staged_dir = layout
                    .root()
                    .join("versions")
                    .join(format!("{version}.tmp"));
                let final_dir = layout.root().join("versions").join(&version);

                if !staged_dir.exists() {
                    anyhow::bail!(
                        "No staged version found at {}. Run `update stage` first.",
                        staged_dir.display()
                    );
                }
                fs::rename(&staged_dir, &final_dir)?;
                let current = layout.rollback_to(&version)?;
                print_json(&current)?;
            }
            UpdateCommand::Rollback { drive } => {
                let layout = DriveLayout::new(drive);
                print_json(&layout.rollback()?)?;
            }
        },

        Commands::RamAssess {
            available_gb,
            model_gb,
            context_tokens,
            kv_bytes_per_token,
            overhead_gb,
        } => {
            let snapshot = usbuddy_core::ram::MemorySnapshot {
                total_bytes: (available_gb * 1024.0 * 1024.0 * 1024.0) as u64,
                available_bytes: (available_gb * 1024.0 * 1024.0 * 1024.0) as u64,
            };
            let decision = assess_fit(
                snapshot,
                RamEstimateInput {
                    model_bytes: (model_gb * 1024.0 * 1024.0 * 1024.0) as u64,
                    context_tokens,
                    kv_bytes_per_token,
                    runtime_overhead_bytes: (overhead_gb * 1024.0 * 1024.0 * 1024.0) as u64,
                },
            );
            print_json(&decision)?;
        }

        Commands::Engine { command } => match command {
            EngineCommand::Status { drive } => {
                let layout = DriveLayout::new(drive);
                let current = layout
                    .read_current()
                    .with_context(|| "drive is not initialised")?;
                print_json(&engine_report_status(&layout, &current.active))?;
            }
            EngineCommand::Install {
                drive,
                selection,
                tag,
            } => {
                let layout = DriveLayout::new(drive);
                let current = layout
                    .read_current()
                    .with_context(|| "drive is not initialised")?;
                let sel = EngineSelection::parse(&selection)?;
                let installed = install_engines(&layout, &current.active, &sel, &tag, |line| {
                    eprintln!("{line}");
                })?;
                print_json(&installed)?;
            }
        },

        Commands::InstallRuntime { drive, from } => {
            let layout = DriveLayout::new(drive);
            let current = layout
                .read_current()
                .with_context(|| "drive is not initialised")?;
            let platform = detect_platform();
            let arch = match platform.arch.as_str() {
                "x86_64" => "x64".to_string(),
                "aarch64" => "arm64".to_string(),
                other => other.to_string(),
            };
            let bin_name = if platform.os == "windows" {
                "usbuddy-runtime.exe"
            } else {
                "usbuddy-runtime"
            };
            let source = from.unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|workspace| workspace.join("target").join("release").join(bin_name))
                    .unwrap_or_else(|| PathBuf::from(format!("target/release/{bin_name}")))
            });
            if !source.exists() {
                anyhow::bail!(
                    "runtime binary not found at {} — run `cargo build --release -p usbuddy-runtime` first or pass --from",
                    source.display()
                );
            }
            let dest_dir = layout
                .version_dir(&current.active)
                .join("bin")
                .join(format!("{}-{arch}", platform.os));
            fs::create_dir_all(&dest_dir)?;
            let dest = dest_dir.join(bin_name);
            fs::copy(&source, &dest)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&dest)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&dest, perms)?;
            }
            eprintln!("✓ Installed runtime to {}", dest.display());
            print_json(
                &serde_json::json!({ "runtime": dest, "platform": format!("{}-{arch}", platform.os) }),
            )?;
        }
    }
    Ok(())
}

fn fetch_latest_release_info() -> anyhow::Result<serde_json::Value> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("usbuddy/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let info: serde_json::Value = client
        .get(RELEASE_API_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .context("failed to fetch release info from GitHub API")?;
    Ok(info)
}

#[derive(Debug, Serialize)]
struct DriveInspection {
    root: String,
    initialized: bool,
    current: Option<usbuddy_core::layout::CurrentVersionPointer>,
    platform: usbuddy_core::platform::PlatformInfo,
}

fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("failed to encode JSON output")?
    );
    Ok(())
}
