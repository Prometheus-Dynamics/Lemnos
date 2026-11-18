#!/usr/bin/env bash
# The B8 capture tool's rehearsal on the PC: lemnosd --sim with the fake FIFO accelerometer and gyroscope
# (testing/crates/fake-devices, built with --features fake-devices) and lemnos-bmi088-batch, with the commands
# the board session runs. Each case is one lemnosd, one board file written here, and one capture. Each
# case states the exit code its checks must give: 0 every check passes, 5 a check fails, 4 a data fault.
#
#   testing/crates/bmi088-batch/rehearsal.sh [--seconds N] [--out DIR]
#
# Cases (45 s each, as B8's check 3, unless --seconds):
#   accel-model        exact model stamps, the chip exact: every check passes (0)
#   accel-chip-fast    the chip 1.5% fast, the model stamps it: every check passes (0)
#   accel-prefix-tick  the pre-fix stamping (tick time, every stamp taken at the previous tick), the chip
#                      1.5% fast: check (a) fails (5)
#   accel-planted      the model's stamps planted 1.5% fast: checks (a) and (b) fail (5)
#   accel-torn         a torn batch: a data fault (4)
#   gyro-model         the gyroscope 0.8% slow on its own oscillator, the accelerometer exact: every check
#                      passes on both dies, and check 7's spread is printed (0)
#   gyro-fail          the gyroscope refuses its batches: a data fault (4)
#
# Environment:
#   LEMNOS_BIN   directory holding lemnosd, lemnos-ctl and lemnos-bmi088-batch (default:
#                ${CARGO_TARGET_DIR:-target}/debug in the repository).
#   TMPDIR       where the scratch directory and the sockets go (sockets need short paths).
#
# Exit status is 0 only when every case gave its expected exit code. Output: OUT/<case>.txt (the tool's
# rows) and OUT/<case>.lemnosd.log, default OUT = a directory under TMPDIR.
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../../.." && pwd)
die() { echo "rehearsal: $*" >&2; exit 2; }

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

# The board: the accelerometer is `imu` and the gyroscope is `imu-gyro`, so the gyro case can name both.
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

fails=0
# case NAME EXPECTED_EXIT ACCEL_CONFIG GYRO_CONFIG TOOL_ARGS...
case_run() {
  local name=$1 expect=$2 accel=$3 gyro=$4
  shift 4
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
  "$tool" --socket "$sock" "$@" --seconds "$seconds" > "$out/$name.txt" 2>&1
  local rc=$?
  stop
  if [ "$rc" = "$expect" ]; then
    echo "PASS $name: exit $rc (expected $expect)"
  else
    echo "FAIL $name: exit $rc, expected $expect; see $out/$name.txt"
    fails=$((fails + 1))
  fi
  grep -E "^b8\.(status|a\.|b\.|c\.|verdict|accel\.verdict|check7\.)" "$out/$name.txt" | sed 's/^/    /'
}

accel=(--device imu --contract accel)
gyro=(--device imu-gyro --contract gyro --accel-device imu)
case_run accel-model 0 "oscillator_ppm = 0.0, stamps = \"model\"" "oscillator_ppm = 0.0" "${accel[@]}"
case_run accel-chip-fast 0 "oscillator_ppm = 15000.0, stamps = \"model\"" "oscillator_ppm = 0.0" "${accel[@]}"
case_run accel-prefix-tick 5 "oscillator_ppm = 15000.0, stamps = \"tick\"" "oscillator_ppm = 0.0" "${accel[@]}"
case_run accel-planted 5 "oscillator_ppm = 0.0, stamp_error_ppm = 15000.0" "oscillator_ppm = 0.0" "${accel[@]}"
case_run accel-torn 4 "torn = true" "oscillator_ppm = 0.0" "${accel[@]}"
case_run gyro-model 0 "oscillator_ppm = 0.0" "oscillator_ppm = -8000.0" "${gyro[@]}"
case_run gyro-fail 4 "oscillator_ppm = 0.0" "fail = true" "${gyro[@]}"

if [ "$fails" = 0 ]; then
  echo "rehearsal: every case gave its expected exit ($out)"
else
  echo "rehearsal: $fails case(s) did not give their expected exit ($out)"
  exit 1
fi
