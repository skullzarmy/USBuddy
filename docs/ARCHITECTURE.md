# Architecture

This document explains how USBuddy is structured and why the load-bearing
decisions are what they are. If you're contributing code, auditing the
project, or trying to understand whether the privacy claims hold up, start
here.

## Two programs, one core

USBuddy is two executables with opposite constraints:

The **installer** runs once on a host, online. It can be heavy — it
talks to the network, may need elevated privileges to format removable
media, ships a desktop GUI, and stays on the host as a normal
application. There are three installer surfaces (`usbuddy-installer-cli`,
`usbuddy-installer-tui`, `usbuddy-installer-gui`) that are thin views
over the same Rust core.

The **runtime** runs ephemerally from the USB drive on whatever host
you plug into. It can't assume privileges, can't write to the host, has
to fit in a tiny binary, and has to leave nothing behind when you
quit. It binds to localhost only, serves a static SPA from RAM, spawns
`llama-server` against a model on the stick, and shuts down cleanly.

Conflating these two programs would breed bad architecture — they need
opposite things from the host. Keeping them separate is half the reason
USBuddy can credibly claim "no footprint."

Both programs are built on `crates/usbuddy-core`. The core owns
everything that has correctness or security consequences: catalog
parsing and integrity checks, atomic file writes, USB layout
resolution, license preference persistence, RAM-fit estimation,
release manifest handling, platform detection, GGUF metadata parsing.
Surfaces (CLI/TUI/GUI/runtime) call into the core — they never
duplicate logic.

## Drive layout

The drive is treated as a shadow tree. Each runtime version installs
into its own subdirectory under `versions/`, and a single
`current.json` pointer at the root selects which version is active.
That single-pointer model is what makes updates atomic and rollback
trivial.

```
/USBuddy/
├── current.json                ← {"active": "0.2.0", "previous": "0.1.0", "schema": 1}
├── USBuddy.command             ← launchers for macOS / Linux / Windows
├── USBuddy.sh                    that detect arch and exec the right runtime
├── USBuddy.bat
├── versions/
│   ├── 0.1.0/
│   │   ├── bin/<os>-<arch>/    ← runtime binary + llama-server per platform
│   │   └── ui/                 ← static SPA pinned to this version
│   └── 0.2.0/                  ← parallel tree for next version
├── models/                     ← shared across versions, sha256-keyed filenames
├── catalog.json                ← catalog snapshot; refreshable independently
├── .usbuddy/                   ← user state that survives runtime upgrades
│   ├── license-prefs.toml      ← intentionally plain-text and greppable
│   ├── runtime-prefs.toml      ← incognito vs. saving toggle
│   ├── chats/                  ← saved conversations when memory is enabled
│   ├── advisories-seen.json    ← dismissed advisories
│   └── hf-token                ← only present if the user added one
└── README.txt
```

Versioned content (runtime, `llama-server`, the SPA bundle) lives under
`versions/{ver}/` and is tested as a unit. Shared content (models,
catalog, user state) lives at the drive root and persists across
runtime upgrades. The launcher shims at the root are dumb — they
read `current.json` and exec the matching binary; they almost never
change.

## Lifecycle

### Install

The installer takes a drive root, lays down the directory structure,
seeds `current.json` with a schema-versioned pointer, writes the
catalog snapshot, drops the three launcher scripts, then stages the
runtime under `versions/{version}/bin/<os>-<arch>/`. `llama-server` is
downloaded from pinned `llama.cpp` releases per supported host
(`engine install <drive> --target all` populates every platform from
one host so the stick is portable).

### Launch

When the user double-clicks a launcher, the script reads `current.json`,
locates `versions/<active>/bin/<os>-<arch>/usbuddy-runtime`, and execs
it with `--drive <self> --open-browser`. The runtime opens its tray
icon, starts an HTTP server on `127.0.0.1:8765`, opens the user's
default browser, and waits. It does not spawn `llama-server` until the
user clicks **Launch** in the UI.

### Engine load

When the user picks a model and clicks Launch, the runtime:

1. Resolves the model file under `models/` and verifies its size.
2. Reads the GGUF header to get the real architecture (layers, heads,
   KV heads, embedding dim, trained context length).
3. Runs the RAM-fit advisor against detected memory using the model's
   actual KV-cache shape. Red band aborts the launch.
4. Spawns `llama-server` with the chosen model and context size.
5. **Polls `llama-server`'s `/health` endpoint until it returns 200.**
   Without this poll, the UI would unlock the chat input while
   `llama-server` is still loading weights (5–25 s), and the first
   message would race the load and come back as a 503 error.
6. Returns success to the UI.

### Upgrade

The installer parses a release manifest, stages the new version under
`versions/{new}.tmp/`, verifies SHA256 against the manifest, renames
the directory to `versions/{new}/`, and atomically rewrites
`current.json` with `active: {new}, previous: {old}`. Models, catalog,
license preferences, HF token, and saved chats are all under shared
roots and never touched.

If the install is interrupted before the rename, the `.tmp` directory
is inert and gets cleaned up next time. If it's interrupted between
the rename and the `current.json` rewrite, the new tree is on disk
but the previous version is still active — the installer detects this
on next run and offers to activate or discard.

### Rollback

A single atomic write to `current.json` that swaps `active` and
`previous`. No file copies. The N-1 version is kept on the drive by
default precisely so rollback is one click.

## Load-bearing invariants

These are the rules the implementation enforces and that any change
to USBuddy has to preserve:

- **Catalog integrity.** Every catalog entry carries a `sha256`.
  Models are verified on download and on every launch. The schema
  version is `usbuddy.catalog/v1`; unknown schemas hard-fail rather
  than degrade silently.

- **RAM-fit gating.** Models that would spill to swap refuse to load.
  Swap-to-disk is the #1 way a local LLM leaks weights to the host;
  this is enforcement, not advice. The advisor parses the GGUF header
  to get the model's real KV-cache shape — no fudge constants.

- **No background work.** The runtime never makes a network call the
  user didn't ask for. There is no telemetry, no auto-update check,
  no catalog refresh in the background. Every network operation is
  explicitly initiated.

- **Yank survivability.** Every drive write goes through the atomic
  rename pattern in `usbuddy_core::atomic`. The runtime never writes
  to the drive during an active chat session. A mid-update yank leaves
  the previous version active and launchable.

- **Idle unload.** After 5 minutes of no chat activity, the runtime
  `SIGTERM`s `llama-server` so model weights leave mlocked RAM. This
  is default-on because a stick plugged into a borrowed laptop while
  the owner walks away is exactly the threat model that "no footprint"
  is supposed to defeat. Configurable via `--idle-timeout-secs`;
  `0` disables.

- **Bundled engine.** `llama-server` is pinned per runtime version and
  not independently updatable. GGUF format and the server API drift
  across `llama.cpp` releases; testing a matrix of every wrapper
  version against every engine version doesn't scale. If a CVE drops
  in `llama.cpp`, USBuddy cuts a patch release of the runtime.

- **Plaintext local prefs.** `license-prefs.toml` and
  `runtime-prefs.toml` are intentionally human-readable. Anyone
  borrowing the drive can `grep` them to see what's been agreed to
  on their behalf and whether chat persistence is on.

## What's not USBuddy

We made conscious choices not to use certain stacks. Each is also
documented in the README's comparison section; here are the
architectural reasons:

- **Ollama** installs a system service, manages a daemon, writes to
  system install paths, and persists model state in the home
  directory. Every one of those is the opposite of what USBuddy
  needs.

- **llamafile** ships weights and engine as one APE binary. That
  makes updates coarse and trips antivirus on multiple hosts. We
  also lose the ability to swap models without redownloading.

- **Electron / Tauri / Wails** for the chat UI would either bundle
  Chromium (a per-platform 100 MB+ runtime, a tray dependency, an
  update vector) or depend on the system WebView. On Linux the
  system WebView is WebKitGTK — the exact category of system
  dependency USBuddy can't have.

- **A native GUI toolkit (GTK/Qt)** for the installer would force
  system deps on every Linux host. `egui` is bundled into the
  installer binary and depends on nothing.

The chat surface is a static SPA opened in the user's default browser.
The installer GUI uses `egui` so it ships as one binary with no
runtime dependencies.

## Deferred for v0.1.0

Three things are intentionally out of scope for the first tagged
release:

- **Automated host-snapshot diffing across all three OSes.** The
  Linux footprint job runs in CI; Windows Sandbox and macOS
  snapshot tooling are tracked follow-ups.

- **Hardware-backed RAM threshold tuning.** The bands (green ≥ 20%
  margin, yellow / red as defined in `usbuddy_core::ram`) are best
  estimates from `llama.cpp`'s published numbers. Real-hardware
  measurement will refine the constants without changing the API.

- **Cross-host runtime installation from a single host.**
  `install-runtime` today copies the *host's* build. Populating
  every platform from one host requires either pulling from a
  published release or running `install-runtime` on each host.
