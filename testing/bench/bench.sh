#!/usr/bin/env bash
# The reference set's harness. docs/design.md ("Proof on hardware") lists what it measures. The
# methods are the ones dev's earlier figures came from (`git show ed43ff3:TODO.md`).
#
#   testing/bench/bench.sh [--out DIR]
#       Dry run, the default. Simulation only: starts lemnosd --sim on the bench board file, records
#       what that can give, and prints the report with dev's figures beside it. No board, no ssh,
#       no systemctl, no hardware.
#
#   testing/bench/bench.sh --real --board USER@HOST [--out DIR] [--daemon-socket PATH]
#                          [--scratch-dir DIR] [--data-mount DIR]
#       Real run on the board, from the local session that holds the board. The daemon that is
#       already up stays up: nothing is stopped, restarted or masked, and no second daemon is
#       started. The run measures that daemon (its socket, its PID and the sha256 of its binary).
#       Its only write to the board's data partition is one scratch file, SCRATCH/run.tar (default
#       /data/lemnos-bench/run.tar), removed at the end. The helper files live in /run on the board,
#       a tmpfs, and are removed too. The data partition is listed before and after the run; any
#       difference outside the scratch file fails the run. A run refuses before writing if less than
#       half of the partition would stay free, or if the scratch directory already exists.
#
# Environment:
#   CARGO_TARGET_DIR   build output (default: target/ in the repository). Must be NVMe or SSD.
#   BENCH_TMP          where --out is made (default: $TMPDIR, else /tmp). Must be NVMe or SSD.
#   BENCH_TARGET       a Rust target triple for the board build, e.g. aarch64-unknown-linux-gnu.
#   BENCH_RUN_ROOT    where the helper files are unpacked on the board (default: /run/lemnos-bench)
#   BENCH_BOARD_SUDO   a prefix for root commands on the board, e.g. "sudo -n" (default: none).
#   BENCH_DAEMON_UNIT  the systemd unit of the daemon to measure (default: lemnosd.service).
#   BENCH_SSH_OPTS     options for ssh, e.g. "-i KEYFILE".
#   BENCH_SSH          the ssh command (default: ssh). The PC rehearsal runs the board commands locally.
#   BENCH_LOCK         the board lock file (default: /run/lock/board-gpu.lock). The PC rehearsal names its own.
#   CARGO_BUILD_JOBS   parallel jobs for the build (default: 2).
#
# The board lock named in AGENTS.md is taken with flock on every run. A dry run continues without
# it when the lock file cannot be created, since it touches no board.
set -euo pipefail

bench_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$bench_dir/../.." && pwd)
lock=${BENCH_LOCK:-/run/lock/board-gpu.lock}
ssh_cmd=${BENCH_SSH:-ssh}
unit=${BENCH_DAEMON_UNIT:-lemnosd.service}
mode=dry-run
board=
out=
daemon_socket=/run/lemnos/lemnosd.sock
data_mount=
scratch_dir=

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
    --daemon-socket)
      [ $# -ge 2 ] || die "--daemon-socket needs a path"
      daemon_socket=$2
      shift
      ;;
    --scratch-dir)
      [ $# -ge 2 ] || die "--scratch-dir needs a directory"
      scratch_dir=$2
      shift
      ;;
    --data-mount)
      [ $# -ge 2 ] || die "--data-mount needs a directory"
      data_mount=$2
      shift
      ;;
    -h | --help)
      sed -n '2,25p' "$0"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done
if [ "$mode" = real ] && [ -z "$board" ]; then die "--real needs --board USER@HOST"; fi
if [ "$mode" != real ] && [ -n "$board" ]; then die "--board needs --real"; fi
if [ "$mode" != real ] && [ -n "$scratch_dir$data_mount" ]; then die "--scratch-dir and --data-mount need --real"; fi

data_mount=${data_mount:-/data}
scratch_dir=${scratch_dir:-$data_mount/lemnos-bench}
case $data_mount in /*) ;; *) die "--data-mount must be an absolute path" ;; esac
case $scratch_dir in
  /*) ;;
  *) die "--scratch-dir must be an absolute path" ;;
esac
case $scratch_dir in
  "$data_mount"/?* | "$data_mount"/?*/*) ;;
  *) die "--scratch-dir $scratch_dir is not inside $data_mount" ;;
esac
case $scratch_dir in
  *'/../'* | */..) die "--scratch-dir must not contain .." ;;
esac
scratch_file=$scratch_dir/run.tar

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
  # The 200 Hz stamping step: a static Rust binary, because the board has no python3.
  CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo build --profile service "${target[@]}" -p lemnos-bench-stamp
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
for bin in lemnosd lemnos-ctl lemnos-bench-stamp; do
  [ -x "$bin_dir/$bin" ] || die "$bin_dir/$bin is missing after the build"
done

work=$out/work
mkdir -p "$work"
cp "$bench_dir/board.toml" "$bench_dir/board-side.sh" "$bench_dir/data-check.sh" "$work/"
cp "$bin_dir/lemnosd" "$bin_dir/lemnos-ctl" "$bin_dir/lemnos-bench-stamp" "$work/"

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
    alloc.steady_state not_measured "" "nothing counts a lemnosd running on the board; service/lemnosd/tests/alloc.rs counts one in a test process on the build host" \
    light.frame_rate not_measured "" "no step in this harness: only the board run testing/board-runs/B17.md gives it" \
    fan.steps not_measured "" "no step in this harness: only the board run testing/board-runs/B14.md gives it" \
    power.readings not_measured "" "$why" \
    usb.cycle not_measured "" "$why" \
    camera.fps not_measured "" "no camera step in this harness: only the board run testing/board-runs/D9.md gives it" \
    camera.drops not_measured "" "no camera step in this harness: only the board run testing/board-runs/D9.md gives it" \
    camera.latency_ms not_measured "" "no camera step in this harness: only the board run testing/board-runs/D9.md gives it" \
    camera.latch_frames not_measured "" "no camera step in this harness: only the board run testing/board-runs/D9.md gives it" \
    lemnosd.rev not_measured "" "$why" \
    lemnosd.check not_measured "" "$why" \
    lemnosd.start not_measured "" "$why" \
    lemnosd.devices not_measured "" "$why" \
    data.isolation not_measured "" "$why"
}

# Runs on the board over ssh. Nothing here stops, starts, restarts or masks a unit.
# shellcheck disable=SC2086
ssh_run() { "$ssh_cmd" -n ${BENCH_SSH_OPTS:-} "$board" "$@"; }
# The same, with the caller's stdin passed through (the data-check script is sent this way).
# shellcheck disable=SC2086
ssh_in() { "$ssh_cmd" ${BENCH_SSH_OPTS:-} "$board" "$@"; }

# remote_exists PATH: 0 when something is at PATH on the board, 1 when nothing is, dies on any other
# failure (an unreachable board must not look like an empty one).
remote_exists() {
  local rc=0
  ssh_run "test -e '$1' || test -L '$1'" || rc=$?
  case $rc in
    0) return 0 ;;
    1) return 1 ;;
    *) die "cannot check $1 on $board (ssh status $rc)" ;;
  esac
}

# Set while the scratch file is ours (between the mkdir and its removal). The exit trap removes it.
created=0
rundir=
real_cleanup() {
  if [ "$created" = 1 ]; then
    ssh_run "${BENCH_BOARD_SUDO:-} sh -c 'rm -f \"\$0/run.tar\"; rmdir \"\$0\"' '$scratch_dir'" ||
      note "could not remove $scratch_file on $board: remove it by hand"
    created=0
  fi
  if [ -n "$rundir" ]; then
    ssh_run "${BENCH_BOARD_SUDO:-} rm -rf '$rundir'" || note "could not remove $rundir on $board: remove it by hand"
    rundir=
  fi
}

real_run() {
  local sudo arch active dpid exe_sha exe_size df_line total avail target size ts rc=0
  local failed=0
  sudo=${BENCH_BOARD_SUDO:-}
  ts=$(date -u +%Y%m%dT%H%M%SZ)
  rundir=${BENCH_RUN_ROOT:-/run/lemnos-bench}/run-$ts
  trap real_cleanup EXIT

  arch=$(ssh_run "uname -m") || die "cannot reach $board over ssh"
  printf '%s\n' "$arch" >"$out/board-arch.txt"

  # The daemon must be up. It is measured here, and left as it is.
  active=$(ssh_run "$sudo systemctl show -p ActiveState --value '$unit'") ||
    die "cannot ask systemd on $board about $unit"
  [ "$active" = active ] ||
    die "$unit is '$active' on $board: this run measures a daemon that is up. Nothing was changed."
  dpid=$(ssh_run "$sudo systemctl show -p MainPID --value '$unit'") || die "cannot read the PID of $unit"
  case $dpid in
    '' | 0 | *[!0-9]*) die "$unit has no main PID on $board ($dpid)" ;;
  esac
  exe_sha=$(ssh_run "$sudo sha256sum /proc/$dpid/exe" | cut -d' ' -f1) || die "cannot hash the binary of pid $dpid"
  [ -n "$exe_sha" ] || die "cannot hash the binary of pid $dpid"
  exe_size=$(ssh_run "$sudo stat -L -c %s /proc/$dpid/exe") || die "cannot size the binary of pid $dpid"
  printf 'lemnosd.rev\tmeasured\tsha256 %s\tthe running daemon of %s, pid %s, binary %s bytes: lemnosd has no version text on its socket, so the binary is its identity\n' \
    "$exe_sha" "$unit" "$dpid" "$exe_size" >>"$out/measured.tsv"
  printf 'size.lemnosd\tmeasured\t%s\tthe running binary on the board, pid %s\n' "$exe_size" "$dpid" >>"$out/measured.tsv"
  note "measuring $unit on $board: pid $dpid, binary sha256 $exe_sha"

  # The data partition: its size, and the mount point it is on.
  df_line=$(ssh_run "df -P -B1 '$data_mount' | awk 'NR == 2 { print \$2, \$4, \$6 }'") ||
    die "cannot read $data_mount on $board"
  read -r total avail target <<<"$df_line"
  [ "$target" = "$data_mount" ] || die "$data_mount is not a mount point on $board (df reports $target)"

  remote_exists "$scratch_dir" && die "refused: $scratch_dir already exists on $board; it is not this run's to touch. Nothing was written."

  # The listing before anything is written here.
  ssh_in "$sudo bash -s -- list '$data_mount'" <"$bench_dir/data-check.sh" >"$out/data-before.tsv" ||
    die "cannot list $data_mount on $board"
  note "listed $data_mount before the run: $(wc -l <"$out/data-before.tsv") entries"

  # The one scratch file: the run's helper files as a tar, sized before it is written.
  tar -C "$work" -cf "$out/run.tar" lemnos-ctl lemnos-bench-stamp board-side.sh data-check.sh
  size=$(stat -c %s "$out/run.tar")
  if ! "$bench_dir/data-check.sh" room "$scratch_file" "$total" "$avail" "$size" | tee "$out/room.txt"; then
    die "refused: $scratch_file is not written; $data_mount was left as it was"
  fi

  created=1
  ssh_in "$sudo sh -c 'set -e; mkdir -m 755 \"\$0\"; cat > \"\$0/run.tar\"' '$scratch_dir'" <"$out/run.tar" ||
    die "could not write $scratch_file on $board"

  # The helper files are run from /run (a tmpfs), not from the data partition.
  ssh_run "$sudo sh -c 'set -e; mkdir -p \"\$(dirname \"\$0\")\"; mkdir -m 700 \"\$0\"; tar -C \"\$0\" -xf \"\$1\"' '$rundir' '$scratch_file'" ||
    die "could not unpack the helper files on $board"
  ssh_run "$sudo bash '$rundir/board-side.sh' board '$rundir' '$daemon_socket' '$dpid'" \
    >"$out/board-rows.tsv" 2>"$out/board-side.err" || rc=$?
  if [ "$rc" -ne 0 ]; then
    note "the board steps failed (status $rc); see $out/board-side.err"
    failed=1
  fi
  cat "$out/board-rows.tsv" >>"$out/measured.tsv"

  # The scratch file goes, and its absence is shown.
  real_cleanup
  ssh_in "$sudo bash -s -- gone '$scratch_file'" <"$bench_dir/data-check.sh" || failed=1
  ssh_in "$sudo bash -s -- gone '$scratch_dir'" <"$bench_dir/data-check.sh" || failed=1

  # The listing after, and any difference outside the scratch file is a failure.
  ssh_in "$sudo bash -s -- list '$data_mount'" <"$bench_dir/data-check.sh" >"$out/data-after.tsv" ||
    die "cannot list $data_mount on $board after the run"
  if "$bench_dir/data-check.sh" diff "$out/data-before.tsv" "$out/data-after.tsv" >"$out/data-diff.txt"; then
    printf 'data.isolation\tmeasured\tno difference\t%s listed before and after; the scratch file is gone\n' "$data_mount" >>"$out/measured.tsv"
    note "$data_mount: no difference outside the scratch file"
  else
    printf 'data.isolation\tfailed\tdifference\t%s changed outside the scratch file: see data-diff.txt\n' "$data_mount" >>"$out/measured.tsv"
    note "FAILED: $data_mount changed outside the scratch file; see $out/data-diff.txt"
    failed=1
  fi
  ssh_run "$sudo rm -rf '$rundir'" || note "could not remove $rundir on $board: remove it by hand"
  rundir=
  trap - EXIT

  printf 'Board: %s (%s); daemon %s pid %s sha256 %s; data %s, scratch %s\n' \
    "$board" "$arch" "$unit" "$dpid" "$exe_sha" "$data_mount" "$scratch_file" >"$out/run-summary.txt"
  [ "$failed" -eq 0 ] || die "the run failed: see the notes above"
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

# Sizes of every service binary that built, from the build that ran. In a real run lemnosd's size is
# the running binary's, measured on the board above, not this build's.
while read -r name; do
  if [ "$mode" = real ] && [ "$name" = lemnosd ]; then continue; fi
  if [ -f "$bin_dir/$name" ]; then
    if [ "$mode" = real ]; then
      where="build host, not the board build (the board's build is not run here)"
    else
      where="build host $(uname -m), not the board build"
    fi
    printf 'size.%s\tmeasured\t%s\t%s\n' "$name" "$(stat -c %s "$bin_dir/$name")" "$where"
  fi
done < <(service_names) >>"$out/measured.tsv"

python3 -I "$bench_dir/report.py" --repo "$repo" --reference "$bench_dir/reference.tsv" \
  --measured "$out/measured.tsv" --mode "$([ "$mode" = real ] && echo board || echo dry-run)" --arch "$arch" | tee "$out/report.txt"
echo "bench: output in $out"
