# USBuddy Footprint Posture

USBuddy targets **no intentional persistence** on the host. That is not the same as promising a perfectly trace-free host after use.

## What USBuddy actively avoids

- Background services or daemons.
- Writes to system install directories.
- Persistent runtime state on the host.
- Network listeners beyond localhost.
- USB writes during active runtime sessions except for explicit user management actions.

## Known residual traces

### Windows

- SmartScreen and Defender reputation checks.
- Prefetch and recent-execution metadata.
- Browser history if private mode is not honored by the user agent.
- Pagefile writes if the host ignores memory locking or a model exceeds safe RAM limits.

### macOS

- Gatekeeper quarantine handling before binaries are unblocked.
- Unified log entries for process launch and networking.
- Spotlight indexing of removable media if not disabled by the host.
- Browser state if the chosen browser does not honor private-mode launch requests.

### Linux

- Shell history if commands are launched manually.
- Desktop environment recent-file or launcher metadata.
- Journald and audit logs.
- Swap if the host is under memory pressure and refuses memory locking.

## Practical mitigations

- Refuse red-band RAM launches.
- Prefer localhost-only runtime exposure.
- Keep conversations in memory only.
- Stage and activate updates atomically.
- Publish residual-trace expectations so users can make informed tradeoffs.

## Out of scope for v0.1.0

- Automated host snapshot-diff verification.
- Formal forensic guarantees.
- Browser-specific private-mode enforcement beyond best-effort launching.
