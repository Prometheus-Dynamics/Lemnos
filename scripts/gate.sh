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

# GATE_PART picks a part of the gate, so CI can run the parts side by side (.github/workflows/next.yml):
#   unset or "all"   everything, in order (what to run locally)
#   "checks"         everything except the workspace tests
#   "tests:K/N"      the workspace tests of shard K of N
# The shards are balanced by measured test time (CI-SPLIT, 2026-11-17). Shard 1 holds lemnos-ctl,
# whose host-merge tests need the real dtmerge (CI builds it in that shard only), and lemnos-slot-init,
# the longest by far. The other long ones (`slow`, longest first) are dealt out over shards 2..N, then
# every other package, sorted by name. A package missing from these lists still lands in a shard: the
# lists only order them. A shard with no package in it fails: a part that tests nothing is never a pass.
part="${GATE_PART:-all}"
case "$part" in
  all | checks) ;;
  tests:[1-9]*/[1-9]*)
    k="${part#tests:}"; n="${k#*/}"; k="${k%/*}"
    if [ "$k" -gt "$n" ]; then echo "GATE_PART=$part: shard $k of $n does not exist" >&2; exit 1; fi
    first=(lemnos-ctl lemnos-slot-init)
    slow=(lemnos-slot-store lemnos-driver-system-slots lemnos-package lemnos-driver-system-flash lemnos-orion
      shm-ring-model lemnos-macros lemnos-driver-system-security shm-ring-doorbell lemnos-camera-proof lemnos-host
      lemnos-linux lemnos-client)
    mapfile -t packages < <(cargo metadata --locked --no-deps --format-version 1 |
      python3 -c 'import json,sys; print("\n".join(sorted(p["name"] for p in json.load(sys.stdin)["packages"])))')
    declare -A known=()
    for pkg in "${packages[@]}"; do known[$pkg]=1; done
    ordered=()
    for pkg in "${slow[@]}"; do [ -n "${known[$pkg]:-}" ] && ordered+=("$pkg"); done
    for pkg in "${packages[@]}"; do
      case " ${first[*]} ${slow[*]} " in *" $pkg "*) ;; *) ordered+=("$pkg") ;; esac
    done
    shard=()
    if [ "$k" -eq 1 ]; then
      for pkg in "${first[@]}"; do [ -n "${known[$pkg]:-}" ] && shard+=(-p "$pkg"); done
    fi
    rest=$((n > 1 ? n - 1 : 1)); i=0
    for pkg in "${ordered[@]}"; do
      want=$((n > 1 ? i % rest + 2 : 1))
      [ "$want" -eq "$k" ] && shard+=(-p "$pkg")
      i=$((i + 1))
    done
    if [ "${#shard[@]}" -eq 0 ]; then echo "GATE_PART=$part: the shard has no package" >&2; exit 1; fi
    echo "shard $k of $n: $((${#shard[@]} / 2)) of ${#packages[@]} packages"
    step "tests, shard $k of $n" cargo test "${shard[@]}"
    echo "gate part $part passed"
    exit 0
    ;;
  *) echo "GATE_PART=$part: expected all, checks or tests:K/N" >&2; exit 1 ;;
esac

# The steps run in three lanes at once. The cargo lane holds the lint, the workspace tests, and the
# steps after them that build the programs the tests start (lemnosd, lemnos-ctl): cargo holds the
# target directory's lock for the whole of each build, so those steps cannot overlap, and they run in
# the order below. The side lane holds the steps that do not build: the U-Boot check (QEMU), the
# packaging and kernel checks, and the cargo queries that read only the lock file. The tail lane
# holds the cargo steps that touch nothing the workspace tests start or read: the doc tests, the
# builds for other targets, the shipped programs (their own profile), the library crates and the
# feature sets the workspace tests do not build (library tests and checks only, no programs). It
# starts once the workspace tests are built and running, when cargo's lock is free, and runs beside
# them. Each step's output is kept whole and printed under its "==> label" line when it finishes,
# with its "label: N s" line after it. A failing step's output is shown, the other lanes are
# stopped, and the gate fails.
gate_out="$(mktemp -d "${TMPDIR:-/tmp}/gate-out.XXXXXX")"
declare -A lane_pid=([c]=0 [s]=0 [t]=0)
stop_lanes() {
  # Only the lanes this shell started, and their children: each lane is its own process group.
  local lane
  for lane in c s t; do
    [ "${lane_pid[$lane]}" = 0 ] || kill -TERM -- "-${lane_pid[$lane]}" 2>/dev/null || true
  done
}
trap 'stop_lanes; rm -rf -- "$gate_out"' EXIT

# unit <key> <label> <command...>: runs one step, its output into $gate_out/<key>.out (the step
# can read its own output file as $unit_out). The .rc file is written last: a step is finished when
# it exists.
unit() {
  local key="$1" label="$2" start=$SECONDS rc=0
  shift 2
  echo "$label" >"$gate_out/$key.label"
  unit_out="$gate_out/$key.out"
  "$@" >"$unit_out" 2>&1 || rc=$?
  echo "$((SECONDS - start))" >"$gate_out/$key.secs"
  echo "$rc" >"$gate_out/$key.rc"
  return "$rc"
}

use_nextest=0
if [ "${GATE_TESTS:-nextest}" = nextest ] && cargo nextest --version >/dev/null 2>&1; then use_nextest=1; fi

cargo_lane() {
  local n=0
  lane_step() { n=$((n + 1)); unit "c$(printf %03d "$n")" "$@"; }
  lane_step clippy cargo clippy --workspace --all-targets -- -D warnings
  # The workspace tests. `cargo test` runs the test programs one after another, so most cores sit idle;
  # cargo-nextest runs every test in its own process, across all programs at once. It does not run doc
  # tests, so those follow. Without cargo-nextest (or with GATE_TESTS=cargo) the plain form runs: the
  # same tests, slower. GATE_TEST_JOBS caps how many tests run at once (default: every core this
  # process may use).
  # The tail lane waits for the tests to be built and running: nextest says "Starting N tests"
  # when its build is over and cargo's lock is free. tests.over is there once the step has ended,
  # whichever way, so the tail lane never waits on a build that failed.
  workspace_tests() {
    local rc=0 watcher
    if [ "$use_nextest" = 1 ]; then
      (until grep -qE '^ *Starting [0-9]+ tests' "$unit_out" 2>/dev/null; do sleep 0.2; done
        touch "$gate_out/tests.running") &
      watcher=$!
      cargo nextest run --workspace --no-fail-fast --test-threads "${GATE_TEST_JOBS:-$(nproc)}" || rc=$?
      kill "$watcher" 2>/dev/null || true
    else
      cargo test --workspace || rc=$?
    fi
    touch "$gate_out/tests.over"
    return "$rc"
  }
  [ "$part" = checks ] || lane_step tests workspace_tests

  lane_step "counters on" cargo test -q -p lemnos-measure --features perf,alloc-count
  # A setting's meaning never depends on a feature: the same board text runs the same tests with and
  # without `std` (the generic sensors' `description_file`, the gadget's `interfaces_file`).
  lane_step "generic sensor, no std" cargo test -q -p lemnos-driver-generic-sensor
  lane_step "generic sensor, std" cargo test -q -p lemnos-driver-generic-sensor --features std
  lane_step "usb gadget, alloc" cargo test -q -p lemnos-driver-usb-gadget
  lane_step "usb gadget, std" cargo test -q -p lemnos-driver-usb-gadget --features std
  lane_step "client on tokio" cargo clippy -q -p lemnos-client --all-targets --features tokio -- -D warnings
  lane_step "client on tokio" cargo test -q -p lemnos-client --features tokio
  lane_step "C surface, shared form" cargo test -q -p lemnos-capi --features shared
  # The vendor-header layouts: with no header given, the tool says per variant which header it needs and
  # compares nothing (the drivers' own layout assertions are compiled and tested above).
  lane_step "layout headers, not run in the gate" cargo run -q -p lemnos-layout-check

  # The generated overlays compile with the dtc the contract names (built from its pinned release) with
  # every warning class an error, and apply to a base built from their SoC map (testing/devtree/check.sh).
  lane_step devtree testing/devtree/check.sh
  # Every command in the documents runs against the simulated board, and every link leads somewhere.
  lane_step documents scripts/check-docs.py
  # The mdev.conf that `lemnos-ctl tree --mdev` writes is run by BusyBox 1.37.0 and 1.38.0 (their
  # mdev, built from the pinned release tarballs), in a namespace, and each node's owner, group and mode
  # are checked against the udev rule for the same device. CI builds them first (BUSYBOX_DIR, cached).
  lane_step mdev testing/mdev/check.sh
}

# The cargo steps that can run beside the workspace tests. None builds a program the tests start,
# and none writes where the tests' programs are: the doc tests build nothing new, the checks for other
# targets and the shipped programs have their own directories under the target, and the library
# crates have no programs. A step that builds lemnosd or lemnos-ctl for this machine is not one of
# them (the tests start those, and a build with other features would swap them mid-run): those stay
# in the cargo lane, after the tests.
tail_lane() {
  local n=0
  lane_step() { n=$((n + 1)); unit "t$(printf %03d "$n")" "$@"; }
  if [ "$part" != checks ]; then
    until [ -f "$gate_out/tests.running" ] || [ -f "$gate_out/tests.over" ]; do sleep 0.2; done
  fi
  # cargo-nextest does not run doc tests, so they run here. (The plain form runs them with the tests.)
  if [ "$part" != checks ] && [ "$use_nextest" = 1 ]; then
    lane_step "doc tests" cargo test --workspace --doc
  fi

  # What must still build without std, on the targets a microcontroller host uses.
  no_std_crates=(lemnos-abi lemnos-hal lemnos-core lemnos-contract-accelerometer lemnos-contract-gyroscope lemnos-contract-magnetometer lemnos-contract-orientation lemnos-driver-orientation lemnos-host-static lemnos-mcu lemnos-driver-usb-gadget lemnos-driver-generic-sensor shm-sample-time)
  for target in thumbv7em-none-eabihf riscv32imac-unknown-none-elf armv7-unknown-linux-gnueabihf; do
    for crate in "${no_std_crates[@]}"; do
      lane_step "$crate on $target" cargo check -q -p "$crate" --no-default-features --target "$target"
    done
  done

  # What must build on the desktop targets: the crates meant to be portable, checked for Windows and
  # macOS. A crate listed here that stops building there fails the gate, and the step names it.
  # Linux-only crates stay off this list.
  portable_crates=(shm-ring shm-ring-time shm-sample-time)
  for target in x86_64-pc-windows-msvc aarch64-apple-darwin x86_64-apple-darwin; do
    for crate in "${portable_crates[@]}"; do
      lane_step "$crate on $target" cargo check -q -p "$crate" --target "$target"
    done
  done

  # The crates used as plain libraries, alone and without their driver halves.
  lane_step libraries scripts/check-libraries.sh
  # The shipped programs (packaging/shipped.txt) agree: see scripts/check-products.sh.
  lane_step products scripts/check-products.sh
  # The sets the workspace tests never build: each crate's tests in the exact set it ships with,
  # and each library option (no defaults, all features) linted and built. See the script.
  lane_step features scripts/check-features.sh
}

side_lane() {
  local n=0
  lane_step() { n=$((n + 1)); unit "s$(printf %03d "$n")" "$@"; }
  lane_step format cargo fmt --all --check
  # The steps below are the gate's checks that run the same as before, in this lane: none builds.
  lane_step boundaries scripts/check-boundaries.sh
  # The bridge and Orion share one copy of each shm-* crate (the root Cargo.toml's patch of the Orion source).
  lane_step orion-copies scripts/check-orion-copies.sh
  # The unit, udev and sysusers files; nothing is installed here.
  lane_step packaging scripts/check-packaging.sh
  # Every file under packaging/ is listed in packaging/files.txt (installed, or marked never), and the list is well formed.
  lane_step packaging-files scripts/check-packaging-files.py
  # The shipped programs (packaging/shipped.txt) pull in no C: no C compiler crate, no foreign -sys crate.
  # Also checks that docs/building.md lists each of them.
  lane_step shipped scripts/check-shipped.sh
  # The module's CONFIG_ symbols are all in docs/kernel-config.md, and the QEMU harness enables the required ones.
  lane_step kernel-config scripts/check-kernel-config.py
  # The module's C is one capped file of one-line shims (kernel/lemnos_helpers.c); nothing else.
  lane_step kernel-helpers scripts/check-kernel-helpers.py
  # packaging/uboot/boot.cmd run by U-Boot itself: stock U-Boot v2026.10 under QEMU's raspi3b (the Pi 64-bit build from its pinned release
  # (testing/uboot/build-uboot.sh, cached in UBOOT_DIR), with the disk image as the SD card, runs the script over disk images with the slot files and
  # the uboot-env partition, boot by boot, and checks the slot each boot starts, the environment it leaves, and
  # the guards (the boot-count ladder, the rescue at 8, and the no-partition case). CI caches UBOOT_DIR.
  lane_step uboot testing/uboot/check.sh
}

# The lanes start; the main shell prints each finished step, in its lane's order, as it finishes.
# set -m gives each lane its own process group, so a failure can stop exactly the groups started here.
set -m
cargo_lane & lane_pid[c]=$!
side_lane & lane_pid[s]=$!
tail_lane & lane_pid[t]=$!
set +m
declare -A next=([c]=1 [s]=1 [t]=1) lane_name=([c]=cargo [s]=side [t]=tail)
show() { # prints the finished steps of lane $1 that are next in its order; a failing one stops the gate
  local lane="$1" key label rc
  while :; do
    key="$lane$(printf %03d "${next[$lane]}")"
    [ -f "$gate_out/$key.rc" ] || return 0
    label="$(cat "$gate_out/$key.label")"
    rc="$(cat "$gate_out/$key.rc")"
    echo "==> $label"
    cat "$gate_out/$key.out"
    if [ "$rc" != 0 ]; then
      echo "gate: $label failed (exit $rc)" >&2
      stop_lanes
      exit 1
    fi
    echo "    $label: $(cat "$gate_out/$key.secs") s"
    next[$lane]=$(( next[$lane] + 1 ))
  done
}
running=3
while [ "$running" -gt 0 ]; do
  for lane in c s t; do
    show "$lane"
    if [ "${lane_pid[$lane]}" != 0 ] && ! kill -0 "${lane_pid[$lane]}" 2>/dev/null; then
      rc=0; wait "${lane_pid[$lane]}" || rc=$?
      show "$lane"
      [ "$rc" = 0 ] || { echo "gate: the ${lane_name[$lane]} lane stopped (exit $rc) before its step finished" >&2; stop_lanes; exit 1; }
      lane_pid[$lane]=0
      running=$((running - 1))
    fi
  done
  if [ "$running" -gt 0 ]; then sleep 0.2; fi
done

echo "gate passed${GATE_PART:+ (part $GATE_PART)}"
