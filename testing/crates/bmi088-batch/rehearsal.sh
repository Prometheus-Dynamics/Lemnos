#!/usr/bin/env bash
# The B8 board session, rehearsed on the PC: lemnosd --sim with the fake FIFO accelerometer and gyroscope
# (testing/crates/fake-devices, built with --features fake-devices) and lemnos-bmi088-batch.
#
#   testing/crates/bmi088-batch/rehearsal.sh [--seconds N] [--out DIR] [--only "CASE ..."]
#
# --only runs the named cases of part 2 alone, with no session: for running one case again.
#
# Part 1, the session. It runs session.sh, the script the board runs, with MODE=sim: each die captured
# from its bind (tick time, then the model) and again after convergence, the gyroscope's with the
# accelerometer on the same polls for check 7, and each capture file replayed. Every capture must pass.
#
# Part 2, the cases: one lemnosd, one board file written here, and one capture each. A case states the
# exit code its checks must give (0 every check passes, 5 a check fails, 4 a data fault, 3 not measured)
# and the verdict rows that must read as stated, so a case that is red is red for its own reason.
#
#   accel-model        the model from the first sample, the chip exact: every check passes (0)
#   accel-chip-fast    the chip 1.5% fast, the model stamps it: every check passes (0)
#   accel-prefix-tick  the stamping from before the model (tick time only, not held in order), the chip
#                      1.5% fast: backwards stamps, and no model record to measure a rate on (5)
#   accel-planted      the model's stamps planted 1.5% fast: checks (a), (b) and (d) fail (5)
#   accel-torn         a torn batch: a data fault (4)
#   gyro-model         the gyroscope 0.8% slow on its own oscillator, the accelerometer exact: every check
#                      passes on both dies, and check 7 passes (0)
#   gyro-fail          the gyroscope refuses its batches: a data fault (4)
# The planted faults of the scoped verdicts (each from bind, so both modes are in the capture):
#   tick-backwards     a tick-time stamp earlier than the record before it: check (c) fails, each mode's
#                      own bounds pass (5)
#   tick-stale         tick-time stamps two drain periods old: the tick-time verdict fails, the model's
#                      passes (5)
#   model-gap          one sample lost between two model records: check (c) and the model's verdict fail,
#                      the tick-time verdict passes (5). When the lost sample is a batch's newest, check (a)
#                      sees that batch's newest stamp one period older and fails with it.
#   gyro-offset        the gyroscope's model stamps 20 ms late, a constant offset between the dies: check
#                      7 fails on the same-poll difference (5)
#   no-model           a die that never reaches its model, with --wait-model: NOT MEASURED (3)
#
# Each case that writes a capture file and gives 0 or 5 replays it: the replay must give the same exit.
#
# Environment:
#   LEMNOS_BIN   directory holding lemnosd, lemnos-ctl and lemnos-bmi088-batch (default:
#                ${CARGO_TARGET_DIR:-target}/debug in the repository).
#   TMPDIR       where the scratch directory and the sockets go (sockets need short paths).
#
# The simulator runs in real time, so run this on a machine that is not overloaded: a host that ticks a whole
# drain period late, or a tool that cannot poll, shows as a red here as it would on the board.
#
# Exit status is 0 only when the session passed and every case gave its expected exit code and rows.
# Output: OUT/session/ (the session's own files), OUT/<case>.txt (the tool's rows) and
# OUT/<case>.lemnosd.log, default OUT = a directory under TMPDIR. The planted cases need 15 s or more
# (--seconds): the accelerometer's model begins 6 s after bind, and the lost sample is 2 s after that.
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../../.." && pwd)
die() { echo "rehearsal: $*" >&2; exit 2; }

seconds=45
out=
only=
while [ $# -gt 0 ]; do
  case $1 in
    --seconds) seconds=${2:?--seconds needs a number}; shift ;;
    --out) out=${2:?--out needs a directory}; shift ;;
    --only) only=${2:?--only needs case names}; shift ;;
    *) die "unknown option $1" ;;
  esac
  shift
done

bin=${LEMNOS_BIN:-${CARGO_TARGET_DIR:-$repo/target}/debug}
lemnosd=$bin/lemnosd
ctl=$bin/lemnos-ctl
tool=$bin/lemnos-bmi088-batch
for f in "$lemnosd" "$ctl" "$tool"; do [ -x "$f" ] || die "no $f: build lemnosd and lemnos-ctl with --features lemnosd/fake-devices, and lemnos-bmi088-batch"; done

work=$(mktemp -d "${TMPDIR:-/tmp}/b8r.XXXXXX") || die "no scratch directory"
[ -n "$out" ] || out=$work/out
mkdir -p "$out"
pid=

stop() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  fi
  pid=
}
trap 'stop; rm -rf "$work"' EXIT

fails=0

session_part() {
echo "== the session (session.sh, MODE=sim)"
if MODE=sim BIN=$bin BUS_NOTE="sim: no bus" "$here/session.sh" --seconds "$seconds" --out "$out/session"; then
  echo "PASS session"
else
  echo "FAIL session: see $out/session/summary.txt"
  fails=$((fails + 1))
fi
# The from-bind captures must hold both modes, and the captures after convergence the model alone:
# a session that passes without a tick-time record has not rehearsed the board's.
for tag in accel-c1 gyro-c1; do
  if awk -F'\t' '$1 == "b8.tick_records" && $3 > 0 { t = 1 } $1 == "b8.model_records" && $3 > 0 { m = 1 } END { exit !(t && m) }' "$out/session/$tag.rows.txt" 2>/dev/null; then
    echo "PASS $tag holds tick-time and model records"
  else
    echo "FAIL $tag does not hold both modes"
    fails=$((fails + 1))
  fi
done
for tag in accel-c2 gyro-c2; do
  if awk -F'\t' '$1 == "b8.tick_records" && $3 == 0 { t = 1 } END { exit !t }' "$out/session/$tag.rows.txt" 2>/dev/null; then
    echo "PASS $tag holds the model's records only"
  else
    echo "FAIL $tag holds tick-time records after --wait-model"
    fails=$((fails + 1))
  fi
done
}
[ -n "$only" ] || session_part

# The board: the accelerometer is `imu` and the gyroscope is `imu-gyro`, so a gyro case can name both.
board() { # board FILE ACCEL_CONFIG GYRO_CONFIG
  cat > "$1" <<BOARD
format = "lemnos.board"
schema_version = 1

[board]
id = "b8-rehearsal"

[[devices]]
id = "imu"
driver = "fake-fifo-accelerometer"
config = { $2 }

[[devices]]
id = "imu-gyro"
driver = "fake-fifo-gyroscope"
config = { $3 }
BOARD
}

# case_run NAME EXPECTED_EXIT ROWS ACCEL_CONFIG GYRO_CONFIG TOOL_ARGS...
# ROWS is a space-separated list of key=status the tool's rows must hold (`b8.` is put before each key).
case_run() {
  local name=$1 expect=$2 rows=$3 accel=$4 gyro=$5
  shift 5
  if [ -n "$only" ]; then
    case " $only " in *" $name "*) ;; *) return ;; esac
  fi
  local sock=$work/$name.sock
  board "$work/$name.toml" "$accel" "$gyro"
  "$lemnosd" --sim --board "$work/$name.toml" --socket "$sock" > "$out/$name.lemnosd.log" 2>&1 &
  pid=$!
  local up=0
  for _ in $(seq 1 60); do
    if "$ctl" --socket "$sock" list > /dev/null 2>&1; then up=1; break; fi
    sleep 0.25
  done
  if [ "$up" != 1 ]; then
    echo "FAIL $name: lemnosd did not come up; see $out/$name.lemnosd.log"
    stop; fails=$((fails + 1)); return
  fi
  local rec=() replay=()
  case " $* " in
    *" gyro "*)
      rec=(--out "$out/$name.tsv" --accel-out "$out/$name.accel.tsv")
      replay=(--replay "$out/$name.tsv" --contract gyro --accel-replay "$out/$name.accel.tsv") ;;
    *)
      rec=(--out "$out/$name.tsv")
      replay=(--replay "$out/$name.tsv" --contract accel) ;;
  esac
  "$tool" --socket "$sock" "$@" "${rec[@]}" --seconds "$seconds" > "$out/$name.txt" 2>&1
  local rc=$?
  stop
  local bad=
  for want in $rows; do
    grep -q "^b8\.${want%%=*}	${want#*=}	" "$out/$name.txt" || bad="$bad $want"
  done
  if [ "$rc" = "$expect" ] && [ -z "$bad" ]; then
    echo "PASS $name: exit $rc (expected $expect)${rows:+; rows as stated: $rows}"
  else
    echo "FAIL $name: exit $rc, expected $expect${bad:+; rows not as stated:$bad}; see $out/$name.txt"
    fails=$((fails + 1))
  fi
  grep -E "^b8\.(status|wait_model|tick_records|model_records|a\.|b\.|c\.|d\.|model\.verdict|tick\.verdict|verdict|accel\.verdict|check7\.)" "$out/$name.txt" | sed 's/^/    /'
  if [ "$rc" = 0 ] || [ "$rc" = 5 ]; then
    local elapsed
    elapsed=$(awk -F'\t' '$1 == "b8.d.elapsed_ms" { printf "%.6f", $3 / 1000 }' "$out/$name.txt")
    "$tool" "${replay[@]}" --elapsed-s "${elapsed:-$seconds}" --poll-ms 50 > "$out/$name.replay.txt" 2>&1
    local rr=$?
    if [ "$rr" = "$rc" ]; then
      echo "PASS $name replay: exit $rr, the same as the live run"
    else
      echo "FAIL $name replay: exit $rr, live run gave $rc; see $out/$name.replay.txt"
      fails=$((fails + 1))
    fi
  fi
}

echo "== the cases"
accel=(--device imu --contract accel)
gyro=(--device imu-gyro --contract gyro --accel-device imu)
exact="oscillator_ppm = 0.0"
case_run accel-model 0 "verdict=pass tick.verdict=none" "$exact, stamps = \"model\"" "$exact" "${accel[@]}"
case_run accel-chip-fast 0 "verdict=pass" "oscillator_ppm = 15000.0, stamps = \"model\"" "$exact" "${accel[@]}"
case_run accel-prefix-tick 5 "c.verdict=fail b.verdict=fail" "oscillator_ppm = 15000.0, stamps = \"tick\"" "$exact" "${accel[@]}"
case_run accel-planted 5 "b.verdict=fail" "$exact, stamp_error_ppm = 15000.0" "$exact" "${accel[@]}"
case_run accel-torn 4 "" "torn = true" "$exact" "${accel[@]}"
case_run gyro-model 0 "verdict=pass accel.verdict=pass check7.verdict=pass" "$exact" "oscillator_ppm = -8000.0" "${gyro[@]}"
case_run gyro-fail 4 "" "$exact" "fail = true" "${gyro[@]}"
case_run tick-backwards 5 "c.verdict=fail a.verdict=pass b.verdict=pass tick.verdict=pass model.verdict=pass verdict=fail" \
  "$exact, stamps = \"bind\", fault = \"tick-backwards\"" "$exact" "${accel[@]}"
case_run tick-stale 5 "tick.verdict=fail model.verdict=pass c.verdict=pass verdict=fail" \
  "$exact, stamps = \"bind\", fault = \"tick-stale\"" "$exact" "${accel[@]}"
case_run model-gap 5 "c.verdict=fail c.model.misses=measured model.verdict=fail tick.verdict=pass b.verdict=pass verdict=fail" \
  "$exact, stamps = \"bind\", fault = \"model-gap\"" "$exact" "${accel[@]}"
case_run gyro-offset 5 "check7.poll_verdict=fail check7.verdict=fail accel.verdict=pass" \
  "$exact" "$exact, stamp_offset_ms = 20.0" "${gyro[@]}"
case_run no-model 3 "wait_model=not_reached status=not_measured" "$exact, stamps = \"tick\"" "$exact" "${accel[@]}" --wait-model 3

if [ "$fails" = 0 ] && [ -n "$only" ]; then
  echo "rehearsal: the cases run ($only) gave their expected exit and rows ($out)"
elif [ "$fails" = 0 ]; then
  echo "rehearsal: the session passed and every case gave its expected exit and rows ($out)"
else
  echo "rehearsal: $fails step(s) did not come out as stated ($out)"
  exit 1
fi
