# USBuddy

> A private, safe, portable LLM that lives on your USB drive.

## What it is

USBuddy is a zero-install, portable local AI environment: a high-quality LLM
that runs **fully offline** directly from a USB drive or external SSD. Plug it
into any machine, run the AI, unplug, and walk away. The application code and
model parameters all live on the drive.

> Inspired by the *concept* behind
> [USB-Uncensored-LLM](https://github.com/techjarves/USB-Uncensored-LLM).
> USBuddy is an independent, clean-room build — no code is carried over.

---

## Requirements (user-facing)

### A. Easy, cross-platform installer
- Cross-platform installer UI (Windows / macOS / Linux).
- Intelligently handles **formatting and installation onto the USB drive**.
- **Proper security** throughout.
- **Source verification** of everything it downloads.
- A **dynamic, "aware" wizard-style setup** that adapts to the host machine.

### B. Secure, encapsulated AI
- The AI runs **securely and self-contained**.
- Offline by default; no surprise network exposure.

### C. Truly portable — no footprint after disconnect
- Portable to a new machine **without leaving a footprint after disconnect**.
- Loads into **RAM** (and, if needed, temporary disk space).
- **Cleans up on exit/eject** and leaves the host machine no different — no
  traces beyond normal user activity (e.g., browser history).

---

## Architecture overview

USBuddy is **two programs** with opposite constraints. Conflating them leads to
bad tech choices.

|                     | The Installer                              | The Runtime (lives on USB)                 |
| ------------------- | ------------------------------------------ | ------------------------------------------ |
| Runs where          | Host OS, once                              | Host OS, ephemerally, anywhere             |
| Privileges          | Likely needs admin/sudo (to format)        | Must work as unprivileged user             |
| Footprint on host   | Doesn't matter (normal app)                | Must be ~zero                              |
| Network             | Online to fetch + verify                   | Offline by default                         |
| UI weight           | Can be richer                              | Must be tiny, no system deps               |
| Distribution        | GitHub Releases, per-OS                    | Drops to USB; user double-clicks launcher  |

Both programs follow a **CLI-first** internal structure: a Rust CLI binary
does all real work (drive detection, formatting, catalog fetch, downloads,
verification, install, launch, cleanup). The GUI (`egui`) and TUI (`ratatui`)
are thin surfaces that shell into the CLI. Single source of truth, fully
scriptable, automatable, and testable.

### Honest constraints baked into the design

These are consequences of the stated requirements. Any stack must satisfy them.

1. **Drive format must be exFAT.** Only viable cross-OS format for files
   >4 GB. No Unix permissions; the runtime assumes a fully-readable drive.
2. **AutoRun is dead.** Post-Stuxnet, no OS auto-executes from USB. The drive
   root ships a per-OS launcher (`launch-windows.exe`, `launch-macos.command`,
   `launch-linux.sh`).
3. **No app stores; no paid code-signing in scope.** Distribution is via this
   repo's Releases. Documented Gatekeeper / SmartScreen unblock steps where
   applicable. No Apple Developer ID, no Authenticode cert. This is a
   conscious tradeoff, documented to users.
4. **Swap/pagefile is the #1 footprint leak vector.** Loading a model that
   won't fit in RAM causes the OS to write weights to `pagefile.sys` / swap,
   which persists after eject. The runtime uses `VirtualLock` / `mlock` where
   possible and **refuses to load a model that won't fit comfortably in
   available RAM**.
5. **"Zero footprint" is aspirational.** Unavoidable incidental traces:
   Windows Prefetch, macOS unified log, Linux journald, Spotlight indexing of
   the USB, Defender SmartScreen telemetry, recent-files registries, browser
   history. The honest framing is **"no intentional persistence; minimize
   incidental traces; publish exactly what we know remains"** in
   `docs/FOOTPRINT.md`.
6. **USB I/O is the bottleneck.** A 7B-Q4 model is ~4 GB. Stream loads with
   real progress UI; never freeze.
7. **GPU is opportunistic, never assumed.** CPU is the always-works baseline.
   CUDA / Metal / Vulkan / ROCm are detected and used when available.
8. **The drive can vanish at any moment.** No writes to the USB during a
   session; state held in RAM only. Yank-resistance is a hard requirement.

---

## The stack

| Layer                | Choice                                                                                                | Rationale (short)                                                                                                                                          |
| -------------------- | ----------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Inference engine** | **`llama.cpp` (`llama-server`)**                                                                       | Most portable engine; broadest backend matrix (CPU / CUDA / Metal / Vulkan / ROCm); GGUF format; MIT-licensed; OpenAI-compatible HTTP API on localhost.    |
| **Runtime wrapper**  | **Rust**                                                                                               | Memory-safe (this code touches the host); tiny static binaries; deterministic cleanup via `Drop`; first-class crypto; robust process/signal handling.      |
| **Chat UI**          | **Static SPA, served by wrapper, opened in user's default browser in private mode** (+ CLI/TUI fallback) | No frontend runtime on USB; no WebView dep; talks directly to `llama-server`'s OpenAI-compatible API.                                                       |
| **Installer surfaces** | **CLI (foundation) + `egui` GUI + `ratatui` TUI**, all calling the same Rust core                    | No system UI deps (no GTK / WebKit / Qt). CLI is the workhorse; GUI/TUI are thin presentation layers. Scriptable, dotfiles-friendly, SSH-friendly.          |

### Ruled out, with reasons

- **Ollama** — daemon/service model, installs into system paths, registers
  background services. Architecturally opposite to "no footprint."
- **llamafile** — bundles weights + engine into one APE binary; bad for
  updates; trips some EDR/AV products.
- **MLC-LLM / vLLM / TGI / SGLang** — server-class or research stacks;
  heavier deps; weaker portability story.
- **Electron / Tauri / Wails** — bundles Chromium (Electron) or depends on
  system WebView (Tauri/Wails on Linux = WebKitGTK, the exact problem this
  project must avoid).
- **Native cross-platform GUI for the chat surface** (Qt / GTK) — heavy,
  per-OS pain, no real benefit over a browser for a chat interface.

---

## Model layer

llama.cpp is engine-agnostic by design — GGUF is the portable container.
USBuddy adds a **catalog**, a **picker**, a **verifier**, and a **RAM-fit
advisor** on top.

### In-repo catalog (`catalog.json`)

- **Catalog lives in this repo**, maintained by the project. The repo is the
  trust root.
- HTTPS + GitHub authenticates transport and authorship; git history is the
  audit log.
- **Forkable.** Anyone can fork the repo (or just the catalog) and point their
  installer at a custom URL.
- **No Sigstore / cosign infrastructure.** Repo authentication is sufficient
  for this distribution model. SHA256 per model entry is mandatory — that's
  integrity, not signing.

### Catalog format

- **Format:** JSON. Machine-generated, schema-validatable, diffable in PRs.
- **Schema versioning:** `schema: "usbuddy.catalog/v1"` at the root. Installer
  refuses unknown schema versions with a clear "upgrade USBuddy" error.
- **Entry granularity:** **flat** — one entry per downloadable artifact.
  Family relationships expressed via a `family_id` field
  (`llama-3.1-8b-instruct-q4_k_m` and `llama-3.1-8b-instruct-q5_k_m` share
  `family_id: "llama-3.1-8b-instruct"`). Picker groups by family in UI;
  storage is flat.
- **Prompt templates:** reference by name (`prompt_template: "chatml"`,
  `"llama3"`, `"mistral"`) — llama-server already implements these. An
  embedded override field exists for truly custom templates.
- **Capabilities:** array of strings — `["chat", "function_calling",
  "json_mode", "vision", "code", "long_context"]`. Picker can filter.
- **Aliases:** `aliases: []` per entry, so renames don't break existing
  installs' `catalog.local.json` references.
- **Update channels:** single `stable` channel for v1. Multi-channel deferred
  until there's a real use case.
- Full schema lives in `docs/CATALOG-SPEC.md` (forthcoming).

### Three sources of models, one picker

1. **Catalog models** — curated, in the official `catalog.json`. License
   acceptance handled by USBuddy.
2. **Custom catalogs** — power users / orgs can add additional catalog URLs
   (their own mirror, a community catalog). Each is a separate trust decision
   by the user.
3. **Drop-in** — any `.gguf` file copied to `/models/` shows up in the picker,
   parsed from GGUF embedded metadata. Flagged `community-unverified` in UI.
   License handling is the user's responsibility (out of scope).

### Content profiles (replaces "uncensored" branding)

| Label                  | Meaning                                                                                | UI treatment                                          |
| ---------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `aligned`              | Standard instruct + safety training (Llama-Instruct, Qwen-Instruct, Mistral-Instruct). | Default. No warning.                                  |
| `minimally-aligned`    | Instruct-tuned, refusal training removed/reduced (Dolphin, Hermes, Nous).              | One-time confirmation per model.                      |
| `base`                 | Pretrained foundation, no instruct tuning. Will complete anything.                     | One-time confirmation + custom system prompt required. |
| `code`                 | Code-specialized (Qwen-Coder, DeepSeek-Coder).                                         | No warning.                                           |
| `vision`               | Multimodal.                                                                            | Future — adds complexity.                             |
| `community-unverified` | Drop-in GGUF, no catalog entry.                                                        | Persistent badge in UI.                               |

The capability the source repo flagged as "uncensored" is fully preserved via
the `minimally-aligned` and `base` profiles. The framing is factual,
jurisdictionally portable, and avoids marketing / contributor / CA-review
baggage. The word "uncensored" may appear in upstream model descriptions; it
is not used in the USBuddy product surface.

### Integrity & gated models

- Every catalog entry carries `sha256`. Verified at download **and** at every
  launch (USB corruption is real).
- **Gated models** (Llama family, etc., requiring upstream approval): catalog
  entry sets `auth: { type: "hf_token", gate_url: "..." }`.
  - **Primary path:** user provides a HuggingFace token, stored on the USB
    with clear docs on storage posture. USBuddy handles managed download and
    license acceptance.
  - **Fallback:** if no token, USBuddy walks the user through manual download
    on the gate URL and accepts the resulting `.gguf` as a drop-in.

### RAM-fit advisor

Runs every launch, on every host. Doubles as a **footprint security control**:
a model that doesn't fit in RAM will swap to disk, which is the #1 leak
vector.

Bands, measured against **detected available RAM** (not total):

| Band   | Rule                                                                              | Behavior                                                     |
| ------ | --------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Green  | model + KV cache + overhead fits with ≥ 20% margin; ≥ 3 GB host OS headroom left  | Loads silently.                                              |
| Yellow | fits but margin < 20%, OR host headroom < 3 GB but ≥ 1 GB                          | One-time "may slow your computer" notice; loads.             |
| Red    | doesn't fit, OR would leave host with < 1 GB headroom                              | **Refuses to load.** Suggests smaller quant or shorter context. |

Refinements baked in:

- **Context-length slider** in the picker with live RAM impact (KV cache grows
  with context). User can reduce context to shift Yellow → Green.
- **Idle unload, default-on, 5 min threshold.** After 5 min of no activity,
  llama-server unloads the model from mlocked RAM. Reloads on next message
  (~2–5 s). Toggleable in config. Default-on because idle mlocked weights
  contradict the entire footprint pitch.

Initial threshold numbers are best-guess from llama.cpp's published figures
and will be empirically tuned on real hardware before v0.1.0 GA.

### Mid-session model swap

- **Architecture:** kill-and-restart. SIGTERM the running `llama-server`,
  spawn a new one with the new model. Conversation state lives **in the
  wrapper as messages** (not as tokenized state), so it's model-agnostic.
- **On swap:** wrapper sends full message history as a normal OpenAI-compat
  chat completion to the new model. llama-server's prompt caching makes
  subsequent messages fast.
- **UI:** "Switching to {model}..." with real progress; model load is the
  bottleneck.
- **Refuses swap if new model fails RAM-fit.** Suggests alternatives.
- **Forced "start fresh conversation" on `instruct` ↔ `base` profile-boundary
  swaps.** Base models don't speak chat format and derail. Same-profile
  swaps carry history normally.
- **No two-process hot swap.** Doubles RAM use and directly fights the
  RAM-fit advisor.

---

## License handling

Three-tier model:

1. **Managed (catalog downloads)** — USBuddy adheres to the letter and spirit
   of upstream licenses. Per-model: a "View license" link expands the full
   license text, and a single "I accept the license" checkbox sits next to
   the model in the picker. Install button stays disabled until every
   selected model's checkbox is ticked. Acceptance recorded as
   `(model_id, license_sha256, timestamp, host_at_accept)`. Re-prompts if
   upstream license changes. A "Credits" screen in the chat UI shows required
   attribution.
2. **Drop-in** — user's responsibility. Out of scope.
3. **Opt-out** — auditable, plain-text config (`/.usbuddy/license-prefs.toml`)
   with `scope = "all" | "permissive_only" | "none"`. `permissive_only`
   auto-accepts Apache / MIT / BSD-style and still prompts on restrictive
   terms (Llama community license, etc.).

The opt-out file is visible and greppable so anyone borrowing the drive can
see what's been agreed to on their behalf.

---

## Security & footprint posture

### Code signing
- **Not in scope.** No App Store distribution; no paid Apple Developer ID; no
  Authenticode cert.
- **macOS:** Unsigned binaries trip Gatekeeper. Unblock steps are documented
  consistently in `README`, `INSTALL.md`, and the installer's macOS screen
  itself (three places, same wording). The installer strips
  `com.apple.quarantine` from binaries written to the USB so Gatekeeper
  doesn't trip on every host.
- **Windows:** SmartScreen "More info → Run anyway" documented similarly.
- **Linux:** Nothing needed.

### Footprint
- `docs/FOOTPRINT.md` documents per-OS residual traces honestly.
- A future `footprint.yml` workflow will run snapshot-diff tests (Windows
  Sandbox / fresh macOS runner / Docker) nightly to catch regressions.
  **Deferred** — not blocking v0.1.0.

### Trust root
- **This GitHub repo. That's it.** No external PKI.

---

## USB drive layout

```
/USBuddy/
├── launch-windows.exe          ← per-OS launcher
├── launch-macos.command
├── launch-linux.sh
├── bin/
│   ├── windows-x64/            ← wrapper + llama-server variants
│   ├── macos-arm64/llama-server
│   ├── macos-x64/llama-server
│   ├── macos/usbuddy-wrapper   ← universal2; picks arch at runtime
│   └── linux-x64/
├── ui/                         ← static SPA bundle
├── models/                     ← GGUFs (catalog + drop-in)
│   └── catalog.local.json      ← provenance for installed models
├── catalog.json                ← snapshot at install time
├── .usbuddy/
│   ├── trust/                  ← repo trust info
│   ├── license-prefs.toml      ← visible audit trail
│   └── version.json
└── README.txt                  ← plain-text user-facing note
```

---

## Build, release & supply chain

Two GitHub Actions workflows. Hard split — different triggers, different cost
profiles, different risk.

### `ci.yml` — always-on (PR + push to `main`)

| Job           | Purpose                                                                                |
| ------------- | -------------------------------------------------------------------------------------- |
| `lint`        | `cargo fmt --check`, `cargo clippy -- -D warnings`. Single Linux runner.                |
| `test`        | `cargo test` on the full matrix. Path / process / FS semantics differ per OS.           |
| `build-check` | `cargo build --release` on the matrix. Catches "works on my Mac" before release day.    |
| `audit`       | `cargo audit` for CVEs. Weekly cron + on-PR. Annotation only; doesn't fail the build.   |

Cached with `Swatinem/rust-cache@v2`.

### `release.yml` — manual fire (`workflow_dispatch`)

**Inputs:**
- `version` (required, semver string — e.g. `0.1.0` or `0.2.0-rc.1`)
- `draft` (default: `true`)
- `prerelease` is **auto-derived** from presence of `-` in `version`

**Pipeline:** `validate → test → build (matrix) → package → release`

**Build matrix:**

| Output binary                                | Runner          | How                                                                                                  |
| -------------------------------------------- | --------------- | ---------------------------------------------------------------------------------------------------- |
| `usbuddy-installer-windows-x64.exe`          | `windows-latest`| `x86_64-pc-windows-msvc`.                                                                            |
| `usbuddy-installer-macos-universal.tar.gz`   | `macos-latest`  | Build `aarch64-apple-darwin` + `x86_64-apple-darwin`, then `lipo -create` into one universal2 binary. |
| `usbuddy-installer-linux-x64.tar.gz`         | `ubuntu-latest` | `x86_64-unknown-linux-gnu`.                                                                          |

- **macOS:** single universal2 download supports both Apple Silicon and Intel
  Macs. No "which chip do I have?" UX friction; halves macOS CI minutes vs.
  separate runners.
- **Linux ARM:** deferred for v0.1.0. Power users build from source.

**Per-build job steps:**
1. Checkout at commit.
2. Inject `version` into build-time env (`env!("USBUDDY_VERSION")`).
3. Build installer + wrapper + launchers; bundle static SPA + catalog snapshot.
4. Compute SHA256 of installer.
5. Upload artifact.

> **Not in the release bundle:** llama.cpp binaries and model weights. The
> installer downloads these at install time, per the `NOTICE`'s
> upstream-licensure boundary. Keeps releases small (~10–20 MB).

**Package step:**
- Aggregate matrix artifacts.
- Generate `SHA256SUMS.txt` covering every release asset.
- Generate **SBOM (CycloneDX)** via `cargo-cyclonedx` — fulfills the `NOTICE`'s
  "signed artifact manifest" promise.
- Generate **SLSA build provenance** via
  `actions/attest-build-provenance@v1` — GitHub-native, free, lets users run
  `gh attestation verify` against the binary.

**Release step:**
- Workflow **auto-creates the tag** `v{version}` at the build commit (single
  source of truth).
- Creates a **draft** release with all assets + `SHA256SUMS.txt` + SBOM +
  provenance attestation.
- Release notes auto-generated via `generate_release_notes: true`.
- Maintainer reviews the draft, edits notes, publishes manually.

### Deferred (separate workflows, post-v0.1.0)
- `footprint.yml` — snapshot-diff regression tests on Windows Sandbox / macOS
  / Docker.
- `catalog-validate.yml` — JSON-schema check + URL liveness + upstream SHA256
  verification, gating catalog PRs.
- End-to-end install test on real USB-like volumes.

---

## Status

- Repo scaffolded.
- Architecture & plan: **locked in** (this document).
- Code: **not yet started.** Next groundwork: `docs/ARCHITECTURE.md`,
  `docs/CATALOG-SPEC.md`, and `docs/FOOTPRINT.md` before any code lands.
- License: **Apache-2.0** (see `LICENSE` and `NOTICE`).

### Decided
- Inference engine: **llama.cpp / `llama-server`**
- Wrapper language: **Rust**
- Installer surfaces: **CLI is the foundation**; `egui` GUI and `ratatui` TUI
  are thin shells over it
- Chat UI: **static SPA in browser private mode**, with CLI/TUI fallback
- Drive format: **exFAT**
- Model layer: **in-repo catalog, forkable, three sources, SHA256 integrity**
- Catalog format: **JSON, schema-versioned (`usbuddy.catalog/v1`), flat
  entries with `family_id`, named prompt templates, `capabilities` array,
  `aliases` for renames, single `stable` channel**
- **Content profile taxonomy** replaces "uncensored" branding (capability
  fully preserved)
- Gated models: **HF token primary + manual walkthrough + drop-in fallback**
- License handling: **three-tier (managed / drop-in / opt-out)** with
  per-model checkbox + "View license" UX in the picker and auditable
  opt-out config
- RAM-fit advisor: **green / yellow / red bands** against detected available
  RAM, **context-length slider** in picker, **idle unload default-on at 5
  min** (toggleable)
- Mid-session model swap: **kill-and-restart with message-level state
  replay**; force "start fresh" on `instruct` ↔ `base` profile-boundary swaps
- Code signing: **out of scope; documented unblock posture for macOS/Windows**
- CI/CD: **`ci.yml` + `release.yml` (`workflow_dispatch`), matrix above, draft
  releases, SBOM + SLSA provenance, workflow auto-tags**

### Open (implementation work, not architecture)
- Empirical tuning of RAM-fit threshold constants on real hardware
- Authoring `docs/ARCHITECTURE.md`, `docs/CATALOG-SPEC.md`, `docs/FOOTPRINT.md`
- Deferred workflows: `footprint.yml`, `catalog-validate.yml`, E2E install test

## License

Apache-2.0. See [`LICENSE`](./LICENSE) and [`NOTICE`](./NOTICE).
