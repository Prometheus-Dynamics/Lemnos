#!/usr/bin/env bash
# The check every PR into next must pass. CI runs exactly this.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

step() {
  local label="$1" start
  shift
  echo "==> $label"
  start=$SECONDS
  "$@"
  echo "    $label: $((SECONDS - start)) s"
}

# Cargo.lock is tracked. A dependency change that did not update it fails here, before any
# other cargo command could rewrite it: --locked refuses to change the lock.
lockfile() {
  if ! cargo metadata --locked --format-version 1 >/dev/null; then
    echo "Cargo.lock is out of date with the manifests. Run: cargo metadata --format-version 1 >/dev/null" >&2
    echo "then commit Cargo.lock with the change (docs/building.md, \"The lock file\")." >&2
    return 1
  fi
}
step lockfile lockfile

step format cargo fmt --all --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step tests cargo test --workspace

# What must still build without std, on the targets a microcontroller host uses.
no_std_crates=(lemnos-abi lemnos-hal lemnos-core lemnos-contract-accelerometer lemnos-contract-gyroscope lemnos-contract-magnetometer lemnos-contract-orientation lemnos-driver-orientation lemnos-host-static lemnos-mcu lemnos-driver-usb-gadget shm-sample-time)
for target in thumbv7em-none-eabihf riscv32imac-unknown-none-elf armv7-unknown-linux-gnueabihf; do
  for crate in "${no_std_crates[@]}"; do
    step "$crate on $target" cargo check -q -p "$crate" --no-default-features --target "$target"
  done
done

step "counters on" cargo test -q -p lemnos-measure --features perf,alloc-count
step "client on tokio" cargo clippy -q -p lemnos-client --all-targets --features tokio -- -D warnings
step "client on tokio" cargo test -q -p lemnos-client --features tokio
step "C surface, shared form" cargo test -q -p lemnos-capi --features shared

# The crates used as plain libraries, alone and without their driver halves.
step libraries scripts/check-libraries.sh
step boundaries scripts/check-boundaries.sh
# The unit, udev and sysusers files; nothing is installed here.
step packaging scripts/check-packaging.sh
# Every command in the documents runs against the simulated board, and every link leads somewhere.
step documents scripts/check-docs.py

echo "gate passed"
