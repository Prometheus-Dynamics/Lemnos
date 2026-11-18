#!/usr/bin/env bash
# The B14 fan-control rehearsal, in two phases. The FILE phase checks the controller's curve, slew, hold,
# sensor fault, disable, restart and stop against a file-backed thermal zone: the script writes the zone's
# `temp` file in millidegrees, so every temperature is exact. The REAL phase runs the controller on the
# board's real thermal zone under a bounded CPU load, and checks that the fan's duty follows the curve at the
# temperature the zone reports. Every check line says which zone it used: [file] or [real].
#
#   testing/fan-control/b14.sh sim [--out DIR]
#       Runs both phases on lemnosd --sim: the FILE phase on sim-board.toml (a thermal-zone on the script's
#       directory), then the REAL phase on sim-board-real.toml (a fake thermal zone standing in for the real
#       one, which the script's load stand-in drives). Needs lemnosd built with --features fake-devices.
#
#   testing/fan-control/b14.sh board --phase file|real --socket PATH [--restart-hook CMD] [--stop-hook CMD]
#       --baseline-pwm N --baseline-enable N --baseline-state N [--load-hook CMD] [--out DIR]
#       Runs against the board's service, which must be running board.toml for --phase file, and
#       board-real.toml for --phase real. The run takes the board lock first (AGENTS.md).
#         --restart-hook CMD   restarts the service with the board file for the phase, and is called as
#                              `CMD file` or `CMD real`; the operator's hook switches the board file.
#         --stop-hook CMD      stops the service with SIGTERM and returns once the process has exited.
#         --load-hook CMD      the bounded CPU load (the busybox board has no `timeout`, so the hook bounds
#                              the load itself). It is called with no argument and returns when the load is
#                              over. Used by the REAL phase only.
#       The three --baseline options are the fan's pwm1, pwm1_enable and its cooling device's cur_state,
#       recorded by the run before any write (B14 step 1). Build on NVMe or SSD, not the HDD.
#
# Check 11 (write failure, the bind-out and the retry schedule) runs on the sim only: a marker file
# (`<fan>.refuse`) makes the fake fan refuse the controller's writes, the host binds the fan out after five
# refusals, and the controller's health row names its attempts. On the board, check 11 is the hand step of
# B14.md, and this script says so.
#
# The heat latency (check 3) is bounded by the controller's own step period, which is the period at which
# it reads its zone and steps its duty (drivers/fan-control/src/law.rs, PERIOD_NS = 1 s), plus the script's own
# read interval. Rising duty is not slew-limited (drivers/fan-control/src/lib.rs: "Rising is immediate"), so no
# slew term applies to a heat. A cool is slew-limited (check 4), which is the fall limit.
#
# Environment:
#   LEMNOS_BIN   directory holding lemnosd and lemnos-ctl (default: ${CARGO_TARGET_DIR:-target}/debug).
#   TMPDIR       where the scratch directory and the socket go (sockets need short paths).
#
# Checks print PASS or FAIL with their zone. Samples go to OUT/samples.tsv (the zone's reading, in
# millidegrees, is its eighth column on the REAL rows). Exit status is 0 only when every check passes.
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
die() { echo "b14: $*" >&2; exit 2; }

mode=${1:-}
[ "$mode" = sim ] || [ "$mode" = board ] || die "usage: b14.sh sim|board [options]; see the top of this file"
shift

socket=${LEMNOSD_SOCKET:-}
phase=
restart_hook=
stop_hook=
load_hook=
base_pwm=
base_enable=
base_state=
out=
while [ $# -gt 0 ]; do
  case $1 in
    --socket) socket=${2:?--socket needs a path}; shift ;;
    --phase) phase=${2:?--phase needs file or real}; shift ;;
    --restart-hook) restart_hook=${2:?--restart-hook needs a command}; shift ;;
    --stop-hook) stop_hook=${2:?--stop-hook needs a command}; shift ;;
    --load-hook) load_hook=${2:?--load-hook needs a command}; shift ;;
    --baseline-pwm) base_pwm=${2:?--baseline-pwm needs a number}; shift ;;
    --baseline-enable) base_enable=${2:?--baseline-enable needs a number}; shift ;;
    --baseline-state) base_state=${2:?--baseline-state needs a number}; shift ;;
    --out) out=${2:?--out needs a directory}; shift ;;
    -h | --help) sed -n '2,33p' "$0"; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done
if [ "$mode" = board ]; then
  { [ "$phase" = file ] || [ "$phase" = real ]; } || die "board mode needs --phase file or real"
fi

bin=${LEMNOS_BIN:-${CARGO_TARGET_DIR:-$repo/target}/debug}
lemnosd=$bin/lemnosd
ctl_bin=$bin/lemnos-ctl
[ -x "$lemnosd" ] && [ -x "$ctl_bin" ] || die "no lemnosd and lemnos-ctl in $bin (set LEMNOS_BIN)"

work=$(mktemp -d "${TMPDIR:-/tmp}/b14.XXXXXX") || die "no scratch directory"
out=${out:-$work/out}
mkdir -p "$out"
samples=$out/samples.tsv
log=$out/run.log
: > "$log"
printf 'ms\tcheck\tduty\tdemand\tdegraded\theld\thealth\tzone_mC\n' > "$samples"

# The controller's constants (drivers/fan-control/src/law.rs) and the script's read interval.
CTRL_PERIOD_MS=1000
SAMPLE_MS=100
# Check 3's bound: one controller step to read the zone, then one read interval for the script to see it.
HEAT_BOUND_MS=$((CTRL_PERIOD_MS + SAMPLE_MS))

pid=
pass=0
fail=0
ZL=
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
  [ -n "${load_pid:-}" ] && kill -TERM "$load_pid" 2>/dev/null
}
trap cleanup EXIT

# The file phase's zone (the curve's temperature) is the script's `temp` file. The REAL phase's zone is the
# board's, and the sim's stand-in is written by the sim load.
zone_dir=
set_temp() {
  printf '%s\n' "$1" > "$zone_dir/temp"
}

# expected MILLIDEGREES: the curve's duty for that temperature, from CURVE_SPEC ("value:duty,...", as the
# board file's curve_t0 has it), linear between points, flat beyond the first and the last.
expected() {
  awk -v t="$1" -v spec="$CURVE_SPEC" 'BEGIN {
    n = split(spec, p, ",");
    for (i = 1; i <= n; i++) { split(p[i], q, ":"); v[i] = q[1] + 0; d[i] = q[2] + 0 }
    if (t <= v[1]) r = d[1];
    else if (t >= v[n]) r = d[n];
    else for (i = 1; i < n; i++) if (t <= v[i + 1]) { r = d[i] + (t - v[i]) * (d[i + 1] - d[i]) / (v[i + 1] - v[i]); break }
    printf "%d\n", r + 0.5
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

# The board's fan and its cooling device in sysfs (found by name and type, as the run's notes record).
board_fan_dir() {
  local d
  for d in /sys/class/hwmon/hwmon*; do
    [ "$(cat "$d/name" 2>/dev/null)" = pwmfan ] && { echo "$d"; return 0; }
  done
  return 1
}
board_cool_state() {
  local f
  for f in /sys/class/thermal/cooling_device*/type; do
    [ "$(cat "$f" 2>/dev/null)" = pwm-fan ] && { cat "$(dirname "$f")/cur_state"; return 0; }
  done
  return 1
}

# at_baseline: the fan is at the duty, mode and cooling state it had before the take-over. On the sim
# that is the fake fan's file (a duty); on the board it is pwm1, pwm1_enable and the cooling state.
at_baseline() {
  if [ "$mode" = sim ]; then
    [ "$(cat "$fan_file" 2>/dev/null)" = "$BASE_DUTY" ]
  else
    local d
    d=$(board_fan_dir) || return 1
    [ "$(cat "$d/pwm1" 2>/dev/null)" = "$base_pwm" ] \
      && [ "$(cat "$d/pwm1_enable" 2>/dev/null)" = "$base_enable" ] \
      && [ "$(board_cool_state 2>/dev/null)" = "$base_state" ]
  fi
}

# sample CHECK [ZONE_MC]: one read, written to samples.tsv with the time, the health and the zone's reading.
sample() {
  state_now || return 1
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(now_ms)" "$1" "$DUTY" "$DEMAND" "$DEGRADED" "$HELD" "$(health_of)" "${2:-}" >> "$samples"
}

# zone_mc: the real zone's reading in millidegrees, from the service (`ctl read`), or the file's.
zone_mc() { ctl read cpu-thermal temperature | awk '{ printf "%d\n", $3 * 1000 + 0.5 }'; }

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

# le32 N: N as four little-endian bytes in hex, the wire's form of an i32.
le32() { printf '%08x' $(($1 & 0xffffffff)) | sed -E 's/(..)(..)(..)(..)/\4\3\2\1/'; }
# point VALUE DUTY: one curve point as the wire has it: a length word of 8, then value and duty.
point() { printf '08000000%s%s' "$(le32 "$1")" "$(le32 "$2")"; }

start_service() { # start_service BOARD_FILE (sim only)
  "$lemnosd" --sim --board "$1" --socket "$socket" >> "$out/lemnosd.log" 2>&1 &
  pid=$!
  wait_until 15 ctl list > /dev/null 2>&1 || die "lemnosd --sim did not come up; see $out/lemnosd.log"
}

stop_service() {
  if [ -n "$pid" ]; then
    kill -TERM "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    pid=
  fi
}

# --- the FILE phase -----------------------------------------------------------------------------

phase_file() {
  ZL="[file]"
  say "b14: FILE phase: the curve, slew, hold, fault, disable, restart and stop checks use the file-backed zone"
  if [ "$mode" = sim ]; then
    zone_dir=$work/zone
    fan_file=$work/fan
    socket=$work/lemnosd.sock
    mkdir -p "$zone_dir"
    printf '45000\n' > "$zone_dir/temp"
    sed -e "s|@ZONE@|$zone_dir|" -e "s|@FAN@|$fan_file|" "$here/sim-board.toml" > "$work/board.toml"
    start_service "$work/board.toml"
  else
    zone_dir=/run/lemnos-b14/zone
    mkdir -p "$zone_dir" || die "cannot make $zone_dir"
    printf '45000\n' > "$zone_dir/temp"
    ctl list > /dev/null || die "no service answers on $socket"
  fi

  # 1 bind
  describe=$(ctl describe fan-control 2>&1)
  expect "$ZL 1 fan-control describes duty and demand" bash -c '[[ $1 == *duty* && $1 == *demand* ]]' _ "$describe"
  expect "$ZL 1 fan-control is listed running and ok" bind_running
  expect "$ZL 1 the fan and the zone are listed" devices_listed
  say "b14: fan-control health at bind is $(health_of)"

  # 2 curve
  CURVE_SPEC=30000:300,60000:600,75000:1000
  POINTS="30000 45000 60000 70000 75000 20000"
  HEAT=61000
  COOL=30000
  if [ "$mode" = board ]; then
    CURVE_SPEC=42000:300,50000:600,58000:1000
    POINTS="42000 45000 50000 54000 56000"
    HEAT=56000
    COOL=42000
  fi
  MID=45000
  BASE_DUTY=702
  for t in $POINTS; do
    EXP=$(expected "$t")
    set_temp "$t"
    expect "$ZL 2 $t mC: duty $EXP within 1 of the curve" wait_until 25 at_expected
    expect "$ZL 2 $t mC: the fan's duty equals the commanded duty" fan_matches
    expect "$ZL 2 $t mC: the duty is at least the floor of 300" [ "$DUTY" -ge 300 ]
  done

  # 3 heat: the duty reaches the curve within HEAT_BOUND_MS of the temperature passing the point.
  EXP=$(expected "$HEAT")
  set_temp "$HEAT"
  heat_t0=$(now_ms)
  heat_ok=false
  while [ $(($(now_ms) - heat_t0)) -le "$HEAT_BOUND_MS" ]; do
    if sample "3 heat" && [ $((DUTY - EXP)) -ge -1 ] && [ $((DUTY - EXP)) -le 1 ]; then
      heat_ok=true
      break
    fi
    sleep 0.1
  done
  expect "$ZL 3 heat to $HEAT mC: the duty reaches the curve within $HEAT_BOUND_MS ms ($CTRL_PERIOD_MS ms step + $SAMPLE_MS ms read)" $heat_ok
  say "b14: $ZL heat latency $(($(now_ms) - heat_t0)) ms (host clock, to the first read at the curve; bound $HEAT_BOUND_MS ms)"

  # 4 cool (slew)
  EXP=$(expected "$COOL")
  set_temp "$COOL"
  settled=false
  end=$(($(now_ms) + 40000))
  while [ "$(now_ms)" -lt "$end" ]; do
    sample "4 cool" || break
    if [ "$DUTY" -le "$EXP" ]; then settled=true; break; fi
    sleep 0.25
  done
  expect "$ZL 4 cooling falls at most 50 per second" fall_limit_ok '^4 '
  expect "$ZL 4 cooling settles at the curve's duty at $COOL mC" $settled

  # 5 hold
  ctl call fan-control hold 900 10000 > /dev/null || bad "$ZL 5 hold refused"
  state_now
  expect "$ZL 5 hold: the duty reads 900 at once, held" bash -c '[ "$1" -eq 900 ] && [ "$2" = true ]' _ "$DUTY" "$HELD"
  hold_t0=$(now_ms)
  end=$((hold_t0 + 13000))
  while [ "$(now_ms)" -lt "$end" ]; do
    sample "5 hold" || break
    [ "$HELD" = true ] || break
    sleep 0.25
  done
  held_span=$(awk -F'\t' '$2 == "5 hold" && $6 == "true" { if (!n) first = $1; last = $1; n++ } END { print n ? last - first : 0 }' "$samples")
  hold_ok() {
    awk -F'\t' -v span="$1" 'BEGIN { bad = 0 }
      $2 == "5 hold" && $6 == "true" && $3 != 900 { bad = 1 }
      END { exit !(span >= 9500 && !bad) }' "$samples"
  }
  expect "$ZL 5 hold: the duty is 900 and held for the 10 s" hold_ok "$held_span"
  EXP=$(expected "$COOL")
  settled=false
  end=$(($(now_ms) + 40000))
  while [ "$(now_ms)" -lt "$end" ]; do
    sample "5 after" || break
    if [ "$DUTY" -le "$EXP" ] && [ "$HELD" = false ]; then settled=true; break; fi
    sleep 0.25
  done
  expect "$ZL 5 after the hold the duty falls at most 50 per second" fall_limit_ok '^5 '
  expect "$ZL 5 after the hold the duty settles at the curve at $COOL mC" $settled

  # 6 failed sensor
  EXP=$(expected "$MID")
  set_temp "$MID"
  expect "$ZL 6 back to $MID mC on the curve before the fault" wait_until 25 at_expected
  last=$DUTY
  fault_t0=$(now_ms)
  ctl off cpu-thermal > /dev/null || bad "$ZL 6 off refused"
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
  expect "$ZL 6 the failed sensor holds the last duty for the first 2.5 s" $hold_ok
  expect "$ZL 6 from the fourth second the duty is 1000 and degraded" $full_ok
  hv=$(health_of)
  say "b14: $ZL health while the zone is off: $hv"
  expect "$ZL 6 the host reports the controller degraded or failed" bash -c '[[ "$1" == degraded || "$1" == failed ]]' _ "$hv"
  ctl on cpu-thermal > /dev/null || bad "$ZL 6 on refused"
  expect "$ZL 6 the zone back on: the duty comes back to the curve within 20 s" wait_until 20 recovered
  expect "$ZL 6 the zone back on: the controller is healthy" healthy

  # 9 disable
  ctl call fan-control set_enabled false > /dev/null || bad "$ZL 9 set_enabled(false) refused"
  expect "$ZL 9 disable: the fan is back at its baseline within a second" wait_until 1 at_baseline
  sleep 3
  expect "$ZL 9 disable: the fan stays at its baseline while disabled" at_baseline
  ctl call fan-control set_enabled true > /dev/null || bad "$ZL 9 set_enabled(true) refused"
  expect "$ZL 9 enabled again: the controller takes the fan back to the curve" wait_until 20 at_expected_mid

  # 7 restart
  set_temp "$MID"
  ctl call fan-control set_curve 0 "$(point 30000 900)$(point 75000 900)" > /dev/null \
    || bad "$ZL 7 set_curve refused"
  expect "$ZL 7 the changed curve is in force (900 at $MID mC)" wait_until 10 at_duty 900
  if [ "$mode" = sim ]; then
    stop_service
    start_service "$work/board.toml"
  else
    "$restart_hook" file || die "the restart hook failed"
    wait_until 30 ctl list > /dev/null 2>&1 || die "no service after the restart"
  fi
  expect "$ZL 7 after the restart the board file's curve is in force at $MID mC" wait_until 10 at_expected_mid
  expect "$ZL 7 after the restart the controller is not held" state_not_held

  # 11 write failure and its retry schedule. It runs in this service start, before the stop: a refused write
  # must not cost the controller its record of the fan, so check 12 is expected to pass after it. On the board
  # check 11 is the hand step (B14.md), so the script says so and carries on to the stop.
  check11

  # 12 stop
  if [ "$mode" = sim ]; then
    stop_service
  else
    "$stop_hook" || die "the stop hook failed"
  fi
  expect "$ZL 12 stop: the fan is back at its baseline once the service has exited" at_baseline
}

at_expected_mid() { EXP=$(expected "$MID"); at_expected; }

# no_zero_while_refused: no sample of check 11 reads a duty of zero (the safe state is full, never zero).
no_zero_while_refused() { ! awk -F'\t' '$2 == "11 fault" && $3 == 0 { found = 1 } END { exit !found }' "$samples"; }

# check 11: the fan's writes are refused for a while, and the retries are timed. The controller waits 1, 2, 4
# and 8 s between attempts and then stays at 8 s (law.rs, RETRY_FIRST_NS and RETRY_CAP_NS); each wait ends at the
# first controller step after it, so an interval is the wait or the wait plus up to one step.
check11() {
  if [ "$mode" != sim ]; then
    say "b14: $ZL 11 write failure: not run by the script on the board (B14.md, the bind-out step); its retry times are the operator's"
    return
  fi
  set_temp "$MID"
  # The fan refuses the controller's writes while the marker is there. Five refusals in a row bind the fan out
  # (the host's rule for a device that stops answering); the drop leaves full duty on it; the controller retries
  # on 1, 2, 4 and then 8 s until the fan is bound again, which happens once the marker is gone.
  : > "$fan_file.refuse"
  local t0 end now_row row="" bound_seen=false full_seen=false n
  local -A first_at=()
  t0=$(now_ms)
  ctl call fan-control hold 900 30000 > /dev/null || bad "$ZL 11 hold refused"
  end=$((t0 + 36000))
  while [ "$(now_ms)" -lt "$end" ]; do
    sample "11 fault" || break
    now_row=$(ctl list | grep "^fan-control " | head -1)
    [ -n "$now_row" ] && row=$now_row
    if [[ "$now_row" =~ attempt\ ([0-9]+) ]]; then
      n=${BASH_REMATCH[1]}
      [ -z "${first_at[$n]:-}" ] && first_at[$n]=$(now_ms)
    fi
    [[ "$now_row" == *"bound out"* ]] && bound_seen=true
    [ "$(cat "$fan_file")" = 1000 ] && full_seen=true
    sleep 0.25
  done
  rm -f "$fan_file.refuse"
  say "b14: $ZL 11 health row at the end of the fault: ${row:-none}"
  local count=0 k
  for k in "${!first_at[@]}"; do count=$((count > k ? count : k)); done
  local ok=true
  for ((n = 1; n < count; n++)); do
    local w=$((1000 << (n - 1)))
    [ "$w" -gt 8000 ] && w=8000
    local gap=$(( ${first_at[$((n + 1))]:-0} - ${first_at[$n]:-0} ))
    say "b14: $ZL 11 attempt $n to $((n + 1)): $gap ms (wait $w ms, bound $((w + CTRL_PERIOD_MS)) ms)"
    # Each attempt is first seen at a 250 ms sample, so each end of a gap is off by up to 250 ms; the first
    # controller step after the wait bounds the gap from above.
    if [ "$gap" -lt $((w - 500)) ] || [ "$gap" -gt $((w + CTRL_PERIOD_MS + 500)) ]; then ok=false; fi
  done
  expect "$ZL 11 the host binds the fan out, and the health row says so (bound out)" $bound_seen
  expect "$ZL 11 the fan is at full duty after the drop (the file reads 1000)" $full_seen
  expect "$ZL 11 the controller retries past the cap: at least 6 attempts in 36 s" [ "$count" -ge 6 ]
  expect "$ZL 11 the attempts wait 1, 2, 4 and then 8 s, each ended by the first step after it" $ok
  expect "$ZL 11 the duty while the fan is out is never zero" no_zero_while_refused
  EXP=$(expected "$MID")
  expect "$ZL 11 once the fan is bound again control resumes on the curve" wait_until 40 at_expected
}

# --- the REAL phase -----------------------------------------------------------------------------

# The sim's stand-in load: the fake zone's temperature climbs from 45 C to 62 C over 10 s, holds 4 s, and
# falls to 35 C over 10 s. The board's load is the bounded load hook, which the sampler watches in the same way.
sim_load() {
  local t
  for t in $(seq 45000 1000 62000); do set_zone "$t"; sleep 0.5; done
  sleep 4
  for t in $(seq 62000 -1000 35000); do set_zone "$t"; sleep 0.5; done
}
set_zone() { printf '%s\n' "$1" > "$real_dir/temp"; }

# sample_real: one row for the REAL phase, with the zone's reading.
sample_real() {
  local t
  t=$(zone_mc) || return 1
  sample "r$1" "$t"
}

# real_follows: no REAL sample's duty is below the curve's duty for the zone's readings of the last second (the
# fan never under-cools; the controller steps once a second). A fall is slew-limited (50 per mille a second), so a duty may stand above the curve while the
# zone falls; the rise is immediate. Once the zone has held still for the last 5 s of the run, the duty is on the
# curve, within 1 per mille.
real_follows() {
  awk -F'\t' -v spec="$CURVE_SPEC" 'function curve(t,   n, p, q, i, v, d) {
      n = split(spec, p, ","); for (i = 1; i <= n; i++) { split(p[i], q, ":"); v[i] = q[1] + 0; d[i] = q[2] + 0 }
      if (t <= v[1]) return d[1]; if (t >= v[n]) return d[n];
      for (i = 1; i < n; i++) if (t <= v[i + 1]) return d[i] + (t - v[i]) * (d[i + 1] - d[i]) / (v[i + 1] - v[i]);
      return d[n] }
    $2 ~ /^r/ { n++; duty[n] = $3; zone[n] = $8 }
    END {
      if (n < 20) exit 1
      for (i = 1; i <= n; i++) {
        # The controller steps once a second, so the duty is the curve at a reading up to one second old.
        m = curve(zone[i])
        for (j = (i - 4 > 1 ? i - 4 : 1); j <= i; j++) if (curve(zone[j]) < m) m = curve(zone[j])
        if (duty[i] < int(m + 0.5) - 1) exit 1
      }
      for (i = n - 19; i <= n; i++) {
        e = int(curve(zone[i]) + 0.5)
        if (zone[i] != zone[n]) exit 1
        if (duty[i] - e < -1 || duty[i] - e > 1) exit 1
      }
      exit 0
    }' "$samples"
}
real_rose() { awk -F'\t' '$2 ~ /^r/ && $8 != "" { if (!n || $8 + 0 > hi) hi = $8 + 0; if (!n || $8 + 0 < lo) lo = $8 + 0; n++ } END { exit !(n && hi - lo >= 3000) }' "$samples"; }

phase_real() {
  ZL="[real]"
  say "b14: REAL phase: the controller reads the real thermal zone under the bounded load"
  if [ "$mode" = sim ]; then
    real_dir=$work/real
    fan_file=$work/fan
    socket=$work/lemnosd.sock
    mkdir -p "$real_dir"
    printf '45000\n' > "$real_dir/temp"
    sed -e "s|@FAN@|$fan_file|" -e "s|@REAL@|$real_dir|" "$here/sim-board-real.toml" > "$work/board-real.toml"
    start_service "$work/board-real.toml"
  else
    ctl list > /dev/null || die "no service answers on $socket"
  fi
  describe=$(ctl describe fan-control 2>&1)
  expect "$ZL 1 fan-control describes duty and demand" bash -c '[[ $1 == *duty* && $1 == *demand* ]]' _ "$describe"
  expect "$ZL 1 fan-control is listed running and ok" bind_running
  expect "$ZL 1 the fan and the real zone are listed" devices_listed
  CURVE_SPEC=30000:300,60000:600,75000:1000
  if [ "$mode" = board ]; then
    CURVE_SPEC=42000:300,50000:600,58000:1000
  fi

  start_zone=$(zone_mc)
  say "b14: $ZL zone at the start of the load: $start_zone mC"

  # The sampler runs while the load does, one row per read, and the load is bounded.
  (
    end=$(($(now_ms) + 60000))
    while [ "$(now_ms)" -lt "$end" ]; do
      sample_real "load" || true
      sleep 0.25
    done
  ) &
  sampler_pid=$!
  if [ "$mode" = sim ]; then
    sim_load &
    load_pid=$!
    wait "$load_pid"
    load_pid=
  else
    "$load_hook" || die "the load hook failed"
  fi
  wait "$sampler_pid" 2>/dev/null
  expect "$ZL r2 the duty never falls below the curve at the zone's reading, and is on the curve once the zone holds" real_follows
  expect "$ZL r2 the zone rose at least 3 C under the load" real_rose
  end_zone=$(zone_mc)
  say "b14: $ZL zone after the load: $end_zone mC"
  expect "$ZL r3 the duty falls at most 50 per second after the load" fall_limit_ok '^rload'
  if [ "$mode" = sim ]; then
    stop_service
  fi
}

# --- run ----------------------------------------------------------------------------------------

if [ "$mode" = sim ]; then
  say "b14: sim, lemnosd from $bin, scratch $work"
  phase_file
  phase_real
else
  [ -n "$socket" ] || die "board mode needs --socket PATH (or LEMNOSD_SOCKET)"
  [ -n "$restart_hook" ] || [ "$phase" = real ] || die "board mode file phase needs --restart-hook CMD"
  [ -n "$stop_hook" ] || [ "$phase" = real ] || die "board mode file phase needs --stop-hook CMD"
  [ "$phase" = file ] && { [ -n "$base_pwm" ] && [ -n "$base_enable" ] && [ -n "$base_state" ] \
    || die "board mode file phase needs --baseline-pwm, --baseline-enable and --baseline-state (recorded before any write)"; }
  [ "$phase" = real ] && [ -z "$load_hook" ] && die "board mode real phase needs --load-hook CMD (the bounded load)"
  exec 9> /run/lock/board-gpu.lock
  flock -n 9 || die "the board lock /run/lock/board-gpu.lock is held elsewhere"
  say "b14: board, phase $phase, socket $socket"
  if [ "$phase" = file ]; then phase_file; else phase_real; fi
fi

say "b14: $pass passed, $fail failed; samples in $samples; checks with the file-backed zone are [file], with the real zone [real]"
[ "$fail" -eq 0 ]
