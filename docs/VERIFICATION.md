# Verifying a USBuddy release

This is the document for paranoid users. If you've downloaded a USBuddy
archive from GitHub Releases and want to confirm the binaries you're
about to run match the code in this repo, here's how.

There's no Apple Developer ID and no Authenticode certificate in scope
for USBuddy. The substitutes are:

- **`SHA256SUMS.txt`** for byte-for-byte integrity.
- **A SLSA build provenance attestation** signed by GitHub Actions,
  which proves the binary was built from a specific commit in this
  repository by the published `release.yml` workflow.
- **A CycloneDX SBOM** listing every Rust crate compiled in.

All three are attached to every release.

## What's in a release

Each tagged release contains:

```
usbuddy-installer-macos-universal.tar.gz   ← macOS Apple Silicon + Intel
usbuddy-installer-linux-x64.tar.gz         ← Linux x86_64
usbuddy-installer-linux-arm64.tar.gz       ← Linux ARM64 (lower priority)
usbuddy-installer-windows-x64.exe          ← Windows x86_64
SHA256SUMS.txt                             ← SHA256 of every asset above
SBOM.cdx.tgz                               ← Per-crate CycloneDX SBOMs (JSON)
```

`llama.cpp` binaries and model weights are not in the bundle. The
installer fetches them at install time and verifies them separately
against the catalog's published SHA256s.

## Verifying integrity

Download both the asset you want and `SHA256SUMS.txt`. Then:

```sh
# macOS / Linux
shasum -a 256 -c SHA256SUMS.txt --ignore-missing

# Windows (PowerShell)
Get-FileHash usbuddy-installer-windows-x64.exe -Algorithm SHA256
# Compare manually against the line in SHA256SUMS.txt
```

If the hash doesn't match, do not run the binary. File an issue with
the version, the hash you got, and where you downloaded it.

## Verifying build provenance

GitHub generates a SLSA build provenance attestation for every release
asset, signed by the Actions OIDC issuer. The attestation proves the
asset was produced by a specific workflow run on a specific commit in
this repo — not by a maintainer's laptop, not by a forked workflow.

Install the GitHub CLI (`brew install gh`, `winget install github.cli`,
or equivalent), authenticate (`gh auth login`), then:

```sh
gh attestation verify \
  usbuddy-installer-macos-universal.tar.gz \
  --owner skullzarmy
```

The expected output is a line like:

```
✓ Verification succeeded!
sha256:<asset-hash> was attested by:
REPO                  PREDICATE_TYPE                  WORKFLOW
skullzarmy/USBuddy    https://slsa.dev/provenance/v1  .github/workflows/release.yml@refs/tags/v0.1.0
```

If you see anything else — different repo, different workflow path,
no attestation found — do not run the binary.

## Inspecting the SBOM

The `SBOM.cdx.tgz` archive contains one CycloneDX JSON file per
workspace crate (`usbuddy-core.cdx.json`,
`usbuddy-installer-cli.cdx.json`, etc.). Each lists every Rust crate
that was compiled into that binary, with versions and license
identifiers.

```sh
tar xzf SBOM.cdx.tgz -C sbom
jq '.components[] | {name, version, licenses}' sbom/usbuddy-runtime.cdx.json
```

Use this when you need to audit transitive dependencies for a known
CVE, license compatibility, or supply-chain provenance.

## Catalog integrity

The catalog has its own integrity story, documented in
[`CATALOG-SPEC.md`](CATALOG-SPEC.md). Every catalog entry carries a
`sha256` that the installer verifies on download and the runtime
re-verifies on every launch. USB media corrupts; the launch-time check
is not paranoia.

If you've forked the catalog or pointed your installer at a custom
catalog URL, that's a separate trust decision. The UI labels custom
catalogs distinctly so an entry from `your-org/internal-catalog` is
never confused with an entry from `skullzarmy/USBuddy`.

## Dismissing Gatekeeper and SmartScreen

There is no Developer ID Application certificate or Authenticode
certificate signing the binaries. The OS will warn you on first launch.
Those warnings are not bugs — they're the OS doing exactly what it
should, given that the binary is unsigned.

### macOS

```sh
# Remove the quarantine flag the OS added when you downloaded the file
xattr -dr com.apple.quarantine /path/to/usbuddy-installer-macos-universal
```

Alternatively, double-click, get the Gatekeeper dialog, go to
**System Settings → Privacy & Security**, scroll down, click
**Open Anyway**. You'll only have to do it once per binary.

The installer additionally strips `com.apple.quarantine` from the
runtime binary it copies onto the USB drive, so the drive-side
launcher doesn't trip Gatekeeper on every new host.

### Windows

When SmartScreen blocks the binary, click **More info → Run anyway**.
Once per binary per machine.

If you'd rather verify and approve from PowerShell:

```powershell
# Unblock the downloaded file
Unblock-File usbuddy-installer-windows-x64.exe
```

### Linux

Nothing to dismiss. Mark executable and run:

```sh
tar xzf usbuddy-installer-linux-x64.tar.gz
chmod +x usbuddy-installer-cli usbuddy-installer-tui usbuddy-installer-gui
./usbuddy-installer-gui
```

## Reporting verification failures

If `shasum -c` fails or `gh attestation verify` reports something
unexpected — wrong repo, wrong workflow, missing attestation — please
file an issue with:

- The release tag.
- The asset filename.
- The exact output you got.
- Where you downloaded it from (browser, curl, `gh release download`,
  third-party mirror).

A mismatch is either a tampered download, a stale mirror, or a CI bug.
Either way we need to know.
