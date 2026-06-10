# Architecture

USBuddy is structured as two executables sharing a Rust core. This document
describes that structure, the on-drive layout, the lifecycle of each
operation, and the invariants the implementation maintains.

## Two executables

The installer runs on a host, online, with whatever privileges are
required to format removable media. It is responsible for laying down
the drive's directory structure, fetching the inference engine and
model weights, copying the runtime onto the drive, and managing
upgrades and rollback. Three installer surfaces are provided
(`usbuddy-installer-cli`, `usbuddy-installer-tui`,
`usbuddy-installer-gui`), each a thin view over the shared core.

The runtime runs from the USB drive on any compatible host. It binds
to `127.0.0.1`, serves a static SPA from RAM, manages the lifecycle of
a child `llama-server` process, and exits cleanly on quit. It performs
no writes to the host filesystem and no writes to the USB drive during
an active chat session.

The two executables have different constraints. The installer can
afford disk space and dependencies on the host; the runtime cannot.
Separating them keeps the runtime small and prevents installer
concerns from leaking into the path that ships on the stick.

Both depend on `crates/usbuddy-core`, which owns:

- Catalog parsing and integrity validation.
- Release manifest parsing and semver comparison.
- SHA256 hashing and verification.
- Atomic file writes for pointer state.
- Drive layout path resolution.
- License preference persistence and acceptance records.
- Platform and architecture detection.
- RAM-fit estimation.
- GGUF header parsing for model architecture metadata.

The surfaces (`-cli`, `-tui`, `-gui`, runtime) call into the core and
do not duplicate logic.

## Drive layout

The drive is organized as a shadow tree. Each runtime version installs
into its own subdirectory under `versions/`, and a single
`current.json` pointer at the drive root selects the active version.
Updates stage a new version tree alongside the current one and atomically
rewrite the pointer; rollback is a single file swap.

```
/USBuddy/
├── current.json                Schema-versioned pointer: {active, previous}
├── USBuddy.command             Per-OS launcher shims. Read current.json,
├── USBuddy.sh                  detect OS+arch, exec the matching runtime.
├── USBuddy.bat
├── versions/
│   ├── 0.1.0/
│   │   ├── bin/<os>-<arch>/    Runtime binary and llama-server per platform.
│   │   └── ui/                 Static SPA pinned to this runtime version.
│   └── 0.2.0/                  Parallel tree for staged upgrade.
├── models/                     Shared across versions. SHA256-keyed filenames.
├── catalog.json                Catalog snapshot. Refreshable independently
│                               of runtime updates.
├── .usbuddy/                   User state. Survives runtime upgrades.
│   ├── license-prefs.toml      Human-readable license opt-out preferences.
│   ├── runtime-prefs.toml      Incognito vs. saving toggle.
│   ├── chats/                  Saved conversations when memory is enabled.
│   ├── advisories-seen.json    Dismissed advisories.
│   └── hf-token                Present only if the user added a token.
└── README.txt
```

Versioned content (the runtime, `llama-server`, the SPA bundle) lives
under `versions/{ver}/` and is tested as a unit. Shared content (model
weights, catalog snapshot, user state) lives at the drive root and
persists across runtime upgrades.

## Lifecycle

### Install

The installer is given a drive root. It writes the directory
structure, initializes `current.json`, copies a catalog snapshot,
writes the per-OS launcher scripts, and stages the runtime under
`versions/{version}/bin/<os>-<arch>/`. `llama-server` is downloaded
from pinned `llama.cpp` releases per supported host;
`engine install <drive> --target all` populates every supported
platform from a single host.

### Launch

A launcher script reads `current.json`, locates
`versions/<active>/bin/<os>-<arch>/usbuddy-runtime`, and execs it
with `--drive <self> --open-browser`. The runtime starts an HTTP
server on `127.0.0.1:8765`, registers a tray icon, opens the user's
default browser, and waits for input. `llama-server` is not spawned
until the user selects a model and clicks Launch.

### Engine load

When the user clicks Launch, the runtime:

1. Resolves the model file under `models/` and stats it.
2. Parses the GGUF header to extract architecture (layers, attention
   heads, KV heads, embedding dimension, trained context length).
3. Evaluates the RAM-fit advisor against detected available memory
   using the model's actual KV-cache shape. Red band aborts the
   launch with a diagnostic.
4. Spawns `llama-server` with the selected model and context size.
5. Polls `llama-server`'s `/health` endpoint at 250 ms intervals
   until it returns HTTP 200, the child process exits (in which
   case the exit status is reported), or 300 seconds elapse.
6. Returns success to the UI.

The `/health` poll is required: `llama-server` binds its HTTP port
within milliseconds of starting but spends 5–25 seconds loading
weights. Without the poll, the chat input would unlock before the
engine is ready and the first message would return HTTP 503.

### Upgrade

The installer fetches a release manifest, downloads the new version's
artifacts into `versions/{new}.tmp/`, verifies SHA256 against the
manifest, renames the directory to `versions/{new}/`, then atomically
rewrites `current.json` with `active: {new}, previous: {old}`. Models,
catalog snapshot, license preferences, HF token, and saved chats are
not touched.

Interruption before the directory rename leaves an inert `.tmp`
directory cleaned up on next run. Interruption between the rename and
the pointer rewrite leaves the new version on disk but inactive; the
installer detects this on next run and offers to activate or discard.

### Rollback

A single atomic write to `current.json` swaps `active` and `previous`.
The N-1 version remains on the drive by default so rollback requires
no file copies.

## Invariants

The implementation maintains the following invariants. Any change to
USBuddy must preserve them.

**Catalog integrity.** Every catalog entry carries a `sha256`. Models
are verified after download and on every launch. The schema version
is `usbuddy.catalog/v1`; unknown schemas are rejected.

**RAM-fit gating.** Models that would not fit comfortably in available
memory are refused, not warned about. The advisor uses the GGUF
header's architecture fields to compute the model's actual KV-cache
size per token; there is no fixed constant.

**No background operations.** The runtime makes no network requests
the user did not initiate. There is no telemetry, no update polling,
and no catalog refresh outside an explicit user action.

**Yank survivability.** All drive writes use atomic rename through
`usbuddy_core::atomic`. The runtime does not write to the drive
during an active chat session. A mid-update yank leaves the previous
version active.

**Idle unload.** After 5 minutes of inactivity, the runtime sends
`SIGTERM` to `llama-server` to release mlocked weights. The threshold
is configurable via `--idle-timeout-secs`; `0` disables the behavior.

**Bundled engine.** `llama-server` is pinned per runtime version and
not independently updatable. GGUF format and the server API evolve
across `llama.cpp` releases; an unbounded matrix of wrapper × engine
versions is not testable. Engine-level CVEs are addressed by cutting
a runtime patch release.

**Plaintext local preferences.** `license-prefs.toml` and
`runtime-prefs.toml` are human-readable. The intent is auditability:
a borrower of the drive can read what has been agreed to on their
behalf and whether chat persistence is enabled.

## Rejected alternatives

The following technology choices were considered and rejected. Each
is rejected for architectural reasons, not preference.

**Ollama.** Installs a system service, manages a daemon process,
writes to system install paths, and persists model state in the user's
home directory. Incompatible with the runtime's footprint requirements.

**llamafile.** Bundles weights and engine into a single APE binary.
Updates require a full re-download of the binary including weights;
multiple antivirus products flag APE format; swapping models requires
swapping binaries.

**Electron, Tauri, or Wails for the chat surface.** Electron bundles
Chromium per platform (a 100 MB+ runtime per install). Tauri and
Wails depend on the system WebView; on Linux that is WebKitGTK, the
system dependency the runtime cannot have. The chat surface is a
static SPA served to the user's existing default browser.

**GTK or Qt for the installer GUI.** Forces system dependencies on
Linux hosts. `egui` is statically linked into the installer binary and
has no runtime dependencies.

## Deferred for v0.1.0

The following items are scheduled but not in v0.1.0:

- Automated host-snapshot diffing on macOS and Windows. The Linux
  job runs in CI; Windows Sandbox and macOS snapshot tooling lack
  comparable container primitives.
- Hardware-backed RAM threshold tuning. Current bands are estimated
  from published `llama.cpp` benchmarks. Empirical tuning on real
  hardware will refine the constants without API changes.
- Single-host installation of cross-platform runtimes.
  `install-runtime` currently copies the host's build; populating
  every platform requires either a release fetch or per-host
  installation.
