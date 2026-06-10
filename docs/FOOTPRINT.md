# Footprint

This document is the honest accounting of what USBuddy leaves on a host
machine after you unplug. Read it before you trust the privacy claims.

## What "no footprint" actually means

USBuddy's guarantee is **no intentional persistence**. The runtime
doesn't install services, doesn't write to system directories, doesn't
register a login item, doesn't drop config in your home directory,
doesn't keep network listeners open beyond `localhost:8765` during the
session, and doesn't write to the USB drive while a chat is in
progress. When you click Quit, every process USBuddy started exits and
every resource it held is released.

That guarantee is what we can enforce in our own code. What we **can't**
enforce is the host operating system itself. Modern OSes log process
launches, index removable media, cache executable hashes for reputation
checks, and write swap when memory pressure forces them to. Some of
those traces persist after you eject the drive. We can't prevent them
from inside a USB application; the most we can do is minimize what we
produce and tell you exactly what gets left behind.

The honest framing is:

> USBuddy promises no intentional persistence. It does not promise a
> perfectly trace-free host. Here is what's left.

## What USBuddy actively avoids

- No system service or daemon registration on any platform.
- No writes to system install paths.
- No writes outside the USB drive at any time.
- No network listeners except `127.0.0.1:8765` (the runtime HTTP
  server) and `127.0.0.1:8766` (`llama-server`, spawned by the
  runtime). Both die when the runtime quits.
- No telemetry. No phone-home. No update checks unless the user
  explicitly clicks one.
- No writes to the USB drive during an active chat session. Chat
  buffers live in RAM until the user enables persistence in the UI.

## What the host operating system records

These traces are produced by the host, not by USBuddy, and we cannot
suppress them from inside the application.

### macOS

- **Gatekeeper quarantine**: until the user runs an unsigned binary
  once (or strips `com.apple.quarantine`), each launch triggers a
  Gatekeeper dialog. The dialog itself doesn't persist; the user's
  approval does, scoped to that binary on that user account.
- **Unified log**: every process launch, network bind, and signal is
  written to the unified log. `log show --predicate 'process ==
  "usbuddy-runtime"'` reproduces this.
- **Spotlight**: if Spotlight indexing of removable media is enabled
  (default on most setups), `mdimporter` will index the drive's
  contents while it's connected. The Spotlight index is on the drive
  itself, but Spotlight metadata about *which drives have been
  connected* lives on the host.
- **Recent items / browser history**: opening the chat URL in your
  default browser will leave it in browser history unless you've
  configured the browser to ignore localhost or used private
  browsing. USBuddy currently asks the OS to open the URL via `open
  <url>`; it does not request private mode.

### Windows

- **SmartScreen reputation check**: the first time you launch an
  unsigned binary, SmartScreen consults Microsoft's reputation service
  over the network. The check itself is a network call from the host;
  the "Run anyway" decision is cached locally.
- **Defender real-time scan**: scans the runtime binary and
  `llama-server` on first execution. Hashes are reported to Defender
  cloud per the user's privacy settings.
- **Prefetch**: `C:\Windows\Prefetch\` records executable launches by
  filename and timestamp.
- **AmCache / ShimCache**: catalog of every executable that's been
  launched.
- **Pagefile**: if a model exceeds the host's available RAM and the
  host ignores or refuses memory locking, weights can spill to
  `pagefile.sys`. USBuddy's RAM-fit advisor refuses to load models
  that don't comfortably fit precisely to prevent this. Pagefile
  writes from refused launches are zero. Pagefile writes from a
  yellow-band launch under unexpected memory pressure are possible.

### Linux

- **Journald / syslog**: process launches and network binds are
  recorded per the host's logging configuration.
- **Shell history**: if the user launched the runtime from a shell
  rather than the `.sh` launcher, the command line lives in
  `~/.bash_history` (or equivalent).
- **Recent files / desktop launcher cache**: most desktop
  environments record recently-opened items including files on
  removable media.
- **Swap**: same dynamic as Windows pagefile. The RAM-fit advisor
  blocks red-band launches; yellow-band launches under unexpected
  pressure can swap.
- **`/var/log/audit/`**: if `auditd` is configured to log execve, the
  runtime's launch is recorded there.

## What USBuddy does to minimize this

- **RAM-fit advisor blocks red-band launches.** A model that would
  spill to swap is refused, not warned. The advisor uses the model's
  real KV-cache shape parsed from the GGUF header — no fudge
  constants.
- **Idle-unload defaults to 5 minutes.** After idle, the runtime
  `SIGTERM`s `llama-server` so model weights leave mlocked RAM. The
  next message reloads them. This shortens the window in which
  weights are pageable.
- **Chat memory defaults to off.** Conversations live in RAM only
  unless the user explicitly enables persistence. When persistence is
  enabled, the warning dialog states that the stick becomes the
  artifact.
- **All drive writes are atomic.** A yank mid-update can't corrupt
  the drive — at worst, the previous version stays active.
- **No background catalog refresh.** Updates and catalog refreshes
  require an explicit user action.

## How to verify this yourself

The Linux footprint job in
[`.github/workflows/footprint.yml`](../.github/workflows/footprint.yml)
runs on every PR that touches the runtime. It boots the runtime against
a scratch drive directory inside a Linux container, snapshots `$HOME`
and `/tmp` before and after, and uploads the diff as a CI artifact.
Anything the runtime writes outside the drive shows up there.

The Windows Sandbox and macOS snapshot-diff equivalents are tracked
follow-ups, deferred for v0.1.0. The reason isn't difficulty — it's that
neither has a clean container model the way Linux does.

You can also run the snapshot diff yourself: before plugging the
drive, take a baseline of whatever paths you care about (browser
history, prefetch, unified log, journald). Use the drive. Take a second
snapshot. Diff. Anything you see is on this page, or it's a bug —
file an issue.

## Out of scope

- **Forensic guarantees.** USBuddy is not a forensic-grade tool. If
  your threat model includes a forensic examiner with disk access to
  the host post-eject, this is not the right product.
- **Browser private-mode enforcement.** The runtime asks the OS to
  open a URL. It cannot force a browser to use private mode without
  per-browser plugins or command-line invocation hacks that defeat
  the "just works" property of `open <url>`.
- **Defeating EDR / corporate monitoring.** Enterprise endpoint tools
  see everything. USBuddy doesn't try to hide from them.
