# USBuddy

> A private, portable LLM that lives on a USB drive. Plug it into any machine, chat, unplug, walk away. Nothing installs, nothing phones home, nothing stays behind.

---

## What it does

USBuddy turns a USB stick into a self-contained AI workstation:

- **Bring your own brain.** A curated model (or any GGUF you drop in) and the
  llama.cpp engine all live on the drive — not the host.
- **Localhost only.** The chat UI is a static SPA served by a tiny Rust
  wrapper at `http://127.0.0.1:8765`. No network calls, no telemetry,
  no auto-update pings.
- **Zero install on the host.** Double-click `USBuddy.command` /
  `.bat` / `.sh` on the drive. The runtime spawns, opens your default
  browser, and shuts down cleanly when you quit.
- **Yank-safe.** All drive writes are atomic. Pull the stick mid-session
  and the worst case is "session ends" — no half-written files, no
  corrupted state.

Inspired by the *concept* behind
[USB-Uncensored-LLM](https://github.com/techjarves/USB-Uncensored-LLM).
Independent, clean-room build — no code carried over.

---

## Status

**Pre-release.** No tagged release has shipped yet. The
[`release.yml`](./.github/workflows/release.yml) workflow is wired and the
maintainer fires it manually when a build is ready. Until then, build from
source — see below.

When releases ship, you'll get signed, attested archives for macOS / Linux
/ Windows from the
[Releases page](https://github.com/skullzarmy/USBuddy/releases),
containing the installer plus the runtime, the SPA, launcher shims, and a
starter catalog. No `llama-server` or model weights are bundled — the
installer fetches those at install time.

---

## Getting started (from source)

You need [Rust stable](https://rustup.rs) (edition 2024) and a folder
anywhere on your machine to act as the "drive." A real USB stick isn't
required for development — any directory works.

```sh
git clone https://github.com/skullzarmy/USBuddy.git
cd USBuddy
cargo build --release --workspace
```

### Three installers, your pick

All three speak to the same Rust core. Use whichever fits how you work.

| Surface                   | Best for                                                |
| ------------------------- | ------------------------------------------------------- |
| `usbuddy-installer-gui`   | Desktop window. The default if you just want to click.  |
| `usbuddy-installer-tui`   | Terminal menu. SSH-friendly, dotfiles-friendly.         |
| `usbuddy-installer-cli`   | Scriptable. Full command tree under `--help`.           |

```sh
# Pick one:
cargo run -p usbuddy-installer-gui
cargo run -p usbuddy-installer-tui
cargo run -p usbuddy-installer-cli -- --help
```

### Full CLI flow against a scratch drive

```sh
DRIVE=/tmp/usbuddy-dev

# Lay down the shadow-tree layout (current.json, .usbuddy/, models/, etc.)
cargo run -p usbuddy-installer-cli -- drive init "$DRIVE" 0.1.0

# Seed with the curated catalog (real SHA256s pulled from upstream LFS)
cp fixtures/catalog/official.catalog.json "$DRIVE/catalog.json"

# Download llama.cpp release binaries for every supported host (~60-90 MB),
# or `--target host` for just the platform you're on.
cargo run -p usbuddy-installer-cli -- engine install "$DRIVE" --target host

# Copy this build's runtime onto the drive for the current host.
cargo run -p usbuddy-installer-cli -- install-runtime "$DRIVE"

# Pull a model.
cargo run -p usbuddy-installer-cli -- model download "$DRIVE" qwen2.5-7b-instruct-q4_k_m

# Run it.
cargo run -p usbuddy-runtime -- serve --drive "$DRIVE" --open-browser
```

That last command serves the chat UI on
`http://127.0.0.1:8765`, opens your default browser, and starts a
tray icon for quit/stop. The runtime spawns `llama-server` on port 8766
when you click **Launch**. After 5 minutes of inactivity it SIGTERMs
`llama-server` so weights leave mlocked RAM — disable with
`--idle-timeout-secs 0`.

### Drive-side launchers

`drive init` writes `USBuddy.command` (macOS), `USBuddy.sh` (Linux), and
`USBuddy.bat` (Windows) to the drive root. Those are what you double-click
on any host once the stick is set up — they detect OS/arch, locate the
per-platform runtime under `versions/<active>/bin/<os>-<arch>/`, and exec
it. On macOS the `.command` script closes its Terminal window when the
runtime exits cleanly.

---

## Models

USBuddy ships a small curated catalog covering the four content profiles
(see [`docs/CATALOG-SPEC.md`](./docs/CATALOG-SPEC.md)):

| Profile               | Means                                                                                  |
| --------------------- | -------------------------------------------------------------------------------------- |
| `aligned`             | Standard instruct + safety training. Default, no warning.                              |
| `minimally-aligned`   | Instruct, with refusal training removed/reduced (Dolphin, Hermes, Nous).               |
| `base`                | Pretrained foundation, no instruct tuning. Will complete anything.                     |
| `code`                | Code-specialized (Qwen Coder, DeepSeek Coder).                                         |
| `community-unverified`| Any `.gguf` you drop in `/models/` on the drive. Persistent badge in the picker.       |

The picker enforces a **RAM-fit advisor** on every launch: green / yellow /
red bands measured against available RAM and the model's real KV-cache
size (parsed from the GGUF header). Red refuses to load — swap-to-disk is
the #1 footprint leak.

Saved chats are **off by default** (incognito). Toggle "Enable memory"
in the chat header to persist conversations under `.usbuddy/chats/` on
the drive in plaintext, with a one-time warning that the stick becomes
the artifact at that point.

---

## How it's built

Two programs with opposite constraints, sharing one Rust core:

- **The installer** — runs once on the host, can be heavier (CLI / TUI / GUI).
- **The runtime** — runs ephemerally from the USB on any host, must be
  tiny, offline by default, leave no footprint.

Architecture decisions (exFAT, shadow-tree `versions/`, yank-safety,
mlock posture, no auto-updates, no system WebView dependencies) are
written up in [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md). The
runtime serves a static SPA opened in the user's default browser; the
installer GUI uses `egui` precisely to avoid GTK / WebKit / Qt /
WebView dependencies.

Further reading:

- [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) — full design rationale.
- [`docs/CATALOG-SPEC.md`](./docs/CATALOG-SPEC.md) — catalog schema.
- [`docs/FOOTPRINT.md`](./docs/FOOTPRINT.md) — honest accounting of
  residual host traces (Spotlight, Prefetch, journald, etc.).
- [`docs/VERIFICATION.md`](./docs/VERIFICATION.md) — how to verify what
  a release shipped.

---

## Development

```sh
# Full validation (run before pushing)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix ui/web run test

# Single test
cargo test -p usbuddy-core <name>
```

The web UI under `ui/web/src/` is embedded into the runtime binary at
compile time via `include_str!` — a plain `cargo build` already bundles
it. The `npm` scripts exist only for linting and bundle-validation tests.

Maintainers regenerate the curated catalog from `seed.toml`:

```sh
cargo run -p xtask -- catalog-fetch
HF_TOKEN=hf_xxx cargo run -p xtask -- catalog-fetch   # for gated entries
```

---

## License

[Apache-2.0](./LICENSE). See also [`NOTICE`](./NOTICE) for upstream
attribution requirements (llama.cpp, model weights).
