#!/usr/bin/env bash
# PC rehearsal of bench.sh --real. The real script runs, unchanged, against a stand-in board on this PC:
#   the data partition  a 96 MiB tmpfs, mounted in a private user and mount namespace (unshare -rm, so no root);
#   the daemon          the real lemnosd, started with --sim on a socket of its own; the board is not involved;
#   systemctl           a stand-in that answers `show` for the stand-in unit from the stand-in's real PID, and
#                       answers inactive and 0 once that PID is gone, as systemctl does for a unit not running;
#   the board's commands run on this PC with sh -c, through BENCH_SSH, and the board lock is a file of its own.
#
# Cases (each is checked; the script exits 1 if any case does not give the result it expects):
#   unit checks     data-check.sh: the bound, the half-partition rule, the listing diff and the gone check
#   refused, real   no stand-in: the real systemctl, for a unit that does not exist, refuses the run
#   refused         the scratch directory already exists
#   refused         less than half of the partition would stay free after the write
#   passes          a full run: the daemon is measured up, the scratch file is gone, /data is unchanged
#   fails           a full run in which a file is written to /data while the scratch file is there
#
#   testing/bench/rehearse.sh OUTROOT
#
# OUTROOT holds each case's run directory and log, on NVMe. Set CARGO_TARGET_DIR to a target on NVMe.
set -uo pipefail

if [ -z "${REHEARSE_IN_NS:-}" ]; then
  exec env REHEARSE_IN_NS=1 unshare -rm bash "$0" "$@"
fi

root=${1:?OUTROOT: a directory on NVMe for the runs}
bench=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$bench/../.." && pwd)
tgt=${CARGO_TARGET_DIR:-/mnt/nvme-wd/lemnos-targets/BENCH-DAEMON-UP}
unit=lemnosd.service
d=/run/user/1000/tp/BENCH-DAEMON-UP/bh-rehearse
export CARGO_TARGET_DIR=$tgt

rm -rf "$d"
mkdir -p "$d/data" "$d/shim" "$root"
mount -t tmpfs -o size=96m tmpfs "$d/data" || { echo "rehearse: cannot mount the stand-in /data" >&2; exit 1; }
sock=$d/lemnosd.sock
results=$root/rehearsal.txt
: >"$results"
failures=0

say() { echo "$*" | tee -a "$results"; }

say "rehearsal: $(date -u +%Y-%m-%dT%H:%M:%SZ), repo $(git -C "$repo" rev-parse --short HEAD) (+ the working tree), target $tgt"
say "stand-in /data: $(df -P -B1 "$d/data" | awk 'NR == 2 { print $2 " bytes total, " $4 " free" }')"

# The build the stand-in and the bench both use (the bench builds the same things, so this is a no-op then).
(cd "$repo" && nice -n 10 taskset -c 6-31 cargo build --profile service --offline -j 6 \
  -p lemnosd -p lemnos-ctl -p lemnos-bench-stamp) >"$root/build.log" 2>&1 ||
  { say "build failed: see $root/build.log"; exit 1; }
srv=$tgt/service/lemnosd

cp "$bench/board.toml" "$d/"

# The systemctl stand-in: it answers for the stand-in unit only, from the stand-in's PID.
cat >"$d/shim/systemctl" <<EOF
#!/bin/sh
# Stand-in for systemctl (rehearsal only): show -p PROP --value UNIT, answered as systemctl answers for a
# unit that is not running (inactive, MainPID 0) when the stand-in's PID is gone.
[ "\$1" = show ] || exit 2
shift
prop= unit=
while [ \$# -gt 0 ]; do
  case \$1 in
    -p) prop=\$2; shift 2 ;;
    --value) shift ;;
    *) unit=\$1; shift ;;
  esac
done
pid=\$(cat "$d/standin.pid" 2>/dev/null || echo 0)
if [ "\$unit" = "$unit" ] && [ "\$pid" -gt 0 ] && [ -d "/proc/\$pid" ]; then
  active=active
else
  active=inactive
  pid=0
fi
case \$prop in
  ActiveState) echo "\$active" ;;
  MainPID) echo "\$pid" ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$d/shim/systemctl"

# The transport: the board's commands run here, as ssh would run them remotely.
cat >"$d/fake-ssh" <<'EOF'
#!/bin/sh
# Rehearsal transport (testing/bench/rehearse.sh): runs the command on this PC with sh -c.
while [ $# -gt 0 ]; do
  case $1 in
    -n) shift ;;
    -i | -o | -p | -l) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
shift
exec sh -c "$*"
EOF
chmod +x "$d/fake-ssh"

start_standin() {
  rm -f "$sock"
  "$srv" --sim --board "$d/board.toml" --socket "$sock" >"$root/standin.log" 2>&1 &
  standin=$!
  echo "$standin" >"$d/standin.pid"
  for _ in $(seq 1 100); do
    [ -S "$sock" ] && return 0
    kill -0 "$standin" 2>/dev/null || break
    sleep 0.1
  done
  say "the stand-in daemon did not open $sock: see $root/standin.log"
  exit 1
}
stop_standin() {
  kill "$standin" 2>/dev/null
  wait "$standin" 2>/dev/null
  rm -f "$d/standin.pid"
}

# bench_case NAME [PATH-PREFIX] -- the bench run for one case; its log and output directory are kept.
bench_case() {
  local name=$1 prefix=$2
  env PATH="${prefix:+$prefix:}$PATH" BENCH_SSH="$d/fake-ssh" BENCH_LOCK="$d/board-gpu.lock" \
    BENCH_RUN_ROOT="$d/run" CARGO_BUILD_JOBS=4 "$bench/bench.sh" --real --board rehearsal \
    --data-mount "$d/data" --scratch-dir "$d/data/lemnos-bench" --daemon-socket "$sock" \
    --out "$root/$name" >"$root/$name.log" 2>&1
}

expect() { # NAME WANT-STATUS(0|fail) PATTERN [LOGFILE]
  local name=$1 want=$2 pattern=$3 logf rc=$5
  logf=${4:-$root/$name.log}
  local ok=1
  if [ "$want" = 0 ] && [ "$rc" -ne 0 ]; then ok=0; fi
  if [ "$want" = fail ] && [ "$rc" -eq 0 ]; then ok=0; fi
  if ! grep -q -- "$pattern" "$logf" 2>/dev/null; then ok=0; fi
  if [ "$ok" = 1 ]; then
    say "PASS $name (status $rc): $(grep -m1 -- "$pattern" "$logf" | tr -s ' ')"
  else
    say "FAIL $name (status $rc, wanted $want with '$pattern'): see $logf"
    failures=$((failures + 1))
  fi
}

# --- 1. the checks the bench uses, on their own
dc=$bench/data-check.sh
"$dc" room a/b 100 90 10 >/dev/null 2>&1 && say "PASS room ok: 100 total, 90 free, 10 written leaves 80" ||
  { say "FAIL room ok"; failures=$((failures + 1)); }
room_out=$("$dc" room "$d/data/lemnos-bench/run.tar" 96 40 30 2>&1); rc=$?
[ "$rc" -ne 0 ] && echo "$room_out" | grep -q "refused: .*run.tar would leave .* less than half" &&
  say "PASS room refuses by name: 96 total, 40 free, 30 written: $(echo "$room_out" | grep refused | tr -s ' ')" ||
  { say "FAIL room half rule: $room_out"; failures=$((failures + 1)); }
room_out=$("$dc" room "$d/data/lemnos-bench/run.tar" 1000000000000 1000000000000 $((65 * 1024 * 1024)) 2>&1); rc=$?
[ "$rc" -ne 0 ] && echo "$room_out" | grep -q "over the bound" &&
  say "PASS room refuses over the bound: 65 MiB against 64 MiB" ||
  { say "FAIL room bound: $room_out"; failures=$((failures + 1)); }
mkdir -p "$d/listcheck" && echo a >"$d/listcheck/f" && "$dc" list "$d/listcheck" >"$root/list-a.tsv"
echo b >>"$d/listcheck/f" && "$dc" list "$d/listcheck" >"$root/list-b.tsv"
"$dc" diff "$root/list-a.tsv" "$root/list-b.tsv" >"$root/list-diff.txt" 2>&1; rc=$?
[ "$rc" -ne 0 ] && grep -q "^[-+]f" "$root/list-diff.txt" &&
  say "PASS listing diff catches a changed file: $(grep -c '^[-+]f' "$root/list-diff.txt") changed lines" ||
  { say "FAIL listing diff"; failures=$((failures + 1)); }
"$dc" gone "$d/data" >/dev/null 2>&1; rc=$?
[ "$rc" -ne 0 ] && say "PASS gone refuses a path that is there" || { say "FAIL gone"; failures=$((failures + 1)); }
rm -rf "$d/listcheck"

# --- 2. the real systemctl, for a unit that does not exist here: the run is refused
bench_case refused-real-systemctl ""; rc=$?
expect refused-real-systemctl fail "cannot ask systemd on rehearsal" "" "$rc"
[ -e "$d/data/lemnos-bench" ] && { say "FAIL nothing written: the scratch directory exists"; failures=$((failures + 1)); }

# --- 3. the stand-in daemon is up for the rest
start_standin
say "stand-in daemon: lemnosd --sim, pid $standin, socket $sock"

# --- 4. the scratch directory already exists: refused
mkdir "$d/data/lemnos-bench"
bench_case refused-scratch-exists "$d/shim"; rc=$?
expect refused-scratch-exists fail "already exists" "" "$rc"
rmdir "$d/data/lemnos-bench"

# --- 5. less than half would stay free: refused by name, nothing written
dd if=/dev/zero of="$d/data/filler" bs=1M count=60 status=none
bench_case refused-half-rule "$d/shim"; rc=$?
expect refused-half-rule fail "less than half the partition" "" "$rc"
[ -e "$d/data/lemnos-bench" ] && { say "FAIL nothing written: the scratch directory exists"; failures=$((failures + 1)); }
rm -f "$d/data/filler"

# --- 6. a full run: the daemon is measured up, the scratch file goes, /data is unchanged
bench_case passes "$d/shim"; rc=$?
expect passes 0 "no difference outside the scratch file" "" "$rc"
[ -e "$d/data/lemnos-bench" ] && { say "FAIL the scratch directory is left behind"; failures=$((failures + 1)); }
[ -z "$(ls -A "$d/data")" ] || { say "FAIL /data is not empty after the pass: $(ls -A "$d/data")"; failures=$((failures + 1)); }

# --- 7. a file written to /data while the scratch file is there: the run fails and names it
(
  until [ -e "$d/data/lemnos-bench/run.tar" ]; do sleep 0.2; done
  echo "written during the run" >>"$d/data/daemon-journal"
) &
writer=$!
bench_case fails-outside-write "$d/shim"; rc=$?
wait "$writer" 2>/dev/null
expect fails-outside-write fail "changed outside the scratch file" "" "$rc"
grep -q "daemon-journal" "$root/fails-outside-write/data-diff.txt" 2>/dev/null &&
  say "PASS the diff names the file: $(grep -m1 'daemon-journal' "$root/fails-outside-write/data-diff.txt")" ||
  { say "FAIL the diff does not name daemon-journal"; failures=$((failures + 1)); }
rm -f "$d/data/daemon-journal"

stop_standin
say "cases with a wrong result: $failures"
[ "$failures" -eq 0 ]
