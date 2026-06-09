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

case "$(uname -m)" in
    arm64) ARCH=arm64 ;;
    x86_64) ARCH=x64 ;;
    *) ARCH="$(uname -m)" ;;
esac

BIN_DIR="$SCRIPT_DIR/versions/$ACTIVE/bin/macos-$ARCH"
RUNTIME="$BIN_DIR/usbuddy-runtime"
ENGINE="$BIN_DIR/llama-server"

# Fall back to the legacy single-arch wrapper if per-arch dir absent.
if [ ! -f "$RUNTIME" ]; then
    BIN_DIR="$SCRIPT_DIR/versions/$ACTIVE/bin/macos"
    RUNTIME="$BIN_DIR/usbuddy-runtime"
    ENGINE="$BIN_DIR/llama-server"
fi

if [ ! -f "$RUNTIME" ]; then
    osascript -e "display alert \"USBuddy\" message \"Runtime binary not found for version $ACTIVE ($(uname -m)). Run: usbuddy-installer-cli install-runtime '$SCRIPT_DIR'\""
    exit 1
fi

if [ ! -f "$ENGINE" ]; then
    osascript -e "display alert \"USBuddy\" message \"llama-server engine not found. Run: usbuddy-installer-cli engine install '$SCRIPT_DIR' all\""
    exit 1
fi

# Strip quarantine on every binary the runtime might exec, including sibling dylibs.
xattr -dr com.apple.quarantine "$BIN_DIR" 2>/dev/null || true
chmod +x "$RUNTIME" "$ENGINE" 2>/dev/null || true

exec "$RUNTIME" serve --drive "$SCRIPT_DIR" --open-browser "$@"
