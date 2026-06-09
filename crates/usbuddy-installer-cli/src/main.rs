use std::{fs, path::PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use semver::Version;
use serde::Serialize;
use usbuddy_core::{
    catalog::load_catalog,
    compiled_version,
    layout::DriveLayout,
    license::{LicensePrefs, LicenseScope},
    platform::detect_platform,
    ram::{RamEstimateInput, assess_fit},
    release::load_release_manifest,
};

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
    Inspect {
        drive: PathBuf,
    },
    Init {
        drive: PathBuf,
        version: String,
        #[arg(long)]
        catalog: Option<PathBuf>,
    },
    DiscoverModels {
        drive: PathBuf,
    },
    Rollback {
        drive: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    Validate {
        path: PathBuf,
        #[arg(long)]
        runtime: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    Validate {
        path: PathBuf,
        #[arg(long)]
        current: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum LicenseCommand {
    ShowPrefs { drive: PathBuf },
    SetPrefs { drive: PathBuf, scope: ScopeArg },
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
    }
    Ok(())
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
