//! Inference-engine provisioning.
//!
//! USBuddy is portable, which means the *inference engine* (`llama-server`
//! from llama.cpp) has to live on the USB drive, not on the host's PATH.
//! This module knows how to fetch the official llama.cpp release archives
//! from GitHub and lay them out at `versions/<v>/bin/<os>-<arch>/` along
//! with their sibling shared libraries so the runtime can exec them on
//! any supported host.
//!
//! Supported targets are the same six that llama.cpp publishes binaries
//! for: macOS arm64/x64, Linux x64/arm64, Windows x64/arm64.

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    download::{DownloadProgress, download_verified_with_progress},
    error::{Result, UsbBuddyError},
    layout::DriveLayout,
};

/// llama.cpp release tag pinned for this USBuddy version. Bump in lock-step
/// with `usbuddy-core`'s `Cargo.toml` version when we want to roll engines
/// forward. Using a pinned tag means the catalog/runtime/engine triple is
/// always reproducible.
pub const DEFAULT_LLAMA_TAG: &str = "b9570";

/// Base URL for the llama.cpp release downloads.
const LLAMA_RELEASE_BASE: &str = "https://github.com/ggml-org/llama.cpp/releases/download";

/// One of the six (os, arch) tuples USBuddy provisions engines for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EngineTarget {
    pub os: &'static str,
    pub arch: &'static str,
}

impl EngineTarget {
    pub const MACOS_ARM64: Self = Self {
        os: "macos",
        arch: "arm64",
    };
    pub const MACOS_X64: Self = Self {
        os: "macos",
        arch: "x86_64",
    };
    pub const LINUX_X64: Self = Self {
        os: "linux",
        arch: "x86_64",
    };
    pub const LINUX_ARM64: Self = Self {
        os: "linux",
        arch: "arm64",
    };
    pub const WINDOWS_X64: Self = Self {
        os: "windows",
        arch: "x86_64",
    };
    pub const WINDOWS_ARM64: Self = Self {
        os: "windows",
        arch: "arm64",
    };

    pub const ALL: [Self; 6] = [
        Self::MACOS_ARM64,
        Self::MACOS_X64,
        Self::LINUX_X64,
        Self::LINUX_ARM64,
        Self::WINDOWS_X64,
        Self::WINDOWS_ARM64,
    ];

    /// Layout-friendly directory name: `<os>-<arch>` with normalised arch
    /// (e.g. `x86_64` collapses to `x64` to match what llama.cpp publishes
    /// and what our launcher scripts already expect).
    pub fn dir_name(self) -> String {
        format!("{}-{}", self.os, normalise_arch(self.arch))
    }

    /// Best-effort match against `std::env::consts::{OS, ARCH}` for the
    /// currently running host. Returns `None` if the host isn't a target
    /// we publish engines for (e.g. FreeBSD).
    pub fn current_host() -> Option<Self> {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        Self::ALL.iter().copied().find(|t| {
            t.os == os && (t.arch == arch || normalise_arch(t.arch) == normalise_arch(arch))
        })
    }

    fn parse(raw: &str) -> Option<Self> {
        let lower = raw.to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|t| t.dir_name() == lower || format!("{}-{}", t.os, t.arch) == lower)
    }

    fn asset_name(self, tag: &str) -> String {
        // llama.cpp uses "ubuntu" for Linux assets, x64 for x86_64,
        // arm64 for aarch64, and ships .zip on Windows / .tar.gz elsewhere.
        let (plat, arch_name, ext) = match (self.os, normalise_arch(self.arch)) {
            ("macos", "arm64") => ("macos", "arm64", "tar.gz"),
            ("macos", "x64") => ("macos", "x64", "tar.gz"),
            ("linux", "x64") => ("ubuntu", "x64", "tar.gz"),
            ("linux", "arm64") => ("ubuntu", "arm64", "tar.gz"),
            ("windows", "x64") => ("win-cpu", "x64", "zip"),
            ("windows", "arm64") => ("win-cpu", "arm64", "zip"),
            other => unreachable!("EngineTarget constructed with unsupported pair {other:?}"),
        };
        format!("llama-{tag}-bin-{plat}-{arch_name}.{ext}")
    }

    fn asset_url(self, tag: &str) -> String {
        format!("{LLAMA_RELEASE_BASE}/{tag}/{}", self.asset_name(tag))
    }

    /// The single executable the runtime cares about, named correctly for
    /// the target OS.
    pub fn server_binary(self) -> &'static str {
        if self.os == "windows" {
            "llama-server.exe"
        } else {
            "llama-server"
        }
    }
}

fn normalise_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Status of an engine install for one target on a given drive.
#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    pub target: EngineTarget,
    pub installed: bool,
    pub server_path: PathBuf,
}

/// Report engine install status for every supported target on the drive.
pub fn report_status(layout: &DriveLayout, version: &str) -> Vec<EngineStatus> {
    EngineTarget::ALL
        .iter()
        .map(|&target| {
            let bin_dir = layout
                .version_dir(version)
                .join("bin")
                .join(target.dir_name());
            let server_path = bin_dir.join(target.server_binary());
            EngineStatus {
                target,
                installed: server_path.exists(),
                server_path,
            }
        })
        .collect()
}

/// Selection used by `install_engines`.
#[derive(Debug, Clone)]
pub enum EngineSelection {
    /// All six supported targets.
    AllPlatforms,
    /// The host the installer is running on (or error if unknown).
    CurrentHost,
    /// A specific named target like `linux-x64`.
    Named(EngineTarget),
}

impl EngineSelection {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "all" | "all-platforms" => Ok(Self::AllPlatforms),
            "host" | "current" => Ok(Self::CurrentHost),
            other => EngineTarget::parse(other).map(Self::Named).ok_or_else(|| {
                UsbBuddyError::InvalidState(format!(
                    "unknown engine target '{other}' — expected 'all', 'host', or one of {}",
                    EngineTarget::ALL
                        .iter()
                        .map(|t| t.dir_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }),
        }
    }

    pub fn resolve(&self) -> Result<Vec<EngineTarget>> {
        match self {
            Self::AllPlatforms => Ok(EngineTarget::ALL.to_vec()),
            Self::Named(t) => Ok(vec![*t]),
            Self::CurrentHost => EngineTarget::current_host()
                .map(|t| vec![t])
                .ok_or_else(|| {
                    UsbBuddyError::InvalidState(format!(
                        "current host {}/{} is not a supported engine target",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ))
                }),
        }
    }
}

/// Outcome of a single target's install.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledEngine {
    pub target: EngineTarget,
    pub server_path: PathBuf,
    pub bytes_installed: u64,
    pub files_installed: usize,
}

/// Progress for one asset (engine archive or runtime binary) inside a
/// multi-asset install. `idx` is 1-based to match how it reads in the UI
/// ("1 of 6").
#[derive(Debug, Clone)]
pub struct AssetProgress {
    pub name: String,
    pub idx: usize,
    pub total: usize,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

/// Install one or more engine targets onto the drive. Each archive is
/// downloaded to a temp file, verified to contain `llama-server`, then
/// every file is extracted flat into `versions/<v>/bin/<os>-<arch>/`.
///
/// `progress` is called with human-readable status strings so callers
/// (CLI / TUI / GUI) can stream them to the user.
pub fn install_engines(
    layout: &DriveLayout,
    version: &str,
    selection: &EngineSelection,
    tag: &str,
    progress: impl FnMut(String),
) -> Result<Vec<InstalledEngine>> {
    install_engines_with_asset_progress(layout, version, selection, tag, progress, |_| {})
}

/// Same as [`install_engines`] but additionally fires `asset_progress`
/// every ~256 KiB of every asset download with a precise byte-level
/// position. UIs use it to render a determinate per-asset progress bar.
pub fn install_engines_with_asset_progress(
    layout: &DriveLayout,
    version: &str,
    selection: &EngineSelection,
    tag: &str,
    mut progress: impl FnMut(String),
    mut asset_progress: impl FnMut(AssetProgress),
) -> Result<Vec<InstalledEngine>> {
    let targets = selection.resolve()?;
    let total_count = targets.len();
    let mut installed = Vec::with_capacity(total_count);

    let cache_dir = layout.usbuddy_dir().join("engine-cache").join(tag);
    fs::create_dir_all(&cache_dir)?;

    for (idx, target) in targets.iter().enumerate() {
        let bin_dir = layout
            .version_dir(version)
            .join("bin")
            .join(target.dir_name());
        fs::create_dir_all(&bin_dir)?;

        let asset_name = target.asset_name(tag);
        let url = target.asset_url(tag);
        let archive_path = cache_dir.join(&asset_name);

        if !archive_path.exists() {
            progress(format!("→ downloading {asset_name}"));
            let asset_name_for_cb = asset_name.clone();
            let one_based = idx + 1;
            download_verified_with_progress(
                &url,
                &archive_path,
                None,
                |DownloadProgress {
                     bytes_done,
                     bytes_total,
                 }| {
                    asset_progress(AssetProgress {
                        name: asset_name_for_cb.clone(),
                        idx: one_based,
                        total: total_count,
                        bytes_done,
                        bytes_total,
                    });
                },
            )
            .map_err(|e| {
                UsbBuddyError::InvalidState(format!(
                    "engine download failed for {}: {e}",
                    target.dir_name()
                ))
            })?;
        } else {
            progress(format!("• cached {asset_name}"));
            // Synthesize a "100%" tick for the bar so the UI shows the
            // cached asset as done rather than stuck at 0.
            let bytes = fs::metadata(&archive_path).map(|m| m.len()).unwrap_or(0);
            asset_progress(AssetProgress {
                name: asset_name.clone(),
                idx: idx + 1,
                total: total_count,
                bytes_done: bytes,
                bytes_total: Some(bytes),
            });
        }

        progress(format!("→ extracting into {}", bin_dir.display()));
        let report = if asset_name.ends_with(".zip") {
            extract_zip(&archive_path, &bin_dir)?
        } else {
            extract_tar_gz(&archive_path, &bin_dir)?
        };

        let server_path = bin_dir.join(target.server_binary());
        if !server_path.exists() {
            return Err(UsbBuddyError::InvalidState(format!(
                "extraction succeeded but {} is missing in {}",
                target.server_binary(),
                bin_dir.display()
            )));
        }

        #[cfg(unix)]
        ensure_unix_executable(&bin_dir)?;

        progress(format!(
            "✓ {} ready ({} files, {:.1} MiB)",
            target.dir_name(),
            report.files,
            report.bytes as f64 / 1_048_576.0
        ));

        installed.push(InstalledEngine {
            target: *target,
            server_path,
            bytes_installed: report.bytes,
            files_installed: report.files,
        });
    }

    Ok(installed)
}

struct ExtractReport {
    files: usize,
    bytes: u64,
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<ExtractReport> {
    // Two-pass extraction so we can resolve in-archive symlinks even on
    // filesystems (exFAT, FAT32) that do not support them. Pass 1 writes
    // regular files; pass 2 copies the underlying bytes under each
    // symlink's name.
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut links: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();

    let file = fs::File::open(archive)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    for entry in tar
        .entries()
        .map_err(|e| UsbBuddyError::Io(io::Error::other(e)))?
    {
        let mut entry = entry.map_err(|e| UsbBuddyError::Io(io::Error::other(e)))?;
        let entry_path = entry
            .path()
            .map_err(|e| UsbBuddyError::Io(io::Error::other(e)))?
            .into_owned();
        let Some(file_name) = entry_path.file_name() else {
            continue;
        };
        let etype = entry.header().entry_type();
        if etype.is_dir() {
            continue;
        }
        if etype.is_symlink() || etype.is_hard_link() {
            if let Ok(Some(link_target)) = entry.link_name()
                && let Some(target_name) = link_target.file_name()
            {
                links.push((file_name.to_owned(), target_name.to_owned()));
            }
            continue;
        }
        let out_path = dest.join(file_name);
        let mut out = fs::File::create(&out_path)?;
        let n = io::copy(&mut entry, &mut out).map_err(UsbBuddyError::Io)?;
        files += 1;
        bytes += n;
    }

    // Resolve link aliases: chase them through other links until we hit
    // a real file. This handles the typical libfoo.dylib -> libfoo.0.dylib
    // -> libfoo.0.14.0.dylib chain that llama.cpp ships.
    for (link, _) in links.iter() {
        if let Some(resolved) = resolve_link(&links, link)
            && let Some(real) = resolved
        {
            let src = dest.join(&real);
            let dst = dest.join(link);
            if src.exists() && src != dst {
                let n = fs::copy(&src, &dst).unwrap_or(0);
                if n > 0 {
                    files += 1;
                    bytes += n;
                }
            }
        }
    }

    Ok(ExtractReport { files, bytes })
}

fn resolve_link(
    links: &[(std::ffi::OsString, std::ffi::OsString)],
    name: &std::ffi::OsStr,
) -> Option<Option<std::ffi::OsString>> {
    let mut current = name.to_owned();
    for _ in 0..16 {
        let next = links
            .iter()
            .find(|(l, _)| l.as_os_str() == current.as_os_str())
            .map(|(_, t)| t.clone());
        match next {
            Some(t) => current = t,
            None => return Some(Some(current)),
        }
    }
    Some(None)
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<ExtractReport> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| UsbBuddyError::Io(io::Error::other(e)))?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| UsbBuddyError::Io(io::Error::other(e)))?;
        if entry.is_dir() {
            continue;
        }
        let in_name = entry.name().to_string();
        let file_name = Path::new(&in_name)
            .file_name()
            .map(|s| s.to_owned())
            .ok_or_else(|| {
                UsbBuddyError::InvalidState(format!("zip entry has no file name: {in_name}"))
            })?;
        let out_path = dest.join(&file_name);
        let mut out = fs::File::create(&out_path)?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = entry.read(&mut buf).map_err(UsbBuddyError::Io)?;
            if n == 0 {
                break;
            }
            io::Write::write_all(&mut out, &buf[..n]).map_err(UsbBuddyError::Io)?;
            bytes += n as u64;
        }
        files += 1;
    }
    Ok(ExtractReport { files, bytes })
}

#[cfg(unix)]
fn ensure_unix_executable(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // Mark binaries and shared libs as executable. exFAT-fuse mounts on
        // some Linux distros come up with mode 0700; we only need to make
        // sure the bits are *requested* — the OS may still ignore them at
        // mount time, but on filesystems that honour them this is enough.
        let is_binary = !name.contains('.')
            || name.ends_with(".so")
            || name.contains(".so.")
            || name.ends_with(".dylib");
        if is_binary {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&path, perms);
        }
    }
    Ok(())
}

/// Default GitHub Releases base URL for the USBuddy runtime per-platform
/// assets. The "latest/download" form auto-redirects to whatever the most
/// recent published (non-prerelease, non-draft) release is, so callers
/// don't need to know the current tag.
pub const DEFAULT_RUNTIME_RELEASE_BASE: &str =
    "https://github.com/skullzarmy/USBuddy/releases/latest/download";

impl EngineTarget {
    /// Bare per-platform runtime asset name as published by USBuddy's
    /// `release.yml` workflow. Must stay in lock-step with the workflow.
    pub fn runtime_asset_name(self) -> String {
        let suffix = if self.os == "windows" { ".exe" } else { "" };
        format!("usbuddy-runtime-{}{}", self.dir_name(), suffix)
    }

    /// Full URL for the runtime asset under a given base (defaults to
    /// `DEFAULT_RUNTIME_RELEASE_BASE` — the "latest release" alias).
    pub fn runtime_asset_url(self, base: &str) -> String {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            self.runtime_asset_name()
        )
    }

    /// Filename used on the drive for this target's runtime binary.
    pub fn runtime_binary(self) -> &'static str {
        if self.os == "windows" {
            "usbuddy-runtime.exe"
        } else {
            "usbuddy-runtime"
        }
    }
}

/// Outcome of installing one per-platform runtime binary.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledRuntime {
    pub target: EngineTarget,
    pub runtime_path: PathBuf,
    pub bytes_installed: u64,
}

/// Download the USBuddy runtime binary for one or more targets from a
/// GitHub release and place it at `versions/<v>/bin/<os>-<arch>/`.
///
/// `base_url` should be the directory that contains the per-platform
/// assets, e.g. `https://github.com/skullzarmy/USBuddy/releases/latest/download`
/// or `https://github.com/skullzarmy/USBuddy/releases/download/v0.2.0`.
pub fn install_runtimes_from_release(
    layout: &DriveLayout,
    version: &str,
    selection: &EngineSelection,
    base_url: &str,
    progress: impl FnMut(String),
) -> Result<Vec<InstalledRuntime>> {
    install_runtimes_from_release_with_asset_progress(
        layout,
        version,
        selection,
        base_url,
        progress,
        |_| {},
    )
}

/// Same as [`install_runtimes_from_release`] but additionally fires
/// `asset_progress` with byte-level position so UIs can render a
/// determinate per-asset bar.
pub fn install_runtimes_from_release_with_asset_progress(
    layout: &DriveLayout,
    version: &str,
    selection: &EngineSelection,
    base_url: &str,
    mut progress: impl FnMut(String),
    mut asset_progress: impl FnMut(AssetProgress),
) -> Result<Vec<InstalledRuntime>> {
    let targets = selection.resolve()?;
    let total_count = targets.len();
    let mut installed = Vec::with_capacity(total_count);

    for (idx, target) in targets.iter().enumerate() {
        let bin_dir = layout
            .version_dir(version)
            .join("bin")
            .join(target.dir_name());
        fs::create_dir_all(&bin_dir)?;

        let asset = target.runtime_asset_name();
        let url = target.runtime_asset_url(base_url);
        let dest = bin_dir.join(target.runtime_binary());

        progress(format!("→ downloading {asset}"));
        let asset_for_cb = asset.clone();
        let one_based = idx + 1;
        download_verified_with_progress(
            &url,
            &dest,
            None,
            |DownloadProgress {
                 bytes_done,
                 bytes_total,
             }| {
                asset_progress(AssetProgress {
                    name: asset_for_cb.clone(),
                    idx: one_based,
                    total: total_count,
                    bytes_done,
                    bytes_total,
                });
            },
        )
        .map_err(|e| {
            UsbBuddyError::InvalidState(format!(
                "runtime download failed for {} ({}): {e} — \
                 if there is no published USBuddy release for this platform yet, \
                 use `install-runtime` without --from-release on a {} host instead",
                target.dir_name(),
                url,
                target.dir_name(),
            ))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dest)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dest, perms)?;
        }

        let bytes = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        progress(format!(
            "✓ {} runtime ready ({:.2} MiB)",
            target.dir_name(),
            bytes as f64 / 1_048_576.0
        ));
        installed.push(InstalledRuntime {
            target: *target,
            runtime_path: dest,
            bytes_installed: bytes,
        });
    }

    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_dir_names_use_normalised_arch() {
        assert_eq!(EngineTarget::MACOS_X64.dir_name(), "macos-x64");
        assert_eq!(EngineTarget::LINUX_ARM64.dir_name(), "linux-arm64");
        assert_eq!(EngineTarget::WINDOWS_X64.dir_name(), "windows-x64");
    }

    #[test]
    fn selection_parses() {
        assert!(matches!(
            EngineSelection::parse("all").unwrap(),
            EngineSelection::AllPlatforms
        ));
        assert!(matches!(
            EngineSelection::parse("host").unwrap(),
            EngineSelection::CurrentHost
        ));
        match EngineSelection::parse("linux-x64").unwrap() {
            EngineSelection::Named(t) => assert_eq!(t, EngineTarget::LINUX_X64),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(EngineSelection::parse("bsd-x64").is_err());
    }

    #[test]
    fn asset_name_matches_known_pattern() {
        assert_eq!(
            EngineTarget::MACOS_ARM64.asset_name("b9570"),
            "llama-b9570-bin-macos-arm64.tar.gz"
        );
        assert_eq!(
            EngineTarget::WINDOWS_X64.asset_name("b9570"),
            "llama-b9570-bin-win-cpu-x64.zip"
        );
    }

    #[test]
    fn runtime_asset_names_match_release_convention() {
        assert_eq!(
            EngineTarget::MACOS_ARM64.runtime_asset_name(),
            "usbuddy-runtime-macos-arm64"
        );
        assert_eq!(
            EngineTarget::LINUX_X64.runtime_asset_name(),
            "usbuddy-runtime-linux-x64"
        );
        assert_eq!(
            EngineTarget::WINDOWS_ARM64.runtime_asset_name(),
            "usbuddy-runtime-windows-arm64.exe"
        );
        assert_eq!(
            EngineTarget::LINUX_X64.runtime_asset_url(DEFAULT_RUNTIME_RELEASE_BASE),
            "https://github.com/skullzarmy/USBuddy/releases/latest/download/usbuddy-runtime-linux-x64"
        );
    }
}
