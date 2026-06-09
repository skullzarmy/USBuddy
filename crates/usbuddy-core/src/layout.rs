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

/// POSIX launcher (used for both `USBuddy.command` on macOS and `USBuddy.sh`
/// on Linux). Detects OS+arch, locates `versions/<active>/bin/<os>-<arch>/usbuddy-runtime`,
/// and exec's it with `--drive <self> --open-browser`. Fails loudly if the
/// per-platform binary is missing so the user knows exactly what's wrong.
const POSIX_LAUNCHER: &str = r#"#!/usr/bin/env bash
# USBuddy portable launcher — runs the runtime that lives on this drive.
# No installation required on the host; double-click or `./USBuddy.command`.
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
if [ ! -f "$DIR/current.json" ]; then
  echo "USBuddy: $DIR/current.json not found. Is this a USBuddy drive?" >&2
  exit 1
fi
ACTIVE="$(sed -n 's/.*"active"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$DIR/current.json" | head -1)"
if [ -z "$ACTIVE" ]; then
  echo "USBuddy: could not read active version from current.json" >&2
  exit 1
fi
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Darwin) OSDIR=macos ;;
  Linux)  OSDIR=linux ;;
  *) echo "USBuddy: unsupported OS '$OS'" >&2; exit 1 ;;
esac
case "$ARCH" in
  x86_64|amd64) ARCHDIR=x64 ;;
  arm64|aarch64) ARCHDIR=arm64 ;;
  *) echo "USBuddy: unsupported arch '$ARCH'" >&2; exit 1 ;;
esac
BIN="$DIR/versions/$ACTIVE/bin/${OSDIR}-${ARCHDIR}/usbuddy-runtime"
if [ ! -x "$BIN" ]; then
  echo "USBuddy: runtime not installed for ${OSDIR}-${ARCHDIR}."
  echo "  Expected: $BIN"
  echo
  echo "  Fix with one of:"
  echo "    • Run the USBuddy installer on this host (it will copy the runtime)."
  echo "    • Fetch from a published release:"
  echo "        usbuddy-installer-cli install-runtime \"$DIR\" \\"
  echo "          --from-release --target ${OSDIR}-${ARCHDIR}"
  # Keep the Terminal window open on macOS so the user can read the message.
  if [ "$OSDIR" = "macos" ] && [ -t 0 ]; then
    read -n 1 -s -r -p "Press any key to close…"
    echo
  fi
  exit 1
fi
exec "$BIN" serve --drive "$DIR" --open-browser
"#;

/// Windows launcher. Reads the active version out of `current.json` with
/// `findstr`, picks the matching `windows-x64` or `windows-arm64` runtime,
/// and exec's it. Keeps the console open on error so the user sees why.
const WINDOWS_LAUNCHER: &str = r#"@echo off
setlocal EnableDelayedExpansion
set "DIR=%~dp0"
if "%DIR:~-1%"=="\" set "DIR=%DIR:~0,-1%"
if not exist "%DIR%\current.json" (
  echo USBuddy: %DIR%\current.json not found. Is this a USBuddy drive?
  pause
  exit /b 1
)
set "ACTIVE="
for /f "tokens=2 delims=:," %%a in ('findstr /c:"\"active\"" "%DIR%\current.json"') do (
  set "RAW=%%a"
)
set "RAW=%RAW: =%"
set "RAW=%RAW:"=%"
set "ACTIVE=%RAW%"
if "%ACTIVE%"=="" (
  echo USBuddy: could not read active version from current.json
  pause
  exit /b 1
)
set "ARCHDIR=x64"
if /I "%PROCESSOR_ARCHITECTURE%"=="ARM64" set "ARCHDIR=arm64"
if /I "%PROCESSOR_ARCHITEW6432%"=="ARM64" set "ARCHDIR=arm64"
set "BIN=%DIR%\versions\%ACTIVE%\bin\windows-%ARCHDIR%\usbuddy-runtime.exe"
if not exist "%BIN%" (
  echo USBuddy: runtime not installed for windows-%ARCHDIR%.
  echo   Expected: %BIN%
  echo.
  echo   Fix with one of:
  echo     - Run the USBuddy installer on this host ^(it will copy the runtime^).
  echo     - Fetch from a published release:
  echo         usbuddy-installer-cli install-runtime "%DIR%" ^^
  echo           --from-release --target windows-%ARCHDIR%
  pause
  exit /b 1
)
"%BIN%" serve --drive "%DIR%" --open-browser
endlocal
"#;

const DRIVE_README: &str = "USBuddy portable runtime root.\n\
\n\
To start USBuddy, double-click the launcher for your platform at this drive's root:\n\
  • macOS:   USBuddy.command\n\
  • Linux:   USBuddy.sh\n\
  • Windows: USBuddy.bat\n\
\n\
The launcher runs the USBuddy runtime that lives on this drive and opens the chat UI\n\
in your default browser. Keep the drive inserted while USBuddy is running.\n\
\n\
If the launcher reports the runtime is not installed for your platform, run the\n\
USBuddy installer on this host once (it will copy the runtime onto the drive), or\n\
fetch all platforms ahead of time with `usbuddy-installer-cli install-runtime\n\
<drive> --from-release --target all`.\n";

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
        // Always (re)write the README so guidance stays in sync with the launchers.
        fs::write(self.root.join("README.txt"), DRIVE_README)?;
        self.write_current(&CurrentVersionPointer {
            schema: CURRENT_SCHEMA_V1,
            active: active_version.into(),
            previous: None,
        })?;
        // Drop in the cross-platform launchers so the stick is double-clickable
        // on any host the moment it's initialised — even before a runtime is
        // installed for that platform (the launcher tells the user how to fix it).
        self.write_launchers()?;
        Ok(())
    }

    /// Write `USBuddy.command` (mac), `USBuddy.sh` (linux), and `USBuddy.bat`
    /// (windows) to the drive root. Idempotent — overwrites in place so the
    /// scripts stay current with newer USBuddy versions. The POSIX scripts get
    /// mode `0755` on Unix hosts (no-op on Windows / FAT-style filesystems
    /// that ignore exec bits — the launcher will still run via `bash` and the
    /// Finder treats `.command` as executable regardless).
    pub fn write_launchers(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let mac = self.root.join("USBuddy.command");
        let sh = self.root.join("USBuddy.sh");
        let bat = self.root.join("USBuddy.bat");
        fs::write(&mac, POSIX_LAUNCHER)?;
        fs::write(&sh, POSIX_LAUNCHER)?;
        fs::write(&bat, WINDOWS_LAUNCHER)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&mac, &sh] {
                if let Ok(meta) = fs::metadata(path) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o755);
                    let _ = fs::set_permissions(path, perms);
                }
            }
        }
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

    /// Activate `version`, keeping the currently-active version as previous.
    pub fn rollback_to(&self, version: &str) -> Result<CurrentVersionPointer> {
        let current = self.read_current().ok();
        let previous = current.map(|c| c.active);
        let next = CurrentVersionPointer {
            schema: CURRENT_SCHEMA_V1,
            active: version.into(),
            previous,
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
            let file_name = entry.file_name().to_string_lossy();
            // Skip macOS AppleDouble metadata sidecars and other dot-files
            // that filesystems like exFAT accumulate when a Mac writes to them.
            if file_name.starts_with("._") || file_name == ".DS_Store" {
                continue;
            }
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

    #[test]
    fn initialize_drops_in_cross_platform_launchers() {
        let dir = tempdir().unwrap();
        let layout = DriveLayout::new(dir.path());
        layout.initialize_structure("0.1.0").unwrap();

        let mac = dir.path().join("USBuddy.command");
        let sh = dir.path().join("USBuddy.sh");
        let bat = dir.path().join("USBuddy.bat");
        assert!(mac.exists(), "USBuddy.command must be written");
        assert!(sh.exists(), "USBuddy.sh must be written");
        assert!(bat.exists(), "USBuddy.bat must be written");

        // Sanity-check launcher contents reference the runtime + serve flags
        // so a future refactor that breaks the contract trips this test.
        let mac_body = std::fs::read_to_string(&mac).unwrap();
        assert!(mac_body.contains("usbuddy-runtime"));
        assert!(mac_body.contains("--drive"));
        assert!(mac_body.contains("--open-browser"));
        let bat_body = std::fs::read_to_string(&bat).unwrap();
        assert!(bat_body.contains("usbuddy-runtime.exe"));
        assert!(bat_body.contains("--drive"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&mac).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(
                std::fs::metadata(&sh).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }

        // README should reference the new launcher entry points, not the old
        // "Launch from the platform launcher" placeholder.
        let readme = std::fs::read_to_string(dir.path().join("README.txt")).unwrap();
        assert!(readme.contains("USBuddy.command"));
        assert!(readme.contains("USBuddy.bat"));
        assert!(readme.contains("USBuddy.sh"));
    }
}
