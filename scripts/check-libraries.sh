#!/usr/bin/env bash
# The crates other programs take as plain libraries, with no daemon, no host and no driver
# runtime (docs/drivers.md, "Used as libraries"). Each is built, linted and tested alone with its
# default features off, which leaves the driver half out, and its normal dependency tree must
# hold no part of the engine.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

libraries=(
  lemnos-v4l2
  lemnos-driver-pisp lemnos-driver-pisp-sys lemnos-driver-pisp-fe-sys
  lemnos-driver-rkisp lemnos-driver-rkisp-sys
  lemnos-driver-k230isp lemnos-driver-k230isp-sys
  lemnos-driver-isp600 lemnos-driver-isp600-sys
  lemnos-linux-hotplug
  lemnos-wire lemnos-linux-stream
  # The shared-memory ring family (docs/ring.md): standalone, so no Lemnos crate at all.
  shm-sample-time shm-ring shm-ring-latest shm-ring-time shm-ring-doorbell shm-ring-buffers
  shm-ring-holds shm-fdpass
)

# The daemon, the host, the driver model and its macros, the contracts, the client and the
# backends that carry the driver model.
forbidden='^(lemnosd|lemnos-ctl|lemnos-orion|lemnos-host|lemnos-host-static|lemnos-core|lemnos-macros|lemnos-hal|lemnos-client|lemnos-linux|lemnos-usb|lemnos-sim|lemnos-contract-.*)$'

for crate in "${libraries[@]}"; do
  tree=$(cargo tree -q -e normal -p "$crate" --no-default-features --prefix none --format '{p}' |
    awk '{print $1}' | sort -u)
  pattern=$forbidden
  [[ $crate == shm-* ]] && pattern='^lemnos'
  pulled=$(grep -E "$pattern" <<<"$tree" || true)
  if [ -n "$pulled" ]; then
    echo "$crate with no default features pulls in:" $pulled >&2
    exit 1
  fi
  echo "    $crate:" $(grep -v "^$crate\$" <<<"$tree" | tr '\n' ' ')
  cargo clippy -q -p "$crate" --no-default-features --all-targets -- -D warnings
  cargo test -q -p "$crate" --no-default-features
done
