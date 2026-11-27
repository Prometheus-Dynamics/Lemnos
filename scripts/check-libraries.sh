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
  lemnos-driver-rk-mpp lemnos-driver-rkvenc lemnos-driver-rkjpege
  lemnos-driver-rkisp lemnos-driver-rkisp-sys
  lemnos-driver-k230isp lemnos-driver-k230isp-sys
  lemnos-driver-isp600 lemnos-driver-isp600-sys
  lemnos-linux-hotplug
  lemnos-identity
  lemnos-wire lemnos-linux-stream
  # The shared-memory ring family (docs/ring.md): standalone, so no Lemnos crate at all.
  shm-sample-time shm-ring shm-ring-latest shm-ring-time shm-ring-doorbell shm-ring-buffers
  shm-ring-holds shm-fdpass
)

# The daemon, the host, the driver model and its macros, the contracts, the client and the
# backends that carry the driver model.
forbidden='^(lemnosd|lemnos-ctl|lemnos-orion|lemnos-host|lemnos-host-static|lemnos-core|lemnos-macros|lemnos-hal|lemnos-client|lemnos-linux|lemnos-usb|lemnos-sim|lemnos-contract-.*)$'

# The shm crates (shm/README.md, "What it gives"): their direct normal dependencies are libc, serde
# (optional) and each other, nothing else, so another project can take them at a locked revision.
# Direct edges only: each crate answers for the ones it names.
shm_direct='^(libc|serde|shm-[a-z-]+)$'
for crate in "${libraries[@]}"; do
  [[ $crate == shm-* ]] || continue
  # cargo tree on its own line, so its failure stops the check; only the filter may find nothing.
  tree=$(cargo tree -q -e normal --depth 1 -p "$crate" --no-default-features --prefix none --format '{p}')
  grep -q "^$crate " <<<"$tree" || { echo "check-libraries: cargo tree did not list $crate itself" >&2; exit 1; }
  direct=$(awk '{print $1}' <<<"$tree" | { grep -v "^$crate\$" || true; } | sort -u)
  outside=$(grep -Ev "$shm_direct" <<<"$direct" || true)
  if [ -n "$outside" ]; then
    echo "$crate has a direct normal dependency outside libc, serde and the shm crates:" $outside >&2
    exit 1
  fi
done
echo "    shm direct dependencies: libc, serde and shm crates only"

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
