# USBuddy

**A portable, offline LLM that lives on a USB drive — and only there.**

You plug the stick into any reasonably modern computer. A chat window
opens in your browser. You ask the model whatever you want. You unplug.
The host machine has no record you were ever there beyond the kind of
incidental traces that any USB drive leaves.

That's the whole product.

No daemons. No phone-home. No background updates. No system tray icon
clinging on after you quit. The chat surface is a static SPA served by
a tiny Rust wrapper on localhost; the engine is `llama.cpp`; the model
weights are on the stick. Nothing else is involved.

---

## What's in scope, in one paragraph

USBuddy is two programs sharing a Rust core. The **installer** runs once
on a host to format / lay down / populate the drive — it can be heavy
because the host hosts it. The **runtime** runs ephemerally from the
USB on any host. The runtime is what has to be small, has to stay
inside its lane, and has to leave nothing behind. Conflating the two
breeds bad architecture; keeping them separate is half the reason
USBuddy exists.

The chat UI is a browser SPA, not Electron or a WebView. The installer
GUI is `egui`, not GTK/Qt. The catalog of curated models lives in this
repo and is the integrity root — every entry carries a `sha256` that is
verified on download and on every launch. Updates are atomic and
yank-survivable. Saved chats are off by default. Tradeoffs are explicit.

For the full design rationale, the constraints we're respecting, and the
stacks we ruled out and why, read [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md).

---

## What you can expect it to do

- **Run a 7-8B Q4 model fully offline** at usable speeds on any
  reasonably modern laptop. CPU inference is the always-works baseline;
  Metal / CUDA / Vulkan / ROCm are used opportunistically when present.
- **Refuse to load a model that won't fit comfortably in RAM.** A model
  that spills to swap leaks weights to disk — the #1 footprint failure
  mode. The RAM-fit advisor reads the GGUF header for the real
  KV-cache shape per model and shows you the math.
- **Cap context to what the model was actually trained for.** No more
  arbitrary 32K ceiling on a 128K model.
- **Survive a mid-write yank.** Every drive write is atomic; the
  previous version stays bootable; rollback is one file swap.
- **Idle-unload after 5 minutes** so mlocked weights don't sit in RAM
  on a borrowed laptop after you walk away. Tunable; can't be disabled
  by accident.
- **Optionally remember conversations.** Default off — chats live in
  RAM until you reload. Flip "Enable memory" to persist them in
  plaintext under `.usbuddy/chats/` on the stick (with a confirm
  warning that the stick is now the artifact).
- **Title saved chats by asking the model itself** for a 2–3 word
  summary of the first message.

---

## What you need

This isn't a phone app. To get a real experience:

- **A 64-bit host:** macOS (Apple Silicon or Intel), Linux x86_64,
  Windows x86_64. Linux/Windows ARM64 is built but lower-priority.
- **At least 16 GB RAM** for a 7-8B Q4 model. 32 GB for headroom or for
  larger / higher-precision quants. The advisor will tell you the truth
  for your hardware.
- **A USB 3.0+ drive** with ~10 GB free. exFAT format. USB 2.0 works
  but loading a 4-5 GB model off it is slow.
- **Permission to dismiss Gatekeeper / SmartScreen** the first time you
  run an unsigned binary on macOS / Windows. There is no Apple Developer
  ID or Authenticode certificate in scope; the unblock steps are
  documented in [`docs/VERIFICATION.md`](./docs/VERIFICATION.md).

---

## Status

Pre-release; no tag has been cut. The
[`release.yml`](./.github/workflows/release.yml) workflow produces
signed-by-attestation archives for macOS universal2, Linux x86_64, and
Windows x86_64 with `SHA256SUMS.txt`, a CycloneDX SBOM, and a SLSA build
provenance attestation. When the first release lands it'll be on the
[Releases page](https://github.com/skullzarmy/USBuddy/releases).

Until then, build from source.

---

## Try it

```sh
git clone https://github.com/skullzarmy/USBuddy.git
cd USBuddy
cargo build --release --workspace
```

The friendliest entry point is the GUI:

```sh
cargo run -p usbuddy-installer-gui
```

It can format a real USB stick or use any folder as a "drive" for
development. Once it's prepared the drive, the runtime is a separate
binary that lives on the drive itself — you launch it by double-clicking
`USBuddy.command` / `.bat` / `.sh` at the drive root.

If you'd rather drive it from the terminal:

```sh
DRIVE=/tmp/usbuddy-dev
cargo run -p usbuddy-installer-cli -- drive init "$DRIVE" 0.1.0
cp fixtures/catalog/official.catalog.json "$DRIVE/catalog.json"
cargo run -p usbuddy-installer-cli -- engine install "$DRIVE" --target host
cargo run -p usbuddy-installer-cli -- install-runtime "$DRIVE"
cargo run -p usbuddy-installer-cli -- model download "$DRIVE" qwen2.5-7b-instruct-q4_k_m
cargo run -p usbuddy-runtime -- serve --drive "$DRIVE" --open-browser
```

`usbuddy-installer-cli --help` prints the full command tree.
`usbuddy-installer-tui` is the same thing in ratatui form if you live in
an SSH session.

---

## Why not just use…

- **Ollama** — installs a system service, manages a daemon, writes to
  system paths. Architecturally the opposite of "no footprint."
- **LM Studio** — Electron desktop app, host-resident, account-aware.
  Great product, wrong shape for a USB stick.
- **llamafile** — bundles weights and engine into one APE binary; AV
  products dislike it; updates are coarse.
- **Tauri / Wails / Electron** for the UI — pulls in Chromium or
  system WebView. The latter on Linux is WebKitGTK, exactly what a
  zero-system-deps project can't have.

USBuddy is what's left when you remove every assumption that the host
should know you visited.

---

## Models

Five curated entries ship in
[`fixtures/catalog/official.catalog.json`](./fixtures/catalog/official.catalog.json),
spanning Qwen 2.5 7B Instruct, Mistral 7B v0.3, Llama 3.1 8B (gated),
Qwen 2.5 Coder 7B, and Dolphin 2.9.4. Any `.gguf` you drop into the
drive's `models/` directory is also discovered and shows up as
`community-unverified` in the picker.

The catalog schema, content profiles (`aligned`, `minimally-aligned`,
`base`, `code`, `vision`, `community-unverified`), and the integrity
contract are documented in
[`docs/CATALOG-SPEC.md`](./docs/CATALOG-SPEC.md).

---

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix ui/web run test
```

The web UI under `ui/web/src/` is embedded into the runtime binary at
compile time via `include_str!`; a plain `cargo build` already bundles
it. The `npm` scripts only exist for lint and bundle-presence checks.

Maintainers regenerate the curated catalog from `seed.toml` by fetching
SHA256 + size from Hugging Face's LFS pointer API (no model bytes are
downloaded):

```sh
cargo run -p xtask -- catalog-fetch
HF_TOKEN=hf_xxx cargo run -p xtask -- catalog-fetch   # for gated entries
```

Project conventions, the workspace map, and the load-bearing invariants
are in [`CLAUDE.md`](./CLAUDE.md) (it doubles as a contributor cheat
sheet — read it before opening a PR).

---

## Honest accounting

USBuddy aspires to leave no host-side footprint. "No footprint" is the
goal; "no intentional persistence; minimize incidental traces; publish
exactly what remains" is the honest version. Spotlight indexing,
Windows Prefetch, journald, Defender SmartScreen telemetry, and your
own browser history will all carry some signal that you used a USB
drive recently. The per-OS accounting is in
[`docs/FOOTPRINT.md`](./docs/FOOTPRINT.md).

---

## License

[Apache-2.0](./LICENSE). Upstream attribution and licensing for
`llama.cpp` and bundled-by-reference model weights is in
[`NOTICE`](./NOTICE) and surfaced in the UI's Credits screen.

---

*USBuddy is inspired by the concept behind
[USB-Uncensored-LLM](https://github.com/techjarves/USB-Uncensored-LLM)
— independent clean-room build, no code carried over.*
