# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

USBuddy is a zero-install, portable local LLM that lives on a USB drive. It is **two programs** with opposite constraints that share one Rust core:

- **The installer** — runs once on the host, online, can be heavier (CLI / TUI / GUI).
- **The runtime** — runs ephemerally from the USB on any host, must be tiny, offline by default, leave no footprint.

Read `README.md` and `docs/ARCHITECTURE.md` for the full design rationale before making structural changes. Several constraints (exFAT layout, shadow-tree `versions/`, yank-safety, mlock / no-swap posture, no auto-updates) are load-bearing and called out in the README — don't quietly relax them.

## Workspace layout

Cargo workspace (`resolver = "2"`, edition 2024). Members:

- `crates/usbuddy-core` — single source of truth. All real logic: catalog, layout, hash, atomic writes, RAM-fit, license, download, engine install, release manifest, platform detect. Every installer surface and the runtime call into this.
- `crates/usbuddy-installer-cli` — the workhorse. Scriptable; full command tree (`drive`, `catalog`, `model`, `engine`, `install-runtime`, `update`, `license`, `ram-assess`).
- `crates/usbuddy-installer-tui` — `ratatui` interactive shell. Thin surface over core.
- `crates/usbuddy-installer-gui` — `eframe`/`egui` desktop app. Thin surface over core.
- `crates/usbuddy-runtime` — localhost HTTP server (`axum`) that spawns/kills `llama-server`, reverse-proxies chat, serves the embedded SPA, and idle-unloads weights after 5 min.
- `xtask` — maintainer tool. `catalog-fetch` regenerates `fixtures/catalog/official.catalog.json` from `seed.toml` by fetching SHA256+size from HF LFS pointers (never downloads model bytes).
- `ui/web` — React + TypeScript + Vite + Tailwind SPA (Radix primitives, zustand state). Built into `ui/web/dist/` with fixed filenames (no content hashes); the runtime embeds `dist/index.html`, `dist/assets/app.js`, `dist/assets/styles.css` via `include_str!` at compile time. **`dist/` is committed** so a plain `cargo build` still bundles it — after changing UI sources you must run `npm --prefix ui/web run build` and commit the regenerated `dist/`.

When adding behavior, default to putting it in `usbuddy-core` and exposing it via all three installer surfaces + (where relevant) the runtime. Don't duplicate logic into the CLI/TUI/GUI crates.

## Common commands

Development uses a scratch directory as a "drive" — a real exFAT USB is not required.

```sh
# Build everything
cargo build --release --workspace

# Full validation (run before pushing)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix ui/web run test

# Single test
cargo test -p usbuddy-core <test_name>
cargo test -p usbuddy-installer-cli -- --nocapture <test_name>

# Web UI (needed for lint / tests / rebuilding the embedded bundle)
npm --prefix ui/web install
npm --prefix ui/web run lint    # tsc --noEmit
npm --prefix ui/web run test    # vitest
npm --prefix ui/web run build   # regenerates ui/web/dist (commit it)
npm --prefix ui/web run dev     # vite dev server, proxies /api to :8765

# Maintainer: regenerate the curated catalog from seed.toml
cargo run -p xtask -- catalog-fetch
HF_TOKEN=hf_xxx cargo run -p xtask -- catalog-fetch   # includes gated entries
```

### Typical dev loop against a scratch "drive"

```sh
DRIVE=/tmp/usbuddy-dev
cargo run -p usbuddy-installer-cli -- drive init "$DRIVE" 0.1.0
cp fixtures/catalog/official.catalog.json "$DRIVE/catalog.json"
cargo run -p usbuddy-installer-cli -- engine install "$DRIVE" --target host
cargo run -p usbuddy-installer-cli -- install-runtime "$DRIVE"
cargo run -p usbuddy-installer-cli -- model download "$DRIVE" qwen2.5-7b-instruct-q4_k_m
cargo run -p usbuddy-runtime -- serve --drive "$DRIVE" --open-browser
```

`install-runtime` copies the **host's** current build — to populate the drive for other platforms you must run it on each host (or wait for a release).

## Drive layout (shadow-tree)

Every change that writes to the drive must respect the layout in `README.md` §"USB drive layout":

- **Versioned** (under `versions/{ver}/`): wrapper binary, per-OS `llama-server`, SPA bundle. Tested as a unit.
- **Shared** (drive root): `models/` (sha256-keyed filenames, cross-version), `catalog.json`, `.usbuddy/` user data (license-prefs, hf-token, advisories-seen), launcher shims.
- `current.json` at the root selects the active version. Updates are: download to `{new}.tmp/` → verify SHA256 → atomic rename → atomic rewrite of `current.json`. Use `usbuddy_core::atomic` for these — every drive write must be yank-survivable.

Never write to the drive during a runtime session. Runtime state is RAM-only.

## Non-negotiable invariants

- **Catalog is the trust root.** Every entry carries a `sha256`. Verify on download AND on every launch. Schema version is `usbuddy.catalog/v1`; unknown schema → hard error, never silent fallback. Spec lives in `docs/CATALOG-SPEC.md` and `schemas/catalog.schema.json`.
- **RAM-fit advisor gates model loads.** Green/Yellow/Red bands in `usbuddy-core::ram`. Red refuses to load (swap-to-disk is the #1 footprint leak). Mid-session swaps re-check.
- **No background work, no phone-home.** All update checks, catalog refreshes, and downloads are user-initiated.
- **`llama-server` is bundled with each runtime version, not independently updatable.** GGUF/runtime-API drift makes the test matrix explode otherwise.
- **License acceptance is recorded as `(model_id, license_sha256, timestamp, host_at_accept)`** and re-prompts when `license_sha256` changes. The opt-out file `.usbuddy/license-prefs.toml` is intentionally plain-text and greppable.

## Releases

- `ci.yml` runs lint/test/build-check/audit on PRs and pushes to `main`.
- `release.yml` is **manual `workflow_dispatch`** with a `version` input. Auto-creates the tag `v{version}`, builds the matrix (windows-x64, macos-universal2 via `lipo`, linux-x64), generates `SHA256SUMS.txt`, CycloneDX SBOM, and SLSA build provenance attestation, then produces a draft release. Maintainer publishes manually.
- `llama.cpp` binaries and model weights are **not** in the release bundle — the installer fetches them at install time.
- `footprint.yml` runs a Linux snapshot-diff on runtime-touching PRs.

## Conventions

- The launcher scripts at the repo root (`launch-*.sh/.command/.bat`) ship to the **drive**, not the host. They are not how you start development.
- Don't introduce GTK / WebKit / Qt / Electron / Tauri / WebView dependencies. The chat surface is a static SPA opened in the user's default browser; the installer GUI is `egui` precisely to avoid system UI deps. See README §"Ruled out, with reasons".
- Fixtures: use `fixtures/catalog/official.catalog.json` as the canonical seed for `drive init` flows in tests and docs.
