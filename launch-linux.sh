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

RUNTIME="$SCRIPT_DIR/versions/$ACTIVE/bin/linux-x64/usbuddy-runtime"

if [ ! -f "$RUNTIME" ]; then
    echo "USBuddy: runtime binary not found at $RUNTIME" >&2
    exit 1
fi

exec "$RUNTIME" serve --drive "$SCRIPT_DIR" --open-browser "$@"
