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
no_std_crates=(lemnos-abi lemnos-hal lemnos-core lemnos-contract-accelerometer lemnos-contract-gyroscope lemnos-contract-magnetometer lemnos-contract-orientation lemnos-driver-orientation lemnos-host-static lemnos-mcu lemnos-driver-usb-gadget lemnos-driver-generic-sensor shm-sample-time)
for target in thumbv7em-none-eabihf riscv32imac-unknown-none-elf armv7-unknown-linux-gnueabihf; do
  for crate in "${no_std_crates[@]}"; do
    step "$crate on $target" cargo check -q -p "$crate" --no-default-features --target "$target"
  done
done

# What must build on the desktop targets: the crates meant to be portable, checked for Windows and
# macOS. A crate listed here that stops building there fails the gate, and the step names it.
# Linux-only crates stay off this list.
portable_crates=(shm-ring shm-ring-time shm-sample-time)
for target in x86_64-pc-windows-msvc aarch64-apple-darwin x86_64-apple-darwin; do
  for crate in "${portable_crates[@]}"; do
    step "$crate on $target" cargo check -q -p "$crate" --target "$target"
  done
done

step "counters on" cargo test -q -p lemnos-measure --features perf,alloc-count
# A setting's meaning never depends on a feature: the same board text runs the same tests with and
# without `std` (the generic sensors' `description_file`, the gadget's `interfaces_file`).
step "generic sensor, no std" cargo test -q -p lemnos-driver-generic-sensor
step "generic sensor, std" cargo test -q -p lemnos-driver-generic-sensor --features std
step "usb gadget, no std" cargo test -q -p lemnos-driver-usb-gadget
step "usb gadget, std" cargo test -q -p lemnos-driver-usb-gadget --features std
step "client on tokio" cargo clippy -q -p lemnos-client --all-targets --features tokio -- -D warnings
step "client on tokio" cargo test -q -p lemnos-client --features tokio
step "C surface, shared form" cargo test -q -p lemnos-capi --features shared

# The crates used as plain libraries, alone and without their driver halves.
step libraries scripts/check-libraries.sh
step boundaries scripts/check-boundaries.sh
# The bridge and Orion share one copy of each shm-* crate (the root Cargo.toml's patch of the Orion source).
step orion-copies scripts/check-orion-copies.sh
# The unit, udev and sysusers files; nothing is installed here.
step packaging scripts/check-packaging.sh
# Every file under packaging/ is listed in packaging/files.txt (installed, or marked never), and the list is well formed.
step packaging-files scripts/check-packaging-files.py
# The shipped programs (packaging/shipped.txt) pull in no C: no C compiler crate, no foreign -sys crate.
# Also checks that docs/building.md lists each of them.
step shipped scripts/check-shipped.sh
# The module's CONFIG_ symbols are all in docs/kernel-config.md, and the QEMU harness enables the required ones.
step kernel-config scripts/check-kernel-config.py
# The generated overlays compile with the dtc the contract names (built from its pinned release) with
# every warning class an error, and apply to a base built from their SoC map (testing/devtree/check.sh).
step devtree testing/devtree/check.sh
# Every command in the documents runs against the simulated board, and every link leads somewhere.
step documents scripts/check-docs.py
# The mdev.conf that `lemnos-ctl tree --mdev` writes is run by BusyBox 1.37.0 and 1.38.0 (their
# mdev, built from the pinned release tarballs), in a namespace, and each node's owner, group and mode
# are checked against the udev rule for the same device. CI builds them first (BUSYBOX_DIR, cached).
step mdev testing/mdev/check.sh

echo "gate passed"
