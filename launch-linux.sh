#!/bin/sh
# USBuddy launcher for Linux.
# Reads current.json to find the active runtime version and executes it.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CURRENT="$SCRIPT_DIR/current.json"

if [ ! -f "$CURRENT" ]; then
    echo "USBuddy: current.json not found at $SCRIPT_DIR" >&2
    echo "Re-run the installer to set up this drive." >&2
    exit 1
fi

ACTIVE=$(python3 -c "import json,sys; print(json.load(open('$CURRENT'))['active'])" 2>/dev/null \
    || python -c "import json,sys; print(json.load(open('$CURRENT'))['active'])")

if [ -z "$ACTIVE" ]; then
    echo "USBuddy: could not read active version from current.json" >&2
    exit 1
fi

case "$(uname -m)" in
    x86_64|amd64) ARCH=x64 ;;
    aarch64|arm64) ARCH=arm64 ;;
    *) ARCH="$(uname -m)" ;;
esac

BIN_DIR="$SCRIPT_DIR/versions/$ACTIVE/bin/linux-$ARCH"
RUNTIME="$BIN_DIR/usbuddy-runtime"
ENGINE="$BIN_DIR/llama-server"

if [ ! -f "$RUNTIME" ]; then
    echo "USBuddy: runtime binary not found at $RUNTIME" >&2
    echo "Re-run the installer with: usbuddy-installer-cli install-runtime \"$SCRIPT_DIR\"" >&2
    exit 1
fi

if [ ! -f "$ENGINE" ]; then
    echo "USBuddy: llama-server engine not found at $ENGINE" >&2
    echo "Provision engines with: usbuddy-installer-cli engine install \"$SCRIPT_DIR\" all" >&2
    exit 1
fi

# exFAT and some FUSE mounts come up without the +x bit. Re-set it if we can.
chmod +x "$RUNTIME" "$ENGINE" 2>/dev/null || true
# Linker needs to find sibling .so files dropped alongside llama-server.
export LD_LIBRARY_PATH="$BIN_DIR:${LD_LIBRARY_PATH:-}"

exec "$RUNTIME" serve --drive "$SCRIPT_DIR" --open-browser "$@"
