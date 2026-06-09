#!/bin/sh
# USBuddy launcher for macOS.
# Double-click this file in Finder to start USBuddy.
# If blocked by Gatekeeper, right-click → Open the first time.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CURRENT="$SCRIPT_DIR/current.json"

if [ ! -f "$CURRENT" ]; then
    osascript -e 'display alert "USBuddy" message "current.json not found. Re-run the installer to set up this drive."'
    exit 1
fi

ACTIVE=$(python3 -c "import json; print(json.load(open('$CURRENT'))['active'])" 2>/dev/null)

if [ -z "$ACTIVE" ]; then
    osascript -e 'display alert "USBuddy" message "Could not read active version from current.json."'
    exit 1
fi

ARCH=$(uname -m)
if [ "$ARCH" = "arm64" ]; then
    BIN_DIR="macos-arm64"
else
    BIN_DIR="macos-x64"
fi

RUNTIME="$SCRIPT_DIR/versions/$ACTIVE/bin/$BIN_DIR/usbuddy-runtime"

# Fall back to the universal2 wrapper if per-arch binary absent.
if [ ! -f "$RUNTIME" ]; then
    RUNTIME="$SCRIPT_DIR/versions/$ACTIVE/bin/macos/usbuddy-runtime"
fi

if [ ! -f "$RUNTIME" ]; then
    osascript -e "display alert \"USBuddy\" message \"Runtime binary not found for version $ACTIVE.\""
    exit 1
fi

# Strip quarantine attribute so Gatekeeper does not re-block on each host.
xattr -d com.apple.quarantine "$RUNTIME" 2>/dev/null || true

exec "$RUNTIME" serve --drive "$SCRIPT_DIR" --open-browser "$@"
