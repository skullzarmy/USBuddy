# Verification

USBuddy release artifacts are unsigned. Integrity and provenance are
established by SHA256 hashes, a SLSA build provenance attestation
signed by the GitHub Actions OIDC issuer, and a CycloneDX SBOM. This
document specifies the verification procedures.

## Release contents

Each tagged release includes the following assets:

| Asset                                       | Contents                                          |
| ------------------------------------------- | ------------------------------------------------- |
| `usbuddy-installer-macos-universal.tar.gz`  | macOS universal2 binaries (Apple Silicon + Intel) |
| `usbuddy-installer-linux-x64.tar.gz`        | Linux x86_64 binaries                             |
| `usbuddy-installer-linux-arm64.tar.gz`      | Linux ARM64 binaries                              |
| `usbuddy-installer-windows-x64.exe`         | Windows x86_64 installer                          |
| `SHA256SUMS.txt`                            | SHA256 of every other asset in the release        |
| `SBOM.cdx.tgz`                              | Per-crate CycloneDX SBOMs in JSON format          |

`llama.cpp` binaries and model weights are not included in release
bundles. The installer fetches them at install time and verifies them
independently against catalog `sha256` values.

## Integrity

```sh
# macOS and Linux
shasum -a 256 -c SHA256SUMS.txt --ignore-missing
```

```powershell
# Windows
Get-FileHash usbuddy-installer-windows-x64.exe -Algorithm SHA256
```

A mismatch indicates tampering, a stale mirror, or a CI bug. Mismatched
artifacts must not be executed.

## Build provenance

GitHub Actions generates a SLSA build provenance attestation for each
release asset, signed by the Actions OIDC issuer. The attestation
binds an asset's SHA256 to a specific commit and workflow path in this
repository.

```sh
gh attestation verify \
  usbuddy-installer-macos-universal.tar.gz \
  --owner skullzarmy
```

A successful verification reports the asset hash, the source
repository, the predicate type (`https://slsa.dev/provenance/v1`), and
the workflow path that produced the asset. The expected workflow path
is `.github/workflows/release.yml`. Any other workflow path indicates
the binary was produced outside the published release pipeline.

## Software bill of materials

`SBOM.cdx.tgz` contains one CycloneDX JSON document per workspace
crate (`usbuddy-core.cdx.json`, `usbuddy-installer-cli.cdx.json`,
`usbuddy-runtime.cdx.json`, and so on). Each document enumerates the
Rust crates compiled into the corresponding binary with their versions
and SPDX license identifiers.

```sh
tar xzf SBOM.cdx.tgz -C sbom
jq '.components[] | {name, version, licenses}' sbom/usbuddy-runtime.cdx.json
```

The SBOM is the source of truth for transitive dependency auditing,
CVE matching, and license compatibility analysis.

## Catalog integrity

Model integrity is established independently of release verification.
Every catalog entry carries a SHA256 (see [`CATALOG-SPEC.md`](CATALOG-SPEC.md))
that the installer verifies after download and the runtime re-verifies
on every launch.

Custom catalogs are a separate trust decision. The UI labels entries
from custom catalogs distinctly from official-catalog entries.

## Code signing

There is no Apple Developer ID certificate or Authenticode certificate
in scope for v0.1.0. As a consequence, first execution of an unsigned
binary triggers a Gatekeeper dialog on macOS or a SmartScreen prompt on
Windows. These are not failures.

### macOS

Strip the quarantine attribute from a downloaded archive:

```sh
xattr -dr com.apple.quarantine /path/to/usbuddy-installer-macos-universal
```

Or approve interactively via **System Settings → Privacy & Security →
Open Anyway**. The approval is per binary and persists.

The installer strips `com.apple.quarantine` from the runtime binary it
copies onto the drive, so the drive-side launcher does not retrigger
Gatekeeper on each new host.

### Windows

```powershell
Unblock-File usbuddy-installer-windows-x64.exe
```

Or click **More info → Run anyway** at the SmartScreen prompt.
Approval is per binary and persists.

### Linux

No dismissal required. After extraction, mark the binary executable:

```sh
tar xzf usbuddy-installer-linux-x64.tar.gz
chmod +x usbuddy-installer-cli usbuddy-installer-tui usbuddy-installer-gui
```

## Reporting verification failures

A failure of `shasum -c`, `gh attestation verify`, or SBOM
verification should be reported as an issue containing:

- The release tag.
- The asset filename.
- The full command output.
- The download source (browser, `curl`, `gh release download`, third
  party).

Such failures indicate either a tampered download, a stale mirror, or
a defect in the release pipeline.
