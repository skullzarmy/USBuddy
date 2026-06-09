# USBuddy Architecture

## Scope boundary for v0.1.0

USBuddy v0.1.0 ships the CLI-first foundation, the portable runtime wrapper, the browser chat surface, and the core data formats required to create, inspect, and evolve an install on removable media. Native installer GUI/TUI parity, automated footprint regression, and hardware-backed RAM threshold tuning remain explicitly deferred until the first working release proves out the core lifecycle.

## Two-program model

USBuddy is intentionally split into two executables with opposite constraints:

- **Installer**: online-capable, may require elevation, formats removable media, downloads verified assets, writes the USB layout, and manages upgrades and rollback.
- **Runtime**: launched from the drive, runs as an unprivileged user, defaults to localhost-only behavior, keeps session state in memory, and treats the host as disposable.

Both programs are built on the same Rust core and expose the same operations through a CLI-first design. Any later GUI or TUI surfaces stay thin and call into the core instead of reimplementing logic.

## Workspace layout

- `crates/usbuddy-core`: shared types and operations.
- `crates/usbuddy-installer-cli`: CLI entrypoint for install, inspection, validation, and rollback workflows.
- `crates/usbuddy-runtime`: localhost-only runtime wrapper and static SPA host.
- `ui/web`: static browser UI bundled with runtime releases.
- `schemas/`: JSON schema definitions for catalogs and release metadata.
- `fixtures/`: sample catalogs and layout state used by tests.
- `examples/`: sample `current.json`, `version.json`, and catalog snapshots.

## Shared core responsibilities

The shared core owns:

1. Catalog parsing and compatibility validation.
2. Release manifest parsing and version comparison.
3. SHA256 hashing and verification.
4. Atomic file writes for pointer state such as `current.json`.
5. USB layout path resolution and shared directory creation.
6. License preference and acceptance record persistence.
7. Platform and architecture reporting.
8. RAM-fit estimation using detected or supplied available memory.

This layer stays UI-agnostic and can be called by CLI, GUI, TUI, or runtime services.

## Canonical USB layout

The installer and runtime both treat the following paths as authoritative:

- `current.json`: active and previous runtime pointers.
- `versions/{version}/`: immutable runtime trees.
- `models/`: shared model storage and local model provenance metadata.
- `.usbuddy/`: user-managed state such as trust, token, advisory dismissal, and license preferences.
- `catalog.json`: cached catalog snapshot.

The core layout manager creates these directories, validates their presence, and reads or writes schema-versioned metadata files atomically.

## Lifecycle

### Install

1. Detect or receive a removable drive root.
2. Confirm formatting requirements and existing-install posture.
3. Create the shared USB layout.
4. Stage a runtime version under `versions/{version}`.
5. Atomically write `current.json` to activate the installed version.

### Runtime start

1. Read `current.json` and resolve the active runtime tree.
2. Load and validate `catalog.json` if present.
3. Discover drop-in `.gguf` files under `models/`.
4. Evaluate RAM-fit against the chosen model and context settings.
5. Start the localhost-only wrapper and expose the chat surface.

### Upgrade

1. Parse a release manifest.
2. Stage the new version under `versions/{new}.tmp`.
3. Verify hashes.
4. Rename to `versions/{new}`.
5. Atomically swap `current.json` to set `active={new}` and `previous={old}`.

### Rollback

Rollback is a single atomic write that swaps `active` and `previous` in `current.json`. Shared user state and models are untouched.

## Failure handling

- **Unknown schema**: fail closed with a clear upgrade message.
- **Missing runtime tree**: keep the install readable but refuse activation.
- **Interrupted staged update**: leave `.tmp` state inert; operators can activate or discard later.
- **Low RAM / red band**: runtime refuses model launch and suggests a smaller configuration.
- **Drive removal during runtime**: runtime must avoid writes to the drive during sessions so sudden disappearance cannot corrupt state.

## Security posture

- Network behavior is explicit and user-initiated.
- Runtime binds only to localhost.
- Hash verification is mandatory for managed downloads.
- Licenses are accepted visibly and persisted with the license hash that was reviewed.
- “Zero footprint” is implemented as **no intentional persistence**, not as a guarantee that the host OS leaves no incidental traces.
