#!/usr/bin/env bash
# The check every PR into next must pass. CI runs exactly this.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# Every step has a CPU budget, written beside it in CPU-seconds, and no step has a wall-clock bound. A budget is
# 3 times the largest CPU (or wall, where larger) that the step used in any green train on the lander, and never
# less than 180 CPU-s: the floor for a cold cache (a fresh target dir compiles the dependencies first). The
# figures are from the lander's gate.log of the 141 green trains. A step that passes its budget is killed with
# its whole process group, and so is one that stops making progress (no CPU and nothing runnable for
# GATE_STALL_SAMPLES samples). Either way the gate fails and names the step. Load does not change either test:
# a slow step under load keeps using CPU or stays runnable. The process control is in scripts/gate-bounds.sh.
GATE_KILL_GRACE=3

gate_out="$(mktemp -d "${TMPDIR:-/tmp}/gate-out.XXXXXX")"
declare -A lane_pid=([c]=0 [s]=0 [t]=0 [f]=0)
stop_lanes() {
  # Only the lanes this shell started, their children, and the steps still running. Each lane and each
  # step is its own process group; a running step's pid and token are in $gate_out/<key>.pid.
  local lane f pid token
  for lane in c s t f; do
    [ "${lane_pid[$lane]}" = 0 ] || kill -TERM -- "-${lane_pid[$lane]}" 2>/dev/null || true
  done
  for f in "$gate_out"/*.pid; do
    [ -e "$f" ] || continue
    read -r pid token <"$f" || continue
    stop_step "$pid" "$token"
  done
}
# Every step the gate runs gets a temp dir of its own, as its TMPDIR: empty when the step starts and
# checked empty when it ends (in bounded, below). A step that leaves an entry there fails the gate, and
# the output lists what was left. The dirs sit directly under the gate's TMPDIR and are named g<key>:
# a socket path under TMPDIR must stay below 108 bytes, so the names stay short. The gate removes them
# on exit, including a step killed at its bound (its dir is checked after the kill).
step_dir() { echo "${TMPDIR:-/tmp}/$1"; }
clean_step_dirs() {
  local d
  for d in "${TMPDIR:-/tmp}"/g[ctfs][0-9][0-9][0-9] "${TMPDIR:-/tmp}"/gtop; do
    [ -e "$d" ] && rm -rf -- "$d"
  done
  return 0
}

# step_tmp_check <label> <dir>: lists what <dir> holds, removes it, and returns 1 if anything was there.
step_tmp_check() {
  local label="$1" dir="$2" e kib n=0
  for e in "$dir"/* "$dir"/.[!.]*; do
    [ -e "$e" ] || [ -L "$e" ] || continue
    n=$((n + 1))
    kib="$(du -sk -- "$e" 2>/dev/null | cut -f1 || true)"
    echo "    ${e##*/}  ${kib:-?} KiB"
  done
  rm -rf -- "$dir"
  if [ "$n" -gt 0 ]; then
    echo "gate: $label left $n entries in its TMPDIR (listed above)"
    return 1
  fi
}

# The ccache temp dir is shared by every step, so it is not checked empty. A step is charged with
# the entries that appeared in that dir during the step and are still there at its end; entries
# present before the step are ccache's steady state and are not counted. The lanes run side by
# side, so an entry another lane's compile left is charged here too: this is a note, not a failure.
ccache_tmp="${CCACHE_TEMPDIR:-}"
if [ -z "$ccache_tmp" ] && command -v ccache >/dev/null 2>&1; then
  ccache_tmp="$(ccache -k temporary_dir 2>/dev/null || true)"
fi
ccache_names() {
  if [ -n "$ccache_tmp" ] && [ -d "$ccache_tmp" ]; then ls -A -- "$ccache_tmp"; fi
}
# ccache_growth <label> <names before the step>
ccache_growth() {
  local label="$1" after new
  [ -n "$ccache_tmp" ] || return 0
  after="$(ccache_names || true)"
  new="$(comm -13 <(printf '%s\n' "$2" | grep . | sort) <(printf '%s\n' "$after" | grep . | sort) || true)"
  if [ -n "$new" ]; then
    echo "note: $label: ccache's temp dir ($ccache_tmp) gained $(printf '%s\n' "$new" | wc -l) entries during the step and they remain at its end (lanes run side by side, so one may be another step's):"
    printf '%s\n' "$new" | sed 's/^/    /'
  fi
}

trap 'stop_lanes; rm -rf -- "$gate_out"; clean_step_dirs' EXIT

# The process control (CPU budgets, the progress watch, the stop of a step) is in gate-bounds.sh.
. "$(dirname "${BASH_SOURCE[0]}")/gate-bounds.sh"

# Cargo.lock is tracked. A dependency change that did not update it fails here, before any
# other cargo command could rewrite it: --locked refuses to change the lock.
lockfile() {
  if ! cargo metadata --locked --format-version 1 >/dev/null; then
    echo "Cargo.lock is out of date with the manifests. Run: cargo metadata --format-version 1 >/dev/null" >&2
    echo "then commit Cargo.lock with the change (docs/building.md, \"The lock file\")." >&2
    return 1
  fi
}
step lockfile 180 lockfile

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
    # No shard has its own green time yet: a shard gets the bound of the whole workspace test step.
    step "tests, shard $k of $n" 7437 cargo test "${shard[@]}"
    echo "gate part $part passed"
    exit 0
    ;;
  *) echo "GATE_PART=$part: expected all, checks or tests:K/N" >&2; exit 1 ;;
esac

# The steps run in four lanes at once. The cargo lane holds the lint, the workspace tests, and the
# steps after them that build the programs the tests start (lemnosd, lemnos-ctl): cargo holds the
# target directory's lock for the whole of each build, so those steps cannot overlap, and they run in
# the order below. The side lane holds the steps that do not build: the U-Boot check (QEMU), the
# packaging and kernel checks, and the cargo queries that read only the lock file. The tail lane
# holds the cargo steps that touch nothing the workspace tests start or read: the doc tests, the
# builds for other targets, the shipped programs (their own profile) and the library crates. It
# starts once the workspace tests are built and running, when cargo's lock is free, and runs beside
# them. The features lane holds the feature sets the workspace tests do not build (library tests and
# checks only, no programs): it starts with the tail lane and runs beside it, and its result is kept
# per crate under the target directory, so a crate nothing touched costs a lookup. Each step's output is kept whole and printed under its "==> label" line when it finishes,
# with its "label: N s" line after it. A failing step's output is shown, the other lanes are
# stopped, and the gate fails. A step past its bound is killed and named the same way.

use_nextest=0
if [ "${GATE_TESTS:-nextest}" = nextest ] && cargo nextest --version >/dev/null 2>&1; then use_nextest=1; fi

cargo_lane() {
  local n=0
  # lane_step <label> <bound> <command...>
  lane_step() { n=$((n + 1)); unit "c$(printf %03d "$n")" "$@"; }
  lane_step clippy 1032 cargo clippy --workspace --all-targets -- -D warnings
  # The tail lane builds beside the running tests, so a thread per core oversubscribes the cores and
  # the contention is paid in CPU time. Measured warm on 18 cores (lander, fcaae491, 2 runs each):
  # 18 threads 133 s / 1538 CPU-s, 12 threads 143 s / 1363 CPU-s, 9 threads 159 s / 1299 CPU-s.
  # Two thirds of the cores keeps most of the wall time and costs the fewest CPU-s that does.
  test_jobs() { local n; n=$(( $(nproc) * 2 / 3 )); [ "$n" -ge 1 ] || n=1; echo "$n"; }
  # The workspace tests. `cargo test` runs the test programs one after another, so most cores sit idle;
  # cargo-nextest runs every test in its own process, across all programs at once. It does not run doc
  # tests, so those follow. Without cargo-nextest (or with GATE_TESTS=cargo) the plain form runs: the
  # same tests, slower. GATE_TEST_JOBS caps how many tests run at once (default: test_jobs above,
  # two thirds of the cores this process may use).
  # The tail lane waits for the tests to be built and running: nextest says "Starting N tests"
  # when its build is over and cargo's lock is free. tests.over is there once the step has ended,
  # whichever way, so the tail lane never waits on a build that failed.
  workspace_tests() {
    local rc=0 watcher
    if [ "$use_nextest" = 1 ]; then
      (until grep -qE '^ *Starting [0-9]+ tests' "$unit_out" 2>/dev/null; do sleep 0.2; done
        touch "$gate_out/tests.running") &
      watcher=$!
      cargo nextest run --workspace --no-fail-fast --test-threads "${GATE_TEST_JOBS:-$(test_jobs)}" || rc=$?
      kill "$watcher" 2>/dev/null || true
    else
      cargo test --workspace || rc=$?
    fi
    touch "$gate_out/tests.over"
    return "$rc"
  }
  [ "$part" = checks ] || lane_step tests 7437 workspace_tests

  lane_step "counters on" 180 cargo test -q -p lemnos-measure --features perf,alloc-count
  # A setting's meaning never depends on a feature: the same board text runs the same tests with and
  # without `std` (the generic sensors' `description_file`, the gadget's `interfaces_file`).
  lane_step "generic sensor, no std" 180 cargo test -q -p lemnos-driver-generic-sensor
  lane_step "generic sensor, std" 180 cargo test -q -p lemnos-driver-generic-sensor --features std
  lane_step "usb gadget, alloc" 180 cargo test -q -p lemnos-driver-usb-gadget
  lane_step "usb gadget, std" 180 cargo test -q -p lemnos-driver-usb-gadget --features std
  lane_step "client on tokio" 180 cargo clippy -q -p lemnos-client --all-targets --features tokio -- -D warnings
  lane_step "client on tokio" 180 cargo test -q -p lemnos-client --features tokio
  lane_step "C surface, shared form" 204 cargo test -q -p lemnos-capi --features shared
  # The vendor-header layouts: with no header given, the tool says per variant which header it needs and
  # compares nothing (the drivers' own layout assertions are compiled and tested above).
  lane_step "layout headers, not run in the gate" 180 cargo run -q -p lemnos-layout-check

  # The generated overlays compile with the dtc the contract names (built from its pinned release) with
  # every warning class an error, and apply to a base built from their SoC map (testing/devtree/check.sh).
  lane_step devtree 180 testing/devtree/check.sh
  # Every command in the documents runs against the simulated board, and every link leads somewhere.
  lane_step documents 222 scripts/check-docs.py
  # The mdev.conf that `lemnos-ctl tree --mdev` writes is run by BusyBox 1.37.0 and 1.38.0 (their
  # mdev, built from the pinned release tarballs), in a namespace, and each node's owner, group and mode
  # are checked against the udev rule for the same device. CI builds them first (BUSYBOX_DIR, cached).
  lane_step mdev 180 testing/mdev/check.sh
}

# The cargo steps that can run beside the workspace tests. None builds a program the tests start,
# and none writes where the tests' programs are: the doc tests build nothing new, the checks for other
# targets and the shipped programs have their own directories under the target, and the library
# crates have no programs. A step that builds lemnosd or lemnos-ctl for this machine is not one of
# them (the tests start those, and a build with other features would swap them mid-run): those stay
# in the cargo lane, after the tests.
tail_lane() {
  local n=0
  # lane_step <label> <bound> <command...>
  lane_step() { n=$((n + 1)); unit "t$(printf %03d "$n")" "$@"; }
  if [ "$part" != checks ]; then
    until [ -f "$gate_out/tests.running" ] || [ -f "$gate_out/tests.over" ]; do sleep 0.2; done
  fi
  # cargo-nextest does not run doc tests, so they run here. (The plain form runs them with the tests.)
  if [ "$part" != checks ] && [ "$use_nextest" = 1 ]; then
    lane_step "doc tests" 348 cargo test --workspace --doc
  fi

  # What must still build without std, on the targets a microcontroller host uses. Each of these checks
  # is 1-7 s on the lander (green max over 58 trains), so the floor sets every bound here.
  no_std_crates=(lemnos-abi lemnos-hal lemnos-core lemnos-contract-accelerometer lemnos-contract-gyroscope lemnos-contract-magnetometer lemnos-contract-orientation lemnos-driver-orientation lemnos-host-static lemnos-mcu lemnos-driver-usb-gadget lemnos-driver-generic-sensor shm-sample-time)
  for target in thumbv7em-none-eabihf riscv32imac-unknown-none-elf armv7-unknown-linux-gnueabihf; do
    for crate in "${no_std_crates[@]}"; do
      lane_step "$crate on $target" 180 cargo check -q -p "$crate" --no-default-features --target "$target"
    done
  done

  # What must build on the desktop targets: the crates meant to be portable, checked for Windows and
  # macOS. A crate listed here that stops building there fails the gate, and the step names it.
  # Linux-only crates stay off this list. Each check is 1-3 s on the lander, so the floor sets the bound.
  portable_crates=(shm-ring shm-ring-time shm-sample-time)
  for target in x86_64-pc-windows-msvc aarch64-apple-darwin x86_64-apple-darwin; do
    for crate in "${portable_crates[@]}"; do
      lane_step "$crate on $target" 180 cargo check -q -p "$crate" --target "$target"
    done
  done

  # The crates used as plain libraries, alone and without their driver halves.
  lane_step libraries 1695 scripts/check-libraries.sh
  # The shipped programs (packaging/shipped.txt) agree: see scripts/check-products.sh.
  lane_step products 606 scripts/check-products.sh
}

# The sets the workspace tests never build: each crate's tests in the exact set it ships with, and
# each library option (no defaults, all features) linted and built (scripts/check-features.sh). Its
# results are cached per crate (target/check-features), so a gate where nothing a crate depends on
# changed spends a lookup on it. It waits, like the tail lane, for cargo's lock to be free.
features_lane() {
  local n=0
  # lane_step <label> <bound> <command...>
  lane_step() { n=$((n + 1)); unit "f$(printf %03d "$n")" "$@"; }
  if [ "$part" != checks ]; then
    until [ -f "$gate_out/tests.running" ] || [ -f "$gate_out/tests.over" ]; do sleep 0.2; done
  fi
    lane_step features 1449 scripts/check-features.sh
}

side_lane() {
  local n=0
  # lane_step <label> <bound> <command...>
  lane_step() { n=$((n + 1)); unit "s$(printf %03d "$n")" "$@"; }
  lane_step format 180 cargo fmt --all --check
  # The steps below are the gate's checks that run the same as before, in this lane: none builds.
  lane_step boundaries 180 scripts/check-boundaries.sh
  # The bridge and Orion share one copy of each shm-* crate (the root Cargo.toml's patch of the Orion source).
  lane_step orion-copies 180 scripts/check-orion-copies.sh
  # The consumer-lock checkers (scripts/check-lemnos-pin.sh, scripts/check-shm-one-source.sh) pass the pinned fixture lock
  # and fail the stray one, listing its strays (testing/lock-checks/). No lander measurement yet.
  lane_step lock-checks 180 testing/lock-checks/check.sh
  # The unit, udev and sysusers files; nothing is installed here.
  lane_step packaging 180 scripts/check-packaging.sh
  # Every file under packaging/ is listed in packaging/files.txt (installed, or marked never), and the list is well formed.
  lane_step packaging-files 180 scripts/check-packaging-files.py
  # The grants the packaged unit and udev rules make (packaging/systemd/grants.toml) against the paths the daemon's crates
  # reach, and the checker's own cases (testing/unit-grants: each planted change must fail by name; the real tree holds).
  lane_step unit-grants 180 scripts/check-unit-grants.py
  lane_step unit-grants-tests 180 python3 -I testing/unit-grants/test_check_unit_grants.py
  # Every QEMU stanza that starts lemnosd starts it through the unit launcher, or says why it is root.
  lane_step root-stanzas 180 scripts/check-root-stanzas.py
  # The shipped programs (packaging/shipped.txt) pull in no C: no C compiler crate, no foreign -sys crate.
  # Also checks that docs/building.md lists each of them.
  lane_step shipped 180 scripts/check-shipped.sh
  # The module's CONFIG_ symbols are all in docs/kernel-config.md, and the QEMU harness enables the required ones.
  lane_step kernel-config 180 scripts/check-kernel-config.py
  # The module's C is one capped file of one-line shims (kernel/lemnos_helpers.c); nothing else.
  lane_step kernel-helpers 180 scripts/check-kernel-helpers.py
  # packaging/uboot/boot.cmd run by U-Boot itself: stock U-Boot v2026.10 under QEMU's raspi3b (the Pi 64-bit build from its pinned release
  # (testing/uboot/build-uboot.sh, cached in UBOOT_DIR), with the disk image as the SD card, runs the script over disk images with the slot files and
  # the uboot-env partition, boot by boot, and checks the slot each boot starts, the environment it leaves, and
  # the guards (the boot-count ladder, the rescue at 8, and the no-partition case). CI caches UBOOT_DIR.
  lane_step uboot 1878 testing/uboot/check.sh
}

# The lanes start; the main shell prints each finished step, in its lane's order, as it finishes.
# set -m gives each lane its own process group, so a failure can stop exactly the groups started here.
set -m
cargo_lane & lane_pid[c]=$!
side_lane & lane_pid[s]=$!
tail_lane & lane_pid[t]=$!
features_lane & lane_pid[f]=$!
set +m
declare -A next=([c]=1 [s]=1 [t]=1 [f]=1) lane_name=([c]=cargo [s]=side [t]=tail [f]=features)
show() { # shows the finished steps of lane $1 that are next in its order; a failing one stops the gate
  local lane="$1" key
  while :; do
    key="$lane$(printf %03d "${next[$lane]}")"
    [ -f "$gate_out/$key.rc" ] || return 0
    finish "$key"
    next[$lane]=$(( next[$lane] + 1 ))
  done
}
# As each lane ends, "gate: lane <name> ended at <S> s" (whole seconds from the gate's start, bash's
# SECONDS), and at the end "gate: wall <S> s": the lander reads them to see which lane set the wall.
running=4
while [ "$running" -gt 0 ]; do
  for lane in c s t f; do
    show "$lane"
    if [ "${lane_pid[$lane]}" != 0 ] && ! kill -0 "${lane_pid[$lane]}" 2>/dev/null; then
      rc=0; wait "${lane_pid[$lane]}" || rc=$?
      show "$lane"
      echo "gate: lane ${lane_name[$lane]} ended at $SECONDS s"
      [ "$rc" = 0 ] || { echo "gate: the ${lane_name[$lane]} lane stopped (exit $rc) before its step finished" >&2; stop_lanes; exit 1; }
      lane_pid[$lane]=0
      running=$((running - 1))
    fi
  done
  if [ "$running" -gt 0 ]; then sleep 0.2; fi
done

echo "gate: wall $SECONDS s"
echo "gate passed${GATE_PART:+ (part $GATE_PART)}"
