#!/usr/bin/env bash
# The B14 fan-control rehearsal: the checks of testing/board-runs/B14.md that drive the controller
# through its curve, its slew limit and a hold, fail its sensor, and restart the service. It runs the
# same checks against the simulated board and against the real one.
#
#   testing/fan-control/b14.sh sim [--out DIR]
#       Starts lemnosd --sim on testing/fan-control/sim-board.toml, in a scratch directory. The fan
#       and the thermal zone are fakes (testing/crates/fake-devices). The zone's temperature is the
#       `temp` file in its directory, which this script writes. Stops its own lemnosd by PID on exit.
#       Needs lemnosd built with --features fake-devices.
#
#   testing/fan-control/b14.sh board --socket PATH --temp-hook CMD --restart-hook CMD [--out DIR]
#       Runs against the board's service, which must be running testing/fan-control/board.toml. Takes
#       the board lock first (AGENTS.md). CMD for --temp-hook is run with the target temperature in
#       millidegrees as its one argument; the operator makes the CPU reach it (load, or cool). CMD for
#       --restart-hook restarts the service. Build on NVMe or SSD, not the HDD.
#
# Environment:
#   LEMNOS_BIN   directory holding lemnosd and lemnos-ctl (default: ${CARGO_TARGET_DIR:-target}/debug
#                in the repository).
#   TMPDIR       where the scratch directory and the socket go (sockets need short paths).
#
# Checks print PASS or FAIL. Each sample of the controller is written to OUT/samples.tsv. Exit status
# is 0 only when every check passes. The timing in samples.tsv is the host's; on the sim it means
# nothing, and the board's figures are bench-board numbers only.
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
die() { echo "b14: $*" >&2; exit 2; }

mode=${1:-}
[ "$mode" = sim ] || [ "$mode" = board ] || die "usage: b14.sh sim|board [options]; see the top of this file"
shift

socket=${LEMNOSD_SOCKET:-}
temp_hook=
restart_hook=
out=
while [ $# -gt 0 ]; do
  case $1 in
    --socket) socket=${2:?--socket needs a path}; shift ;;
    --temp-hook) temp_hook=${2:?--temp-hook needs a command}; shift ;;
    --restart-hook) restart_hook=${2:?--restart-hook needs a command}; shift ;;
    --out) out=${2:?--out needs a directory}; shift ;;
    -h | --help) sed -n '2,25p' "$0"; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done

bin=${LEMNOS_BIN:-${CARGO_TARGET_DIR:-$repo/target}/debug}
lemnosd=$bin/lemnosd
ctl_bin=$bin/lemnos-ctl
[ -x "$lemnosd" ] && [ -x "$ctl_bin" ] || die "no lemnosd and lemnos-ctl in $bin (set LEMNOS_BIN)"

work=$(mktemp -d "${TMPDIR:-/tmp}/b14.XXXXXX") || die "no scratch directory"
out=${out:-$work/out}
mkdir -p "$out"
samples=$out/samples.tsv
log=$out/run.log
: > "$samples"
printf 'ms\tcheck\tduty\tdemand\tdegraded\theld\thealth\n' > "$samples"

pid=
pass=0
fail=0
say() { printf '%s\n' "$*" | tee -a "$log"; }
ok() { pass=$((pass + 1)); say "PASS $*"; }
bad() { fail=$((fail + 1)); say "FAIL $*"; }
expect() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }
now_ms() { awk '{ printf "%d\n", $1 * 1000 }' /proc/uptime; }

ctl() { "$ctl_bin" --socket "$socket" "$@"; }

cleanup() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  fi
}
trap cleanup EXIT

# The zone's temperature. The sim writes its zone file; the board's comes from the operator's hook.
set_temp() {
  if [ "$mode" = sim ]; then
    printf '%s\n' "$1" > "$zone/temp"
  else
    "$temp_hook" "$1" || die "the temperature hook failed for $1"
  fi
}

# expected MILLIDEGREES: the B14 curve's duty, 30 C at 300, 60 C at 600, 75 C at 1000, linear between,
# and the floor of 300 below 30 C.
expected() {
  awk -v t="$1" 'BEGIN {
    if (t <= 30000) d = 300;
    else if (t <= 60000) d = 300 + (t - 30000) * 300 / 30000;
    else if (t <= 75000) d = 600 + (t - 60000) * 400 / 15000;
    else d = 1000;
    printf "%d\n", d + 0.5
  }'
}

field() { printf '%s\n' "$2" | sed -n "s/.*$1: \([^,}]*\).*/\1/p"; }

# state_now: one read of the controller, into DUTY, DEMAND, DEGRADED and HELD. Returns 1 when the read
# is refused. It is the read that ticks the controller, as the board's runs read it.
state_now() {
  local reply
  reply=$(ctl call fan-control state 2>&1) || return 1
  DUTY=$(field duty "$reply")
  DEMAND=$(field demand "$reply")
  DEGRADED=$(field degraded "$reply")
  HELD=$(field held "$reply")
  case $DUTY in '' | *[!0-9-]*) return 1 ;; esac
  return 0
}

health_of() { ctl list | awk '$1 == "fan-control" { print $4 }'; }
fan_duty() { ctl read fan duty | awk '{ print $3 }'; }
bind_running() { ctl list | awk '$1 == "fan-control" && $3 == "running" && $4 == "ok" { f = 1 } END { exit !f }'; }
devices_listed() { ctl list | awk '$1 == "fan" { a = 1 } $1 == "cpu-thermal" { b = 1 } END { exit !(a && b) }'; }
fan_matches() { state_now && [ "$(fan_duty)" -eq "$DUTY" ]; }
state_not_held() { state_now && [ "$HELD" = false ]; }
healthy() { [ "$(health_of)" = ok ]; }

# sample CHECK: one read, written to samples.tsv with the time and the health.
sample() {
  state_now || return 1
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(now_ms)" "$1" "$DUTY" "$DEMAND" "$DEGRADED" "$HELD" "$(health_of)" >> "$samples"
}

# wait_until SECONDS COMMAND...: polls every half second until COMMAND succeeds.
wait_until() {
  local limit=$1 end
  shift
  end=$(($(now_ms) + limit * 1000))
  while :; do
    "$@" && return 0
    [ "$(now_ms)" -ge "$end" ] && return 1
    sleep 0.5
  done
}

at_expected() { state_now && [ $((DUTY - EXP)) -ge -1 ] && [ $((DUTY - EXP)) -le 1 ]; }
at_duty() { state_now && [ "$DUTY" -eq "$1" ]; }
degraded_full() { state_now && [ "$DEGRADED" = true ] && [ "$DUTY" -eq 1000 ]; }
recovered() { state_now && [ "$DEGRADED" = false ] && [ $((DUTY - EXP)) -ge -1 ] && [ $((DUTY - EXP)) -le 1 ]; }

# le32 N: N as four little-endian bytes in hex, the wire's form of an i32.
le32() { printf '%08x' $(($1 & 0xffffffff)) | sed -E 's/(..)(..)(..)(..)/\4\3\2\1/'; }
# point VALUE DUTY: one curve point as the wire has it: a length word of 8, then value and duty.
point() { printf '08000000%s%s' "$(le32 "$1")" "$(le32 "$2")"; }

start_sim() {
  "$lemnosd" --sim --board "$work/board.toml" --socket "$socket" >> "$out/lemnosd.log" 2>&1 &
  pid=$!
  wait_until 15 ctl list > /dev/null 2>&1 || die "lemnosd --sim did not come up; see $out/lemnosd.log"
}

stop_sim() {
  kill -TERM "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  pid=
}

# --- set up -------------------------------------------------------------------------------------

if [ "$mode" = sim ]; then
  zone=$work/zone
  socket=$work/lemnosd.sock
  mkdir -p "$zone"
  printf '45000\n' > "$zone/temp"
  sed "s|@ZONE@|$zone|" "$here/sim-board.toml" > "$work/board.toml"
  say "b14: sim, lemnosd from $bin, scratch $work"
  start_sim
else
  [ -n "$socket" ] || die "board mode needs --socket PATH (or LEMNOSD_SOCKET)"
  [ -n "$temp_hook" ] || die "board mode needs --temp-hook CMD"
  [ -n "$restart_hook" ] || die "board mode needs --restart-hook CMD"
  exec 9> /run/lock/board-gpu.lock
  flock -n 9 || die "the board lock /run/lock/board-gpu.lock is held elsewhere"
  say "b14: board, socket $socket"
  ctl list > /dev/null || die "no service answers on $socket"
fi

# --- 1 bind -------------------------------------------------------------------------------------

describe=$(ctl describe fan-control 2>&1)
expect "1 fan-control describes duty and demand" bash -c '[[ $1 == *duty* && $1 == *demand* ]]' _ "$describe"
expect "1 fan-control is listed running and ok" bind_running
expect "1 the fan and the zone are listed" devices_listed
ctl_list_health=$(health_of)
say "b14: fan-control health at bind is $ctl_list_health"

# --- 2 curve ------------------------------------------------------------------------------------

# A fall is slewed at 50 per second, so from full duty to 300 takes 14 s: the limit allows for it.
for t in 30000 45000 60000 70000 75000 20000; do
  EXP=$(expected "$t")
  set_temp "$t"
  expect "2 $t mC: duty $EXP within 1 of the curve" wait_until 25 at_expected
  expect "2 $t mC: the fan's duty equals the commanded duty" fan_matches
  expect "2 $t mC: the duty is at least the floor of 300" [ "$DUTY" -ge 300 ]
done

# A window of samples whose label matches PATTERN falls by at most 52 in any one second.
fall_limit_ok() {
  awk -F'\t' -v pat="$1" '$2 ~ pat { n++; t[n] = $1; d[n] = $3 }
    END {
      worst = 0; j = 1
      for (i = 1; i <= n; i++) {
        while (t[i] - t[j] > 1000) j++
        for (k = j; k <= i; k++) if (d[k] - d[i] > worst) worst = d[k] - d[i]
      }
      exit !(worst <= 52)
    }' "$samples"
}

# --- 3 heat -------------------------------------------------------------------------------------

# The duty reaches the curve within 1.1 s of the temperature passing the point (the B14 check 4).
EXP=$(expected 61000)
set_temp 61000
heat_t0=$(now_ms)
heat_ok=false
while [ $(($(now_ms) - heat_t0)) -le 1100 ]; do
  if sample "3 heat" && [ $((DUTY - EXP)) -ge -1 ] && [ $((DUTY - EXP)) -le 1 ]; then
    heat_ok=true
    break
  fi
  sleep 0.1
done
expect "3 heat past 60 C: the duty reaches the curve within 1.1 s" $heat_ok
say "b14: heat latency $(($(now_ms) - heat_t0)) ms (host clock, to the first read at the curve)"

# --- 4 cool (slew) -------------------------------------------------------------------------------

EXP=$(expected 30000)
set_temp 30000
settled=false
end=$(($(now_ms) + 40000))
while [ "$(now_ms)" -lt "$end" ]; do
  sample "4 cool" || break
  if [ "$DUTY" -le "$EXP" ]; then settled=true; break; fi
  sleep 0.25
done
expect "4 cooling falls at most 50 per second" fall_limit_ok '^4 '
expect "4 cooling settles at the curve's duty at 30 C" $settled

# --- 5 hold -------------------------------------------------------------------------------------

ctl call fan-control hold 900 10000 > /dev/null || bad "5 hold refused"
state_now
expect "5 hold: the duty reads 900 at once, held" bash -c '[ "$1" -eq 900 ] && [ "$2" = true ]' _ "$DUTY" "$HELD"
hold_t0=$(now_ms)
end=$((hold_t0 + 13000))
while [ "$(now_ms)" -lt "$end" ]; do
  sample "5 hold" || break
  [ "$HELD" = true ] || break
  sleep 0.25
done
held_span=$(awk -F'\t' '$2 == "5 hold" && $6 == "true" { if (!n) first = $1; last = $1; n++ } END { print n ? last - first : 0 }' "$samples")
# hold_ok SPAN: the held samples cover at least 9.5 s and every one reads 900.
hold_ok() {
  awk -F'\t' -v span="$1" 'BEGIN { bad = 0 }
    $2 == "5 hold" && $6 == "true" && $3 != 900 { bad = 1 }
    END { exit !(span >= 9500 && !bad) }' "$samples"
}
expect "5 hold: the duty is 900 and held for the 10 s" hold_ok "$held_span"

# After the hold: from 900 down at the slew rate, to the curve at 30 C.
EXP=300
settled=false
end=$(($(now_ms) + 40000))
while [ "$(now_ms)" -lt "$end" ]; do
  sample "5 after" || break
  if [ "$DUTY" -le "$EXP" ] && [ "$HELD" = false ]; then settled=true; break; fi
  sleep 0.25
done
expect "5 after the hold the duty falls at most 50 per second" fall_limit_ok '^5 '
expect "5 after the hold the duty settles at the curve (300 at 30 C)" $settled

# --- 6 failed sensor ----------------------------------------------------------------------------

EXP=$(expected 45000)
set_temp 45000
expect "6 back to 45 C on the curve before the fault" wait_until 25 at_expected
last=$DUTY
fault_t0=$(now_ms)
ctl off cpu-thermal > /dev/null || bad "6 off refused"
hold_ok=true
full_ok=true
end=$((fault_t0 + 8000))
while [ "$(now_ms)" -lt "$end" ]; do
  sleep 0.5
  elapsed=$(($(now_ms) - fault_t0))
  if ! sample "6 fault"; then
    hold_ok=false
    continue
  fi
  if [ "$elapsed" -le 2500 ] && [ "$DUTY" -ne "$last" ]; then hold_ok=false; fi
  if [ "$elapsed" -ge 4000 ] && { [ "$DUTY" -ne 1000 ] || [ "$DEGRADED" != true ]; }; then full_ok=false; fi
done
expect "6 the failed sensor holds the last duty for the first 2.5 s" $hold_ok
expect "6 from the fourth second the duty is 1000 and degraded" $full_ok
hv=$(health_of)
say "b14: health while the zone is off: $hv"
expect "6 the host reports the controller degraded or failed" bash -c '[[ "$1" == degraded || "$1" == failed ]]' _ "$hv"

ctl on cpu-thermal > /dev/null || bad "6 on refused"
# The duty falls to the curve at the slew rate: from 1000 to 450 is about 11 s.
expect "6 the zone back on: the duty comes back to the curve within 20 s" wait_until 20 recovered
expect "6 the zone back on: the controller is healthy" healthy

# --- 7 restart ----------------------------------------------------------------------------------

EXP=450
set_temp 45000
ctl call fan-control set_curve 0 "$(point 30000 900)$(point 75000 900)" > /dev/null \
  || bad "7 set_curve refused"
expect "7 the changed curve is in force (900 at 45 C)" wait_until 10 at_duty 900

if [ "$mode" = sim ]; then
  stop_sim
  start_sim
else
  "$restart_hook" || die "the restart hook failed"
  wait_until 30 ctl list > /dev/null 2>&1 || die "no service after the restart"
fi

# After the restart: the board file's curve, not the one that was set, and a fresh start.
expect "7 after the restart the board file's curve is in force (450 at 45 C)" wait_until 10 at_expected
expect "7 after the restart the controller is not held" state_not_held

say "b14: $pass passed, $fail failed; samples in $samples"
[ "$fail" -eq 0 ]
