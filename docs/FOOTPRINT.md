# Host footprint

USBuddy's design goal is to leave no intentional persistence on a host
machine. This document specifies the boundaries of that goal: what the
runtime and installer are guaranteed not to do, and what the host
operating system records about the session regardless.

## Scope of the guarantee

The guarantee USBuddy makes is no intentional persistence on the host.
That is bounded by what the implementation controls. It does not
extend to artifacts produced by the operating system itself.

In concrete terms:

- The runtime does not install services, register login items, or
  modify system configuration on any platform.
- The runtime writes only to the USB drive, and only while idle —
  never during an active chat session.
- The runtime binds only to `127.0.0.1:8765` (the HTTP server) and
  spawns `llama-server` on `127.0.0.1:8766`. Both processes terminate
  on quit.
- No network requests are initiated unless the user explicitly
  triggers one. The runtime does not perform telemetry, update
  polling, or background catalog refreshes.
- The installer writes to the USB drive and the cache directories
  expected of a normal desktop application on its host. It does not
  modify system paths.

The guarantee does not extend to:

- Operating-system logs of process execution, library load, or
  socket activity.
- Filesystem and execution caches maintained by the host
  (Prefetch, AmCache, ShimCache, Spotlight, journald, audit).
- Network requests initiated by the host on USBuddy's behalf
  (Gatekeeper assessment, SmartScreen reputation lookups, Defender
  cloud submissions, browser history sync, OCSP / CRL checks).
- Memory pressure outcomes (swap or pagefile writes) on hosts that
  refuse `mlock` or that the RAM-fit advisor underestimates.

## Per-platform host artifacts

The following artifacts are produced by the host operating system
during a USBuddy session. They are not under USBuddy's control.

### macOS

- **Gatekeeper assessment**. First execution of each unsigned binary
  triggers a Gatekeeper dialog and a network assessment. The user's
  approval is persisted per-binary in the launch services database.
- **Unified log**. `log show --predicate 'process == "usbuddy-runtime"'`
  returns process start, network bind, signal delivery, and
  termination events.
- **Spotlight metadata**. When indexing of removable media is enabled
  (the default), `mdimporter` indexes the drive's contents while
  mounted and stores metadata about which drives have been seen on
  the host.
- **Browser history**. The default browser is invoked via `open
  <url>` and records the URL in its history database unless the user
  has configured exclusions or private browsing.
- **launchservicesd cache**. Records the path and signature of every
  executed binary.

### Windows

- **SmartScreen reputation lookup**. First execution of an unsigned
  binary issues a network request to Microsoft's reputation service.
  The user's "Run anyway" approval is cached locally.
- **Microsoft Defender real-time scan**. Scans the runtime binary
  and `llama-server` on first execution. Per the user's privacy
  settings, file hashes and metadata may be submitted to Defender
  cloud.
- **Prefetch**. `C:\Windows\Prefetch\` records executable name,
  path, and launch timestamps.
- **AmCache and ShimCache**. Persist the path, size, and
  compatibility shim status of every executed binary.
- **Event Log**. `Application` and `System` channels record process
  creation if auditing is enabled.
- **Pagefile**. A model loaded under unexpected memory pressure may
  produce writes to `pagefile.sys`. The RAM-fit advisor blocks
  red-band launches to prevent this; yellow-band launches under
  unexpected pressure remain possible.

### Linux

- **journald / syslog**. Records process start and termination
  per the host's logging configuration.
- **auditd**. When `execve` auditing is enabled, the runtime's
  execution is recorded under `/var/log/audit/`.
- **Shell history**. Commands launched manually are recorded in
  the user's shell history file.
- **Desktop launcher and recent-files caches**. Most desktop
  environments record opening removable-media items in
  `~/.local/share/recently-used.xbel` or equivalent.
- **Swap**. As with Windows pagefile, a model under unexpected
  memory pressure may write to swap on a host that ignores
  `mlock`. The RAM-fit advisor blocks red-band launches.

## Mitigations USBuddy implements

The following are implemented in `usbuddy-core` and the runtime to
limit footprint within the bounds the implementation controls.

- **RAM-fit advisor**. Models that do not fit comfortably in
  available memory are refused. The advisor parses the GGUF header
  to compute the model's actual KV-cache size per token. Red-band
  launches do not proceed.
- **Idle unload**. After 5 minutes of chat inactivity, the runtime
  sends `SIGTERM` to `llama-server`, releasing mlocked weights.
  Reload occurs transparently on the next message. Threshold is
  configurable via `--idle-timeout-secs`; `0` disables.
- **Chat memory defaults to off**. Conversations are held in RAM
  only unless the user explicitly enables persistence. When
  enabled, the warning dialog states that the drive becomes the
  artifact at that point.
- **Atomic drive writes**. All writes to drive state pass through
  `usbuddy_core::atomic`. A mid-write yank leaves the previous
  version active.
- **No background network operations**. Catalog refreshes and
  update checks require explicit user action.

## Verification

The Linux footprint job at
[`.github/workflows/footprint.yml`](../.github/workflows/footprint.yml)
runs on every pull request that touches the runtime. The job boots
the runtime against a scratch drive directory inside a Linux container,
snapshots `$HOME` and `/tmp` before and after the session, and uploads
the diff as a CI artifact. Any writes the runtime makes outside the
drive appear in the diff.

Comparable Windows Sandbox and macOS snapshot jobs are deferred until
suitable container primitives are available on those platforms.

The same verification can be reproduced manually: take a baseline of
the directories of interest before connecting the drive, complete a
session, take a second snapshot, and diff.

## Out of scope

- Forensic guarantees. USBuddy does not implement countermeasures
  against post-eject forensic analysis of the host.
- Browser private-mode enforcement. The runtime requests the host
  to open a URL; it cannot reliably force the resulting browser to
  use private mode without per-browser invocation hacks that
  break the default-browser model.
- Enterprise endpoint detection and response systems. USBuddy
  does not attempt to evade EDR or corporate monitoring.
