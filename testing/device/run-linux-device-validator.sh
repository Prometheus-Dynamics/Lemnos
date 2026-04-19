#!/bin/sh
set -eu

BIN="${1:-./linux_device_validator}"
CONFIG="${2:-./helios-board-validator.env}"

if "$BIN" "$CONFIG"; then
    exit 0
fi

STATUS=$?
if [ "$STATUS" -eq 127 ] && [ -x /lib/ld-linux-aarch64.so.1 ]; then
    exec /lib/ld-linux-aarch64.so.1 "$BIN" "$CONFIG"
fi

exit "$STATUS"
