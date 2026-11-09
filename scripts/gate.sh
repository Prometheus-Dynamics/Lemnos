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

step format cargo fmt --all --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step tests cargo test --workspace

# What must still build without std, on the targets a microcontroller host uses.
no_std_crates=(lemnos-abi lemnos-hal lemnos-core lemnos-contract-accelerometer lemnos-contract-gyroscope lemnos-contract-magnetometer lemnos-contract-orientation lemnos-driver-orientation)
for target in thumbv7em-none-eabihf riscv32imac-unknown-none-elf armv7-unknown-linux-gnueabihf; do
  for crate in "${no_std_crates[@]}"; do
    step "$crate on $target" cargo check -q -p "$crate" --no-default-features --target "$target"
  done
done

step "counters on" cargo test -q -p lemnos-measure --features perf,alloc-count
step "client on tokio" cargo clippy -q -p lemnos-client --all-targets --features tokio -- -D warnings
step "client on tokio" cargo test -q -p lemnos-client --features tokio
step "C surface, shared form" cargo test -q -p lemnos-capi --features shared

step boundaries scripts/check-boundaries.sh

echo "gate passed"
