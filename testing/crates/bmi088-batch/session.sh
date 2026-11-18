#!/usr/bin/env bash
# The B8 board session: both dies of the IMU, each captured from its bind and again once its sample
# clock's model stamps the records, with lemnos-bmi088-batch. The same script runs on the board (as root,
# from a directory holding the three binaries and the board file) and on the PC against the simulator, so
# the board's session is rehearsed with the board's own steps.
#
#   MODE=board BOARD_FILE=FILE testing/crates/bmi088-batch/session.sh [--seconds N] [--out DIR]
#   MODE=sim                   testing/crates/bmi088-batch/session.sh [--seconds N] [--out DIR]
#
# Two sessions, each one lemnosd and one bind:
#   accel   the accelerometer die: `--contract accel --device imu`
#   gyro    the gyroscope die, with the accelerometer read on the same polls for check 7:
#           `--contract gyro --device GYRO_DEVICE --accel-device imu`
# Each session takes two captures (N seconds each, 45 by default, 50 ms polls, 5 ms sample period):
#   c1      from bind: started as soon as `imu` is `running ok`. It holds the tick-time records from
#           before the model converges and the model's records after, and each is judged by its own
#           bounds (B8.md, "Scoped verdicts").
#   c2      after convergence: `--wait-model N` first, so the capture starts once every die read delivers
#           the model's records. A die that does not get there in N seconds is NOT MEASURED (the tool's
#           exit 3), never a pass.
# Each capture's file is replayed on the spot, and the replay must give the live exit code.
#
# What it writes under OUT (default /var/tmp/lemnos-b8 on the board, a scratch directory in sim):
#   NAME-cK.rows.txt        the tool's rows, verdicts included     NAME-cK.exit       its exit code
#   NAME-cK.tsv             the die's capture file                 gyro-cK.accel.tsv  the accelerometer's
#   NAME-cK.replay.txt/.exit   the replay of the file(s)
#   NAME-health.txt         `lemnos-ctl list` once a second, with the host's time: the device's health
#   NAME-list-start.txt, NAME-list-end.txt, NAME-show-end.txt, NAME.lemnosd.log, timing.txt, summary.txt
#
# Exit status: 0 when the four captures all exit 0 and each replay matches; 1 otherwise; 2 on a usage
# error or a daemon that did not come up. A capture that fails is recorded with its rows, and the session
# goes on to the next one.
#
# Environment:
#   MODE         board (default) or sim.
#   BOARD_FILE   board mode: the board file to run lemnosd on (the bench file with the IMU's `fifo = true`
#                and both rates at 200). Not changed.
#   BIN          directory holding lemnosd, lemnos-ctl and lemnos-bmi088-batch (default: the script's own
#                directory on the board, ${CARGO_TARGET_DIR:-target}/debug in the repository in sim; sim
#                needs lemnosd built with --features lemnosd/fake-devices).
#   LOCKS        board mode: the lock files held for the whole session, space separated (default
#                /run/lock/board-gpu.lock).
#   GYRO_DEVICE  the gyroscope's device (default imu on the board, where one device carries both dies,
#                and imu-gyro in sim, where each fake is one die).
#   BUS_NOTE     a line for bus-speed.txt: the bus and its speed, which the record needs.
#   TMPDIR       sim: where the scratch directory and the sockets go (sockets need short paths).
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
die() { echo "session: $*" >&2; exit 2; }

mode=${MODE:-board}
seconds=45
out=
while [ $# -gt 0 ]; do
  case $1 in
    --seconds) seconds=${2:?--seconds needs a number}; shift ;;
    --out) out=${2:?--out needs a directory}; shift ;;
    *) die "unknown option $1" ;;
  esac
  shift
done

work=
case $mode in
  board)
    bin=${BIN:-$here}
    board_file=${BOARD_FILE:?board mode needs BOARD_FILE}
    [ -r "$board_file" ] || die "cannot read $board_file"
    [ -n "$out" ] || out=/var/tmp/lemnos-b8
    daemon=(--board "$board_file")
    gyro_device=${GYRO_DEVICE:-imu}
    socket_dir=$out
    ;;
  sim)
    repo=$(cd "$here/../../.." && pwd)
    bin=${BIN:-${CARGO_TARGET_DIR:-$repo/target}/debug}
    work=$(mktemp -d "${TMPDIR:-/tmp}/b8s.XXXXXX") || die "no scratch directory"
    [ -n "$out" ] || out=$work/out
    # The two dies as the board's were measured (BOARD-4): the accelerometer 0.75 % fast, the gyroscope
    # on time, each stamping from its bind as the driver does (tick time, then the model).
    board_file=$work/b8-sim.toml
    cat > "$board_file" <<BOARD
format = "lemnos.board"
schema_version = 1

[board]
id = "b8-session-sim"

[[devices]]
id = "imu"
driver = "fake-fifo-accelerometer"
config = { stamps = "bind", oscillator_ppm = 7500.0 }

[[devices]]
id = "imu-gyro"
driver = "fake-fifo-gyroscope"
config = { stamps = "bind", oscillator_ppm = 0.0 }
BOARD
    daemon=(--sim --board "$board_file")
    gyro_device=${GYRO_DEVICE:-imu-gyro}
    socket_dir=$work
    ;;
  *) die "MODE must be board or sim" ;;
esac
lemnosd=$bin/lemnosd
ctl=$bin/lemnos-ctl
tool=$bin/lemnos-bmi088-batch
for f in "$lemnosd" "$ctl" "$tool"; do [ -x "$f" ] || die "no $f"; done
mkdir -p "$out" || die "cannot make $out"

pid=
watcher=
stop() {
  if [ -n "$watcher" ] && kill -0 "$watcher" 2>/dev/null; then
    kill -TERM "$watcher" 2>/dev/null
    wait "$watcher" 2>/dev/null
  fi
  watcher=
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  fi
  pid=
}
trap 'stop; [ -n "$work" ] && rm -rf "$work"' EXIT

if [ "$mode" = board ]; then
  fd=8
  for lock in ${LOCKS:-/run/lock/board-gpu.lock}; do
    eval "exec $fd>\"\$lock\"" || die "cannot open $lock"
    flock "$fd" || die "cannot lock $lock"
    fd=$((fd + 1))
  done
  echo "locks held: ${LOCKS:-/run/lock/board-gpu.lock} at $(date +%T)" > "$out/locks.txt"
else
  echo "MODE sim: no board, no lock" > "$out/locks.txt"
fi
echo "${BUS_NOTE:-bus not stated (set BUS_NOTE)}" > "$out/bus-speed.txt"
: > "$out/timing.txt"
: > "$out/summary.txt"

fails=0
note() { echo "$*" | tee -a "$out/summary.txt"; }

# capture TAG CONTRACT DEVICE WAIT: one capture and its replay. WAIT is 1 for the capture after convergence.
capture() {
  local tag=$1 contract=$2 device=$3 wait=$4
  local args=(--socket "$sock" --device "$device" --contract "$contract" --out "$out/$tag.tsv")
  local replay=(--replay "$out/$tag.tsv" --contract "$contract")
  if [ "$contract" = gyro ]; then
    args+=(--accel-device imu --accel-out "$out/$tag.accel.tsv")
    replay+=(--accel-replay "$out/$tag.accel.tsv")
  fi
  [ "$wait" = 1 ] && args+=(--wait-model "$seconds")
  echo "$tag start at $EPOCHREALTIME" >> "$out/timing.txt"
  "$tool" "${args[@]}" --seconds "$seconds" --period-ms 50 --sample-period-ms 5 \
    > "$out/$tag.rows.txt" 2> "$out/$tag.err"
  local rc=$?
  echo "$rc" > "$out/$tag.exit"
  echo "$tag end at $EPOCHREALTIME" >> "$out/timing.txt"
  local verdicts
  verdicts=$(awk -F'\t' '$1 ~ /^b8\.(model\.verdict|tick\.verdict|verdict|accel\.verdict|check7\.verdict|wait_model|status)$/ { printf " %s=%s", substr($1, 4), $2 }' "$out/$tag.rows.txt")
  if [ "$rc" = 0 ]; then
    note "PASS $tag: exit 0;$verdicts"
  else
    note "FAIL $tag: exit $rc;$verdicts"
    fails=$((fails + 1))
  fi
  # The file reads back to the same verdicts. A capture that was not taken (exit 3 or 4) has no file to replay.
  if [ "$rc" = 0 ] || [ "$rc" = 5 ]; then
    local elapsed
    elapsed=$(awk -F'\t' '$1 == "b8.d.elapsed_ms" { printf "%.6f", $3 / 1000 }' "$out/$tag.rows.txt")
    "$tool" "${replay[@]}" --elapsed-s "${elapsed:-$seconds}" --poll-ms 50 --sample-period-ms 5 \
      > "$out/$tag.replay.txt" 2>> "$out/$tag.err"
    local rr=$?
    echo "$rr" > "$out/$tag.replay.exit"
    if [ "$rr" = "$rc" ]; then
      note "PASS $tag replay: exit $rr, the same as the live capture"
    else
      note "FAIL $tag replay: exit $rr, the live capture gave $rc"
      fails=$((fails + 1))
    fi
  fi
}

# session NAME CONTRACT DEVICE: one lemnosd, one bind, the two captures.
session() {
  local name=$1 contract=$2 device=$3
  sock=$socket_dir/$name.sock
  rm -f "$sock"
  "$lemnosd" "${daemon[@]}" --socket "$sock" > "$out/$name.lemnosd.log" 2>&1 &
  pid=$!
  for _ in $(seq 1 100); do [ -S "$sock" ] && break; sleep 0.05; done
  echo "$name bind at $EPOCHREALTIME pid $pid" >> "$out/timing.txt"
  # The gate: `imu` is running ok. The reads before it answer AGAIN, which is not data.
  local gated=0
  for _ in $(seq 1 1000); do
    if "$ctl" --socket "$sock" list 2>/dev/null | awk '$1 == "imu" && $3 == "running" && $4 == "ok" { f = 1 } END { exit !f }'; then
      gated=1
      break
    fi
    sleep 0.01
  done
  echo "$name running ok gate=$gated at $EPOCHREALTIME" >> "$out/timing.txt"
  "$ctl" --socket "$sock" list > "$out/$name-list-start.txt" 2>&1
  if [ "$gated" != 1 ]; then
    note "FAIL $name: imu did not reach running ok: no capture (see $name.lemnosd.log)"
    fails=$((fails + 1))
    stop
    return
  fi
  # The device's health once a second, for the record: a re-anchor is named there.
  ( while :; do
      printf '%s\t%s\n' "$EPOCHREALTIME" "$("$ctl" --socket "$sock" list 2>&1 | awk '$1 ~ /^imu/' | tr '\n' ';')"
      sleep 1
    done ) > "$out/$name-health.txt" 2>&1 &
  watcher=$!
  capture "$name-c1" "$contract" "$device" 0
  capture "$name-c2" "$contract" "$device" 1
  "$ctl" --socket "$sock" list > "$out/$name-list-end.txt" 2>&1
  "$ctl" --socket "$sock" show imu > "$out/$name-show-end.txt" 2>&1
  stop
  echo "$name daemon stopped at $EPOCHREALTIME" >> "$out/timing.txt"
}

session accel accel imu
session gyro gyro "$gyro_device"
echo DONE > "$out/done.txt"

if [ "$fails" = 0 ]; then
  note "session: the four captures pass and each replay matches ($out)"
else
  note "session: $fails step(s) did not pass ($out)"
  exit 1
fi
