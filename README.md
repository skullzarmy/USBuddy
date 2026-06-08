# USBuddy

> A private, safe, portable LLM that lives on your USB drive.

## What it is

USBuddy is a zero-install, portable local AI environment: a high-quality
(optionally uncensored) LLM that runs **fully offline** directly from a USB
drive or external SSD. Plug it into any machine, run the AI, unplug, and walk
away. The application code and model parameters all live on the drive.

> Inspired by the *concept* behind
> [USB-Uncensored-LLM](https://github.com/techjarves/USB-Uncensored-LLM).
> USBuddy is an independent, clean-room build — no code is carried over.

---

## Requirements

The project's target requirements. **Architecture and implementation choices
are intentionally left open and will be decided later.**

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

## Status

- Repo scaffolded.
- License: **Apache-2.0** (see `LICENSE` and `NOTICE`).
- Architecture & implementation: **to be decided.**

## Open questions (to decide)

- **Installer UI stack** — Go / Wails / other. *Note: Wails is flagged for
  reconsideration — its Linux build depends on WebKitGTK being present on the
  host, which works against the zero-install goal.*
- **Inference engine** — TBD.
- **Exact footprint guarantees** achievable per OS for requirement C.
- **Uncensored model framing** — keep or not.

## License

Apache-2.0. See [`LICENSE`](./LICENSE) and [`NOTICE`](./NOTICE).
