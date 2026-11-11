#!/usr/bin/env bash
# The reference set's harness. docs/design.md ("Proof on hardware") lists what it measures. The
# methods are the ones dev's earlier figures came from (`git show ed43ff3:TODO.md`).
#
#   testing/bench/bench.sh [--out DIR]
#       Dry run, the default. Simulation only: starts lemnosd --sim on the bench board file, records
#       what that can give, and prints the report with dev's figures beside it. No board, no ssh,
#       no systemctl, no hardware.
#
#   testing/bench/bench.sh --real --board USER@HOST [--out DIR]
#       Real run on the board, from the local session that holds the board. Stops the services that
#       match lemnos* (and BENCH_EXTRA_UNITS), runs next's lemnosd on the bench board file, records
#       the reference set, then starts the stopped services again.
#
# Environment:
#   CARGO_TARGET_DIR   build output (default: target/ in the repository). Must be NVMe or SSD.
#   BENCH_TMP          where --out is made (default: $TMPDIR, else /tmp).
#   BENCH_TARGET       a Rust target triple for the board build, e.g. aarch64-unknown-linux-gnu.
#   BENCH_BOARD_DIR    where the board keeps a run (default: /var/tmp/lemnos-bench).
#   BENCH_BOARD_SUDO   a prefix for root commands on the board, e.g. "sudo -n" (default: none).
#   BENCH_EXTRA_UNITS  more unit patterns to stop, separated by spaces.
#   BENCH_SSH_OPTS     options for ssh, e.g. "-i KEYFILE".
#   CARGO_BUILD_JOBS   parallel jobs for the build (default: 2).
#
# The board lock named in AGENTS.md is taken with flock on every run. A dry run continues without
# it when the lock file cannot be created, since it touches no board.
set -euo pipefail

bench_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$bench_dir/../.." && pwd)
lock=/run/lock/board-gpu.lock
mode=dry-run
board=
out=

die() { echo "bench: $*" >&2; exit 1; }
note() { echo "bench: $*" >&2; }

while [ $# -gt 0 ]; do
  case $1 in
    --real) mode=real ;;
    --board)
      [ $# -ge 2 ] || die "--board needs USER@HOST"
      board=$2
      shift
      ;;
    --out)
      [ $# -ge 2 ] || die "--out needs a directory"
      out=$2
      shift
      ;;
    -h | --help)
      sed -n '2,22p' "$0"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done
if [ "$mode" = real ] && [ -z "$board" ]; then die "--real needs --board USER@HOST"; fi
if [ "$mode" != real ] && [ -n "$board" ]; then die "--board needs --real"; fi

# The board lock. A real run needs it. A dry run goes on without it if it cannot be opened here.
if (: >>"$lock") 2>/dev/null; then
  exec 9>>"$lock"
  flock -w 900 9 || die "the board lock $lock is still held after 900 s"
elif [ "$mode" = real ]; then
  die "cannot open the board lock $lock"
else
  note "cannot open the board lock $lock from this account; a dry run touches no board and goes on without it"
fi

# solid_state DIR: refuses a directory on a rotating disk, since builds and runs go on NVMe or SSD.
solid_state() {
  local src base
  mkdir -p "$1"
  src=$(df --output=source "$1" | tail -n 1)
  base=$(lsblk -no PKNAME "$src" 2>/dev/null | head -n 1 || true)
  [ -n "$base" ] || base=$(basename "$src")
  [ -r "/sys/block/$base/queue/rotational" ] || die "cannot tell whether $1 is on solid state ($src)"
  [ "$(cat "/sys/block/$base/queue/rotational")" = 0 ] ||
    die "$1 is on a rotating disk ($base): build on NVMe or SSD"
}

target_dir=${CARGO_TARGET_DIR:-$repo/target}
if [ -n "${BENCH_TARGET:-}" ]; then
  bin_dir=$target_dir/$BENCH_TARGET/service
else
  bin_dir=$target_dir/service
fi
if [ -z "$out" ]; then
  out=$(mktemp -d "${BENCH_TMP:-${TMPDIR:-/tmp}}/lemnos-bench.XXXXXX")
fi
mkdir -p "$out"
solid_state "$target_dir"
solid_state "$out"

# Service crates, by package name, from their manifests. The first two are the ones the harness
# runs; the others are sized when they build.
service_names() {
  local dir name
  for dir in "$repo"/service/*/; do
    name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -n 1)
    [ -n "$name" ] && echo "$name"
  done
}

build() {
  local target=()
  if [ -n "${BENCH_TARGET:-}" ]; then target=(--target "$BENCH_TARGET"); fi
  cd "$repo"
  CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo build --profile service "${target[@]}" \
    -p lemnosd -p lemnos-ctl
  local name
  while read -r name; do
    case $name in
      lemnosd | lemnos-ctl) continue ;;
    esac
    CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo build --profile service "${target[@]}" \
      -p "$name" || note "$name did not build; its size is not measured"
  done < <(service_names)
}
build
for bin in lemnosd lemnos-ctl; do
  [ -x "$bin_dir/$bin" ] || die "$bin_dir/$bin is missing after the build"
done

work=$out/work
mkdir -p "$work"
cp "$bench_dir/board.toml" "$bench_dir/stamp.py" "$bench_dir/board-side.sh" "$work/"
cp "$bin_dir/lemnosd" "$bin_dir/lemnos-ctl" "$work/"

# Rows for every key of the reference set. A step that measures a key writes a later row, which
# wins. So a key that no step reached stays not measured, with the reason here.
placeholders() {
  local why=$1
  printf '%s\t%s\t%s\t%s\n' \
    imu.delivered not_measured "" "$why" \
    imu.misses not_measured "" "$why" \
    imu.largest_gap_ms not_measured "" "$why" \
    imu.repeats not_measured "" "$why" \
    imu.read_us not_measured "" "no read-time export on next: lemnos-ctl watch returns values, not timings" \
    cpu.idle_pct not_measured "" "$why" \
    cpu.50hz_pct not_measured "" "$why" \
    cpu.200hz_pct not_measured "" "$why" \
    threads.lemnosd not_measured "" "$why" \
    mem.pss_kb not_measured "" "$why" \
    alloc.steady_state not_measured "" "no allocation counter wraps lemnosd on next (engine/client/tests/alloc.rs counts the client only)" \
    light.frame_rate not_measured "" "no step in this harness yet: testing/board-runs/B17.md has the check" \
    fan.steps not_measured "" "no step in this harness yet: testing/board-runs/B14.md has the check" \
    power.readings not_measured "" "$why" \
    usb.cycle not_measured "" "$why" \
    camera.fps not_measured "" "no camera driver, receiver, ISP or frame stream on next" \
    camera.drops not_measured "" "no camera driver, receiver, ISP or frame stream on next" \
    camera.latency_ms not_measured "" "no camera driver, receiver, ISP or frame stream on next" \
    camera.latch_frames not_measured "" "no camera driver, receiver, ISP or frame stream on next"
}

# Runs on the board over ssh. Stops the services that match lemnos*, and starts them again on exit
# (the trap), whatever happens in between.
# shellcheck disable=SC2086
ssh_run() { ssh -n ${BENCH_SSH_OPTS:-} "$board" "$@"; }

stopped=$out/stopped-units.txt
restore_services() {
  local unit
  [ -s "$stopped" ] || return 0
  tac "$stopped" | while read -r unit; do
    ssh_run "${BENCH_BOARD_SUDO:-} systemctl start '$unit'" ||
      note "could not start $unit again: start it on the board"
  done
  : >"$stopped"
}

real_run() {
  local remote sudo units unit arch
  sudo=${BENCH_BOARD_SUDO:-}
  remote=${BENCH_BOARD_DIR:-/var/tmp/lemnos-bench}/run-$(date -u +%Y%m%dT%H%M%SZ)
  trap restore_services EXIT

  arch=$(ssh_run "uname -m") || die "cannot reach $board over ssh"
  printf '%s\n' "$arch" >"$out/board-arch.txt"
  ssh_run "mkdir -p '$remote'"
  tar -C "$work" -cf - lemnosd lemnos-ctl board.toml stamp.py board-side.sh |
    ssh ${BENCH_SSH_OPTS:-} "$board" "tar -C '$remote' -xf -"

  units=$(ssh_run "$sudo systemctl list-units --type=service --state=active --plain --no-legend 'lemnos*' ${BENCH_EXTRA_UNITS:-} | awk '{print \$1}'") ||
    die "could not list the services on $board"
  : >"$stopped"
  note "stopping on $board: ${units:-no services matched}"
  for unit in $units; do
    ssh_run "$sudo systemctl stop '$unit'" || die "could not stop $unit"
    printf '%s\n' "$unit" >>"$stopped"
  done

  ssh_run "$sudo bash '$remote/board-side.sh' board '$remote'" \
    >"$out/board-rows.tsv" 2>"$out/board-side.err" ||
    note "the board steps failed; see $out/board-side.err"
  cat "$out/board-rows.tsv" >>"$out/measured.tsv"
  restore_services
  note "board run files kept in $remote on $board"
}

placeholders "$([ "$mode" = real ] && echo "not measured in the board run" || echo "dry run: no board, so no timing or hardware read")" >"$out/measured.tsv"

if [ "$mode" = real ]; then
  real_run
  arch=$(cat "$out/board-arch.txt")
else
  bash "$work/board-side.sh" sim "$work" >>"$out/measured.tsv" 2>"$out/sim.err" ||
    note "the simulation steps failed; see $out/sim.err"
  arch=$(uname -m)
fi

# Sizes of every service binary that built, from the build that ran.
while read -r name; do
  if [ -f "$bin_dir/$name" ]; then
    if [ "$mode" = real ]; then
      where="board build, target ${BENCH_TARGET:-native}"
    else
      where="build host $(uname -m), not the board build"
    fi
    printf 'size.%s\tmeasured\t%s\t%s\n' "$name" "$(stat -c %s "$bin_dir/$name")" "$where"
  fi
done < <(service_names) >>"$out/measured.tsv"

python3 -I "$bench_dir/report.py" --repo "$repo" --reference "$bench_dir/reference.tsv" \
  --measured "$out/measured.tsv" --mode "$mode" --arch "$arch" | tee "$out/report.txt"
echo "bench: output in $out"
