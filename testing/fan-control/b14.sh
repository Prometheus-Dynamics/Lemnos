#!/usr/bin/env bash
# The B14 fan-control rehearsal, in two phases. The FILE phase checks the controller's curve, slew, hold,
# sensor fault, disable, restart, refused writes and stop against a file-backed thermal zone: the script writes
# the zone's `temp` file in millidegrees, so every temperature is exact. The REAL phase runs the controller on
# the board's real thermal zone under a bounded CPU load, and checks the controller's law on what the controller
# itself read. Every check line says which zone it used: [file] or [real].
#
#   testing/fan-control/b14.sh sim [--out DIR]
#       Runs both phases on lemnosd --sim: the FILE phase on sim-board.toml (a thermal-zone on the script's
#       directory), then the REAL phase on sim-board-real.toml (a fake thermal zone standing in for the real
#       one, which the script's load stand-in drives). Needs lemnosd built with --features fake-devices.
#       This is every step of the board run, check 11's flap included.
#
#   testing/fan-control/b14.sh plants [--out DIR]
#       Runs the planted faults on the sim, one short run each, and passes only when each one turns its own
#       check red (or, for the zone that never settles, NOT MEASURED). See `plants` at the end of this file.
#
#   testing/fan-control/b14.sh board --phase file|real --socket PATH --board-file FILE
#       [--restart-hook CMD] [--stop-hook CMD] [--refuse-hook CMD] [--load-hook CMD]
#       --baseline-pwm N --baseline-enable N --baseline-state N [--out DIR]
#       Runs against the board's service, which must be running board.toml for --phase file, and
#       board-real.toml for --phase real. The run takes the board lock first (AGENTS.md).
#         --board-file FILE    the board file the service runs for this phase. The script reads the controller's
#                              curve, floor and slew from it: what the checks expect is the board file's, not a
#                              constant here.
#         --restart-hook CMD   restarts the service with the board file for the phase, and is called as
#                              `CMD file` or `CMD real`; the operator's hook switches the board file.
#         --stop-hook CMD      stops the service with SIGTERM and returns once the process has exited.
#         --refuse-hook CMD    check 11. `CMD on` makes the fan refuse its writes, its drop's included (on the
#                              board: a read-only bind mount of a copy of pwm1 over the fan's pwm1), and `CMD off`
#                              undoes it. Without it check 11 is NOT MEASURED.
#         --load-hook CMD      the bounded CPU load (the busybox board has no `timeout`, so the hook bounds
#                              the load itself). It is called with no argument and returns when the load is
#                              over. Used by the REAL phase only.
#       The three --baseline options are the fan's pwm1, pwm1_enable and its cooling device's cur_state,
#       recorded by the run before any write (B14 step 1). Build on NVMe or SSD, not the HDD.
#
# The tolerances, each derived here and in B14.md, none fitted to a run:
#   CURVE_TOL = 1 per mille. The controller's duty against the curve. The law interpolates in whole numbers and
#       drops the fraction (law.rs, Curve::eval), and this script rounds, so the two differ by at most 1.
#   FAN_TOL = half the fan's step. The fan's read-back against the commanded duty. The fan's `duty` item says
#       its step (`lemnos-ctl show fan`): the largest distance between two duties the fan can hold. hwmon-fan's
#       pwm1 has 255 levels, so its step is ceil(1000 / 255) = 4 per mille. A duty is written as the nearest
#       level and read back from it (hwmon-fan: pwm = round(duty * 255 / 1000), duty = round(pwm * 1000 / 255)),
#       so the read-back is within half a step of the commanded duty: 2 per mille. 300 reads back 302.
#   Slew. Over any two samples, t apart, the duty falls by at most SLEW * (t + CTRL_PERIOD_MS + SAMPLE_MS) / 1000
#       + 1. The controller steps once a period and each step falls by slew * (time since its last step)
#       (law.rs, next_duty), so the falls between two samples add up to slew * (the time from the last step
#       before the first sample to the last step before the second): at most t plus one period. SAMPLE_MS covers
#       the script's own read, and the 1 the law's whole-number fall. SLEW is the board file's slew_per_s.
#   Check 3's heat bound, CTRL_PERIOD_MS + SAMPLE_MS: one controller step to read the zone, then one read.
#       Rising duty is not slew-limited (lib.rs: "Rising is immediate").
#   Check 11's attempt gaps: at least the wait (1, 2, 4, then 8 s; law.rs, RETRY_FIRST_NS and RETRY_CAP_NS) less
#       one turn of the script's sampling loop, and at most the wait plus two controller periods and one turn
#       (each wait ends at the first step after it, and a fan that is bound again is written at the step after
#       the read that found it). An attempt is first seen at the sample after it, so each end of a gap is up to
#       one turn late: the turn is the longest one of the run, measured, and printed. Check 11's attempt count
#       over FLAP_S seconds: at most 4 + ceil((FLAP_S - 7) / 8): four attempts in the first 7 s (the waits of 1,
#       2 and 4 s), then one for each 8 s.
#   The REAL phase's zone scatter S is measured in the run: see `real_follows`.
#
# Environment:
#   LEMNOS_BIN   directory holding lemnosd and lemnos-ctl (default: ${CARGO_TARGET_DIR:-target}/debug).
#   TMPDIR       where the scratch directory and the socket go (sockets need short paths).
#
# Checks print PASS, FAIL or NOT MEASURED with their zone. NOT MEASURED is a check whose condition was not
# reached (the zone did not settle, no refuse hook): it is never a pass. Samples go to OUT/samples.tsv (the
# zone's reading, in millidegrees, is its eighth column on the REAL rows). Exit status: 0 only when every check
# passes; 1 when a check fails; 3 when none fails and one or more is not measured.
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
die() { echo "b14: $*" >&2; exit 2; }

mode=${1:-}
[ "$mode" = sim ] || [ "$mode" = board ] || [ "$mode" = plants ] \
  || die "usage: b14.sh sim|plants|board [options]; see the top of this file"
shift

socket=${LEMNOSD_SOCKET:-}
phase=
restart_hook=
stop_hook=
load_hook=
refuse_hook=
board_file=
plant=
scenario=full
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
    --refuse-hook) refuse_hook=${2:?--refuse-hook needs a command}; shift ;;
    --board-file) board_file=${2:?--board-file needs a file}; shift ;;
    --plant) plant=${2:?--plant needs a name}; shift ;;
    --scenario) scenario=${2:?--scenario needs full, curve or real}; shift ;;
    --baseline-pwm) base_pwm=${2:?--baseline-pwm needs a number}; shift ;;
    --baseline-enable) base_enable=${2:?--baseline-enable needs a number}; shift ;;
    --baseline-state) base_state=${2:?--baseline-state needs a number}; shift ;;
    --out) out=${2:?--out needs a directory}; shift ;;
    -h | --help) sed -n '2,/^set -u/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
  shift
done
if [ "$mode" = board ]; then
  { [ "$phase" = file ] || [ "$phase" = real ]; } || die "board mode needs --phase file or real"
  [ -r "$board_file" ] || die "board mode needs --board-file FILE, the board file the service runs for this phase"
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
# The controller's duty against the curve: the law drops the fraction and the script rounds.
CURVE_TOL=1
# Check 11: how long the refusal lasts, and how often the script lists the devices.
FLAP_S=60
LIST_MS=250
# The REAL phase: the zone has settled when its readings of the last SETTLE_MS span no more than its own
# scatter (see real_follows), and the script waits SETTLE_CAP_S for that after the load. The cap is a wait, not
# a tolerance: a zone that has not settled by then is NOT MEASURED.
SETTLE_MS=5000
SETTLE_CAP_S=${B14_SETTLE_CAP_S:-60}

pid=
pass=0
fail=0
notm=0
ZL=
say() { printf '%s\n' "$*" | tee -a "$log"; }
ok() { pass=$((pass + 1)); say "PASS $*"; }
bad() { fail=$((fail + 1)); say "FAIL $*"; }
not_measured() { notm=$((notm + 1)); say "NOT MEASURED $*"; }
expect() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }

# board_string FILE KEY and board_number FILE KEY: a setting of the fan-control device in a board file.
board_string() { sed -n '/^driver = "fan-control"/,$p' "$1" | sed -n "s/.*[{, ]$2 = \"\([^\"]*\)\".*/\1/p" | head -1; }
board_number() { sed -n '/^driver = "fan-control"/,$p' "$1" | sed -n "s/.*[{, ]$2 = \([0-9][0-9]*\).*/\1/p" | head -1; }
# read_board FILE: the controller's curve, floor and slew as the board file gives them, with the driver's
# defaults for the ones it leaves out (drivers/fan-control/src/lib.rs: floor 0, slew_per_s 50).
read_board() {
  CURVE_SPEC=$(board_string "$1" curve_t0)
  FLOOR=$(board_number "$1" floor)
  SLEW=$(board_number "$1" slew_per_s)
  [ -n "$CURVE_SPEC" ] || die "no curve_t0 for fan-control in $1"
  FLOOR=${FLOOR:-0}
  SLEW=${SLEW:-50}
  say "b14: from $(basename "$1"): curve $CURVE_SPEC, floor $FLOOR, slew $SLEW per second"
}
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
# fan_step: the step of the fan's duty item, as its driver describes it.
fan_step() { ctl show fan | awk '$1 == "duty" { for (i = 2; i < NF; i++) if ($i == "step") { print $(i + 1); exit } }'; }
# fan_matches: the fan's read-back is within half its step of the duty the controller commanded.
fan_matches() {
  local read
  state_now && read=$(fan_duty) && [ $((read - DUTY)) -ge $((-FAN_TOL)) ] && [ $((read - DUTY)) -le "$FAN_TOL" ]
}
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

at_expected() { state_now && [ $((DUTY - EXP)) -ge $((-CURVE_TOL)) ] && [ $((DUTY - EXP)) -le "$CURVE_TOL" ]; }
at_duty() { state_now && [ "$DUTY" -eq "$1" ]; }
degraded_full() { state_now && [ "$DEGRADED" = true ] && [ "$DUTY" -eq 1000 ]; }
recovered() { state_now && [ "$DEGRADED" = false ] && at_expected; }

# fall_limit_ok PATTERN: over any two samples whose label matches PATTERN, the duty falls by no more than the slew
# allows for the time between them and one controller period (the derivation is at the top of this file). A
# breach is written to the log with its two samples.
fall_limit_ok() {
  awk -F'\t' -v pat="$1" -v slew="$SLEW" -v lead=$((CTRL_PERIOD_MS + SAMPLE_MS)) '$2 ~ pat { n++; t[n] = $1; d[n] = $3 }
    END {
      for (i = 2; i <= n; i++) for (j = 1; j < i; j++) {
        allowed = int(slew * (t[i] - t[j] + lead) / 1000) + 1
        if (d[j] - d[i] > allowed) {
          printf "b14: slew: the duty fell %d in %d ms (%d to %d), and the slew allows %d\n", d[j] - d[i], t[i] - t[j], d[j], d[i], allowed
          exit 1
        }
      }
      exit 0
    }' "$samples" >> "$log"
}

# le32 N: N as four little-endian bytes in hex, the wire's form of an i32.
le32() { printf '%08x' $(($1 & 0xffffffff)) | sed -E 's/(..)(..)(..)(..)/\4\3\2\1/'; }
# point VALUE DUTY: one curve point as the wire has it: a length word of 8, then value and duty.
point() { printf '08000000%s%s' "$(le32 "$1")" "$(le32 "$2")"; }

# planted: the daemon's board file as a planted fault changes it (sim only). The script's own expectations are read
# from the file as it is in the repository, so a planted fault is a controller or a fan that does not do what the
# board file says.
planted() {
  case $plant in
    '' | no-settle) cat ;;
    # A curve point 5 per mille off: more than the fan's step, and more than CURVE_TOL.
    curve | real-curve) sed -e 's/60000:600/60000:605/' ;;
    # A fan whose read-back is 5 per mille off the duty it holds: more than half its step.
    fan-step) sed -e 's/levels = 255/levels = 255, read_offset = 5/' ;;
    # A controller that falls 400 per second where the board file says 50: the duty jumps.
    slew) sed -e 's/slew_per_s = 50/slew_per_s = 400/' ;;
    # A controller wired to a zone that does not move: its demand stays put while the zone the script reads moves.
    stale)
      sed -e 's/t0 = "cpu-thermal"/t0 = "frozen"/'
      printf '\n[[devices]]\nid = "frozen"\ndriver = "fake-thermal-zone"\nconfig = { path = "%s" }\n' "$work/frozen"
      ;;
    *) die "unknown plant $plant" ;;
  esac
}

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
    sed -e "s|@ZONE@|$zone_dir|" -e "s|@FAN@|$fan_file|" "$here/sim-board.toml" | planted > "$work/board.toml"
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

  # 2 curve. The curve, the floor and the slew are the board file's. The temperatures the script sets are its
  # own: the board's run stays inside what its curve spans (42 to 58 C).
  if [ "$mode" = sim ]; then
    read_board "$here/sim-board.toml"
    POINTS="30000 45000 60000 70000 75000 20000"
    HEAT=61000
    COOL=30000
  else
    read_board "$board_file"
    POINTS="42000 45000 50000 54000 56000"
    HEAT=56000
    COOL=42000
  fi
  MID=45000
  BASE_DUTY=702
  FAN_STEP=$(fan_step)
  case $FAN_STEP in '' | *[!0-9]*) die "the fan's duty item gives no step (lemnos-ctl show fan)" ;; esac
  FAN_TOL=$((FAN_STEP / 2))
  say "b14: the fan's duty step is $FAN_STEP per mille (its driver's description): the read-back is held to $FAN_TOL of the commanded duty"
  for t in $POINTS; do
    EXP=$(expected "$t")
    set_temp "$t"
    expect "$ZL 2 $t mC: duty $EXP within $CURVE_TOL of the curve" wait_until 25 at_expected
    expect "$ZL 2 $t mC: the fan's duty is within $FAN_TOL (half its step of $FAN_STEP) of the commanded duty" fan_matches
    say "b14: $ZL 2 $t mC: commanded $DUTY, the fan reads back $(fan_duty)"
    expect "$ZL 2 $t mC: the duty is at least the floor of $FLOOR" [ "$DUTY" -ge "$FLOOR" ]
  done
  if [ "$scenario" = curve ]; then
    [ "$mode" = sim ] && stop_service
    return
  fi

  # 3 heat: the duty reaches the curve within HEAT_BOUND_MS of the temperature passing the point.
  EXP=$(expected "$HEAT")
  set_temp "$HEAT"
  heat_t0=$(now_ms)
  heat_ok=false
  while [ $(($(now_ms) - heat_t0)) -le "$HEAT_BOUND_MS" ]; do
    if sample "3 heat" && [ $((DUTY - EXP)) -ge $((-CURVE_TOL)) ] && [ $((DUTY - EXP)) -le "$CURVE_TOL" ]; then
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
  expect "$ZL 4 cooling falls at most $SLEW per second" fall_limit_ok '^4 '
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
  expect "$ZL 5 after the hold the duty falls at most $SLEW per second" fall_limit_ok '^5 '
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

  # 11 refused writes and the flap. It runs in this service start, before the stop: a refused write must not cost
  # the controller its record of the fan, so check 12 is expected to pass after it. On the board it needs the
  # operator's --refuse-hook; without one it is NOT MEASURED and the script carries on to the stop.
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

# fan_readback: the duty the fan itself is at, read past the service: the fake's file on the sim, and on the board
# pwm1 as per mille with hwmon-fan's rounding (pwm_to_duty: round(pwm * 1000 / 255)).
fan_readback() {
  if [ "$mode" = sim ]; then
    cat "$fan_file"
  else
    local d pwm
    d=$(board_fan_dir) || return 1
    pwm=$(cat "$d/pwm1") || return 1
    echo $(((pwm * 1000 + 127) / 255))
  fi
}

# refuse on|off: the fan refuses its writes, its drop's included, as the board's does. On the sim that is the
# fake's markers; on the board it is the operator's hook.
refuse() {
  if [ "$mode" = sim ]; then
    if [ "$1" = on ]; then : > "$fan_file.refuse"; : > "$fan_file.refuse-drop"; else rm -f "$fan_file.refuse" "$fan_file.refuse-drop"; fi
  else
    "$refuse_hook" "$1"
  fi
}

# check 11: the fan's writes are refused for FLAP_S seconds, and what the board does is checked all through.
#
# What the board does (BOARD-4, and drivers/fan-control/tests/host.rs on the simulated clock): the controller's
# write is refused, and so is the release it asks for. The host counts those and binds the fan out at the fifth in
# a row, and its drop's write is refused too, so the fan stays where it was. Binding a hwmon fan makes no write, so
# the host binds it again about a second later (its own first wait), while the writes are still refused. Bound out,
# the controller's attempt is a read; the first read that works is the fan bound again, the write follows at the
# next step, it is refused, and after two more attempts the host binds the fan out again. The fan flaps, once in
# about 25 s, for as long as the refusal lasts. The controller's waits are 1, 2, 4 and then 8 s and stay at 8 s
# across every rebind: its attempt count never goes back.
#
# What must hold throughout:
#   - the duty the controller reports is the fan's own read-back, or is absent (the state read fails) while the
#     duty is unknown: never zero, and never a duty the fan is not at;
#   - the controller's health row names the state in every sample: degraded or failed, with its attempt count
#     and what failed, or that the fan is bound again;
#   - the attempts respect the backoff: each gap is at least its wait, and the count is within its bound;
#   - once the writes work again, control resumes on the curve and the controller is healthy.
check11() {
  if [ "$mode" = board ] && [ -z "$refuse_hook" ]; then
    not_measured "$ZL 11 refused writes and the flap: no --refuse-hook was given"
    return
  fi
  set_temp "$MID"
  EXP=$(expected "$MID")
  expect "$ZL 11 on the curve at $MID mC before the refusal" wait_until 25 at_expected
  local before failures_before=0
  before=$(fan_readback)
  [ "$mode" = sim ] && [ -f "$fan_file.failures" ] && failures_before=$(wc -l < "$fan_file.failures")
  refuse on || { bad "$ZL 11 the refusal could not be put in place"; return; }
  local t0 end row fan_row read n
  local known_seen=false absent_seen=false agree=true zero=false named=true failing=false
  local fan_in=true outs=0 rebinds=0 turn=0 last_turn
  local -A first_at=()
  t0=$(now_ms)
  last_turn=$t0
  end=$((t0 + FLAP_S * 1000))
  while [ "$(now_ms)" -lt "$end" ]; do
    # The longest turn of this loop: how late an attempt can be seen.
    [ $(($(now_ms) - last_turn)) -gt "$turn" ] && turn=$(($(now_ms) - last_turn))
    last_turn=$(now_ms)
    row=$(ctl list | grep "^fan-control " | head -1)
    fan_row=$(ctl list | grep "^fan " | head -1)
    if [[ "$row" =~ attempt\ ([0-9]+) ]]; then
      n=${BASH_REMATCH[1]}
      failing=true
      [ -z "${first_at[$n]:-}" ] && first_at[$n]=$(now_ms)
    fi
    # Once the first write has failed, the row says degraded or failed, with its reason, in every sample: the
    # attempt and what failed, or that the fan is bound again (between the read that found it and the next write).
    if $failing && ! [[ "$row" =~ (degraded|failed).*\((retrying:\ attempt\ [0-9]+|the\ fan\ is\ bound\ again) ]]; then
      named=false
      say "b14: $ZL 11 at $(($(now_ms) - t0)) ms the health row does not name the state: $row"
    fi
    if [[ "$fan_row" == *" running "* ]]; then
      $fan_in || { rebinds=$((rebinds + 1)); say "b14: $ZL 11 at $(($(now_ms) - t0)) ms the host bound the fan again"; }
      fan_in=true
    else
      $fan_in && { outs=$((outs + 1)); say "b14: $ZL 11 at $(($(now_ms) - t0)) ms the host bound the fan out: $fan_row"; }
      fan_in=false
    fi
    if state_now; then
      known_seen=true
      read=$(fan_readback)
      [ "$DUTY" -eq 0 ] && zero=true
      # Until its first write fails the controller reports the duty it commanded, which the fan holds to within
      # half its step. From then on it reports the fan's own read-back, which is the fan's duty exactly.
      local slack=$FAN_TOL
      $failing && slack=0
      if [ $((DUTY - read)) -lt $((-slack)) ] || [ $((DUTY - read)) -gt "$slack" ]; then
        agree=false
        say "b14: $ZL 11 mismatch at $(($(now_ms) - t0)) ms: the controller reports $DUTY, the fan reads $read (allowed $slack)"
      fi
      sample "11 fault" || break
    else
      absent_seen=true
    fi
    sleep "$(awk -v ms="$LIST_MS" 'BEGIN { printf "%.3f", ms / 1000 }')"
  done
  local after
  after=$(fan_readback)
  refuse off || bad "$ZL 11 the refusal could not be lifted"
  say "b14: $ZL 11 health row at the end of the refusal: ${row:-none}"
  local count=0 k
  for k in "${!first_at[@]}"; do count=$((count > k ? count : k)); done
  local ok=true
  for ((n = 1; n < count; n++)); do
    local w=$((1000 << (n - 1)))
    { [ "$n" -gt 4 ] || [ "$w" -gt 8000 ]; } && w=8000
    local gap=$(( ${first_at[$((n + 1))]:-0} - ${first_at[$n]:-0} ))
    local low=$((w - turn)) high=$((w + 2 * CTRL_PERIOD_MS + turn))
    say "b14: $ZL 11 attempt $n to $((n + 1)): $gap ms (wait $w ms; allowed $low to $high ms)"
    if [ "$gap" -lt "$low" ] || [ "$gap" -gt "$high" ]; then ok=false; fi
  done
  say "b14: $ZL 11 the longest turn of the sampling loop was $turn ms: an attempt is seen at most that late"
  local most=$((4 + (FLAP_S - 7 + 7) / 8))
  expect "$ZL 11 the fan flaps while its writes are refused: bound out and bound again, at least twice each in $FLAP_S s (out $outs, in $rebinds)" bash -c '[ "$1" -ge 2 ] && [ "$2" -ge 2 ]' _ "$outs" "$rebinds"
  expect "$ZL 11 the duty the controller reports is the fan's read-back whenever it is known" bash -c '[ "$1" = true ] && [ "$2" = true ]' _ "$known_seen" "$agree"
  expect "$ZL 11 the reported duty is never zero" bash -c '[ "$1" = false ]' _ "$zero"
  expect "$ZL 11 while the fan is bound out the duty is unknown: the state read fails" $absent_seen
  expect "$ZL 11 the health row names the state in every sample: degraded or failed, with the attempt or the rebind" $named
  expect "$ZL 11 the refused fan stays where it was: it reads $before before and $after after" [ "$before" = "$after" ]
  expect "$ZL 11 the controller retries past the cap: at least 6 attempts in $FLAP_S s ($count)" [ "$count" -ge 6 ]
  expect "$ZL 11 the fan is not hammered: at most $most attempts in $FLAP_S s ($count)" [ "$count" -le "$most" ]
  expect "$ZL 11 each attempt waits its turn: 1, 2, 4 and then 8 s, across every rebind" $ok
  if [ "$mode" = sim ]; then
    local writes=$(($(wc -l < "$fan_file.failures") - failures_before))
    expect "$ZL 11 the fan saw at most $most refused duty writes in $FLAP_S s ($writes)" [ "$writes" -le "$most" ]
  fi
  expect "$ZL 11 once the writes work again control resumes on the curve" wait_until 40 at_expected
  expect "$ZL 11 once the writes work again the controller is healthy" wait_until 20 healthy
}

# --- the REAL phase -----------------------------------------------------------------------------

# The sim's stand-in load: the fake zone's temperature climbs from 45 C to 62 C over 10 s, holds 4 s, and falls
# to 35 C in 2 s, where it stays. The fall is faster than the slew can follow (the curve falls 135 per mille a
# second there, the slew allows 50), as the board's is when its load stops, so the duty comes down on the slew.
# The board's load is the bounded load hook, which the sampler watches in the same way. With the `no-settle` plant
# the zone never holds still after the load: it goes on between 35 C and 45 C, a degree every quarter second.
sim_load() {
  local t
  for t in $(seq 45000 1000 62000); do set_zone "$t"; sleep 0.5; done
  sleep 4
  for t in $(seq 59000 -3000 35000); do set_zone "$t"; sleep 0.25; done
  if [ "$plant" = no-settle ]; then
    touch "$work/load-over"
    while :; do
      for t in $(seq 36000 1000 45000) $(seq 44000 -1000 35000); do set_zone "$t"; sleep 0.25; done
    done
  fi
}
set_zone() { printf '%s\n' "$1" > "$real_dir/temp"; }

# sample_real: one row for the REAL phase, with the zone's reading.
sample_real() {
  local t
  t=$(zone_mc) || return 1
  sample "r$1" "$t"
}

# The REAL phase's checks read the samples: one row per read, with the duty and the demand from one state read,
# and the script's own reading of the zone beside them.
#
# What the script's reading of the zone can be held to. The controller reads the zone itself, once a step, and the
# script's reading is another read at another time. On the board two reads of the zone a third of a second apart
# differed by up to 3.3 C (BOARD-4), so the duty cannot be held to the curve at the script's reading: BOARD-4's
# check did that, and failed on the zone's scatter, not on the controller. The controller's `demand` is the curve
# at the controller's own reading, and it comes in the same read as the duty. So the law is checked on the demand,
# exactly, and the demand is tied to the zone in two ways that need no fitted number (real_on_curve and
# real_follows).

# real_law: the controller's law at every sample, on the controller's own demand. The expected duty at a sample is
# the target (the demand, or the floor if that is higher) limited by the slew from the samples before it:
#   - never below the target: the fan never under-cools what the controller read;
#   - a rise is immediate, so the duty is above the target only while it falls;
#   - it falls by no more than the slew allows (as fall_limit_ok), and by no less: while the duty stands above
#     the target it falls SLEW for every second, so over a stretch of samples above the target, t long, it has
#     fallen at least SLEW * (t - CTRL_PERIOD_MS - SAMPLE_MS) / 1000 - 1. A duty that stays off the curve after
#     the slew has had time to catch up fails here.
real_law() {
  awk -F'\t' -v slew="$SLEW" -v floor="$FLOOR" -v lead=$((CTRL_PERIOD_MS + SAMPLE_MS)) '
    $2 ~ /^r/ && $5 == "false" && $6 == "false" { n++; t[n] = $1; d[n] = $3; g[n] = ($4 > floor ? $4 : floor) }
    END {
      if (n < 20) { print "b14: law: fewer than 20 samples"; exit 1 }
      for (i = 1; i <= n; i++) {
        if (d[i] < g[i]) { printf "b14: law: at %d ms the duty %d is below the target %d\n", t[i] - t[1], d[i], g[i]; exit 1 }
        above = (d[i] > g[i])
        for (j = i - 1; j >= 1; j--) {
          allowed = int(slew * (t[i] - t[j] + lead) / 1000) + 1
          if (d[j] - d[i] > allowed) {
            printf "b14: law: the duty fell %d in %d ms (%d to %d), and the slew allows %d\n", d[j] - d[i], t[i] - t[j], d[j], d[i], allowed
            exit 1
          }
          if (above) {
            least = int(slew * (t[i] - t[j] - lead) / 1000) - 1
            if (d[j] - d[i] < least) {
              printf "b14: law: the duty stood above its target for %d ms (%d to %d, target %d) and fell %d, less than the %d the slew gives\n", t[i] - t[j], d[j], d[i], g[i], d[j] - d[i], least
              exit 1
            }
            # The stretch above the target ends at the first earlier sample that was on it.
            if (d[j] <= g[j]) above = 0
          }
        }
      }
      exit 0
    }' "$samples" >> "$log"
}

# zone_stats: the zone's own figures in this run, from the script's readings: the quantum Q (the greatest common
# divisor of the differences between readings: the grid the zone reports on) and the scatter S (the most two
# readings one sample apart differed by).
zone_stats() {
  awk -F'\t' 'function gcd(a, b) { while (b) { x = a % b; a = b; b = x } return a }
    $2 ~ /^r/ && $8 != "" { n++; z[n] = $8 + 0 }
    END {
      q = 0; s = 0
      for (i = 2; i <= n; i++) {
        d = z[i] - z[1]; if (d < 0) d = -d; q = gcd(q, d)
        e = z[i] - z[i - 1]; if (e < 0) e = -e; if (e > s) s = e
      }
      printf "%d %d %d\n", q, s, z[1]
    }' "$samples"
}

# real_on_curve Q Z0: every demand is the board file's curve at a temperature the zone can report. The controller
# reads the same zone, so its reading is on the zone's grid (Z0 + k * Q), and its demand is the curve there, to
# within CURVE_TOL. Nothing here depends on when the controller read the zone, so the zone's scatter does not
# enter. A curve the controller holds that is not the board file's fails here.
real_on_curve() {
  awk -F'\t' -v spec="$CURVE_SPEC" -v q="$1" -v z0="$2" -v tol="$CURVE_TOL" 'function curve(t,   i) {
      if (t <= v[1]) return d[1]; if (t >= v[np]) return d[np];
      for (i = 1; i < np; i++) if (t <= v[i + 1]) return d[i] + (t - v[i]) * (d[i + 1] - d[i]) / (v[i + 1] - v[i]);
      return d[np] }
    BEGIN { np = split(spec, p, ","); for (i = 1; i <= np; i++) { split(p[i], c, ":"); v[i] = c[1] + 0; d[i] = c[2] + 0 } }
    $2 ~ /^r/ && $5 == "false" { n++; t[n] = $1; m[n] = $4 }
    END {
      # The grid points across the curve, one past each end (beyond them the curve is flat).
      first = z0 - q * int((z0 - v[1]) / q + 1); last = v[np] + q
      for (i = 1; i <= n; i++) {
        found = 0
        for (g = first; g <= last && !found; g += q) { at = curve(g); if (m[i] - at >= -tol && m[i] - at <= tol) found = 1 }
        if (!found) { printf "b14: curve: at %d ms the demand %d is not the curve at any temperature the zone reports (grid %d mC from %d)\n", t[i] - t[1], m[i], q, z0; exit 1 }
      }
      exit !(n >= 20)
    }' "$samples" >> "$log"
}

# real_follows S: the demand follows the zone in time. The controller's last read was at most one step before the
# script's sample, so it is held to the script's own readings of that last step (CTRL_PERIOD_MS + SAMPLE_MS back),
# widened by S either way: the demand lies between the curve at the least of those readings less S and the curve
# at the most of them plus S, to within CURVE_TOL. S is not a constant: it is the most two of the script's own
# readings one sample apart differed by in this run, which is how far one read of this zone was seen to be from
# the next. A controller that does not follow the zone (a demand that stays put while the zone moves) fails here.
real_follows() {
  awk -F'\t' -v spec="$CURVE_SPEC" -v s="$1" -v tol="$CURVE_TOL" -v back=$((CTRL_PERIOD_MS + SAMPLE_MS)) 'function curve(t,   i) {
      if (t <= v[1]) return d[1]; if (t >= v[np]) return d[np];
      for (i = 1; i < np; i++) if (t <= v[i + 1]) return d[i] + (t - v[i]) * (d[i + 1] - d[i]) / (v[i + 1] - v[i]);
      return d[np] }
    BEGIN { np = split(spec, p, ","); for (i = 1; i <= np; i++) { split(p[i], c, ":"); v[i] = c[1] + 0; d[i] = c[2] + 0 } }
    $2 ~ /^r/ && $5 == "false" && $8 != "" { n++; t[n] = $1; m[n] = $4; z[n] = $8 + 0 }
    END {
      for (i = 1; i <= n; i++) {
        lo = z[i]; hi = z[i]
        for (j = i - 1; j >= 1 && t[i] - t[j] <= back; j--) { if (z[j] < lo) lo = z[j]; if (z[j] > hi) hi = z[j] }
        if (m[i] < curve(lo - s) - tol || m[i] > curve(hi + s) + tol) {
          printf "b14: follows: at %d ms the demand %d is outside %d to %d, the curve at the zone readings of the last step (%d to %d mC) widened by the scatter %d mC\n", t[i] - t[1], m[i], curve(lo - s), curve(hi + s), lo, hi, s
          exit 1
        }
      }
      exit !(n >= 20)
    }' "$samples" >> "$log"
}
real_rose() { awk -F'\t' '$2 ~ /^r/ && $8 != "" { if (!n || $8 + 0 > hi) hi = $8 + 0; if (!n || $8 + 0 < lo) lo = $8 + 0; n++ } END { exit !(n && hi - lo >= 3000) }' "$samples"; }

# zone_settled S: the zone's readings of the last SETTLE_MS, all taken after the load, span no more than S.
zone_settled() {
  awk -F'\t' -v s="$1" -v win="$SETTLE_MS" '$2 == "rsettle" && $8 != "" { n++; t[n] = $1; z[n] = $8 + 0 }
    END {
      if (!n || t[n] - t[1] < win) exit 1
      lo = z[n]; hi = z[n]
      for (i = n; i >= 1 && t[n] - t[i] <= win; i--) { if (z[i] < lo) lo = z[i]; if (z[i] > hi) hi = z[i] }
      exit !(hi - lo <= s)
    }' "$samples"
}

# duty_held: over the last SETTLE_MS the duty is the target at every sample: the slew has caught up, and the duty
# is on the curve at what the controller read.
duty_held() {
  awk -F'\t' -v floor="$FLOOR" -v win="$SETTLE_MS" '$2 == "rsettle" { n++; t[n] = $1; d[n] = $3; g[n] = ($4 > floor ? $4 : floor) }
    END {
      if (!n) exit 1
      for (i = n; i >= 1 && t[n] - t[i] <= win; i--) if (d[i] != g[i]) exit 1
      exit 0
    }' "$samples"
}

phase_real() {
  ZL="[real]"
  say "b14: REAL phase: the controller reads the real thermal zone under the bounded load"
  if [ "$mode" = sim ]; then
    real_dir=$work/real
    fan_file=$work/fan
    socket=$work/lemnosd.sock
    mkdir -p "$real_dir" "$work/frozen"
    printf '45000\n' > "$real_dir/temp"
    printf '45000\n' > "$work/frozen/temp"
    sed -e "s|@FAN@|$fan_file|" -e "s|@REAL@|$real_dir|" "$here/sim-board-real.toml" | planted > "$work/board-real.toml"
    start_service "$work/board-real.toml"
    read_board "$here/sim-board-real.toml"
  else
    ctl list > /dev/null || die "no service answers on $socket"
    read_board "$board_file"
  fi
  describe=$(ctl describe fan-control 2>&1)
  expect "$ZL 1 fan-control describes duty and demand" bash -c '[[ $1 == *duty* && $1 == *demand* ]]' _ "$describe"
  expect "$ZL 1 fan-control is listed running and ok" bind_running
  expect "$ZL 1 the fan and the real zone are listed" devices_listed

  start_zone=$(zone_mc)
  say "b14: $ZL zone at the start of the load: $start_zone mC"

  # The sampler runs while the load does, one row per read. It stops when the load is over.
  rm -f "$work/load-over"
  (
    while [ ! -e "$work/load-over" ]; do
      sample_real "load" || true
      sleep 0.25
    done
  ) &
  sampler_pid=$!
  if [ "$mode" = sim ]; then
    sim_load &
    load_pid=$!
    if [ "$plant" = no-settle ]; then
      while [ ! -e "$work/load-over" ]; do sleep 0.25; done
    else
      wait "$load_pid"
      load_pid=
    fi
  else
    "$load_hook" || die "the load hook failed"
  fi
  touch "$work/load-over"
  wait "$sampler_pid" 2>/dev/null
  end_zone=$(zone_mc)
  say "b14: $ZL zone after the load: $end_zone mC"

  # After the load: sample until the zone has settled, SETTLE_CAP_S at most.
  local stats q s z0 settled=false held=false
  local cap_end=$(($(now_ms) + SETTLE_CAP_S * 1000))
  while [ "$(now_ms)" -lt "$cap_end" ]; do
    sample_real "settle" || true
    stats=$(zone_stats)
    s=${stats#* }; s=${s% *}
    if zone_settled "$s"; then
      settled=true
      if duty_held; then held=true; break; fi
    fi
    sleep 0.25
  done
  stats=$(zone_stats)
  q=${stats%% *}; s=${stats#* }; s=${s% *}; z0=${stats##* }
  say "b14: $ZL the zone in this run: it reports on a grid of $q mC, and two readings one sample apart differed by up to $s mC"

  expect "$ZL r2 the zone rose at least 3 C under the load" real_rose
  expect "$ZL r3 the law on the controller's own demand: never below the target, a rise at once, a fall of $SLEW per second and no more" real_law
  if [ "$q" -gt 1 ]; then
    expect "$ZL r4 every demand is the board file's curve at a temperature the zone reports (grid $q mC), within $CURVE_TOL" real_on_curve "$q" "$z0"
  else
    not_measured "$ZL r4 every demand is the board file's curve at a temperature the zone reports: the zone's readings show no grid coarser than 1 mC"
  fi
  expect "$ZL r5 the demand follows the zone: within the curve at the zone's readings of the last step, widened by the zone's own scatter ($s mC)" real_follows "$s"
  if $held; then
    ok "$ZL r6 held: the zone settled (its readings of the last $((SETTLE_MS / 1000)) s span at most $s mC) and the duty is the target at every sample of them"
  elif $settled; then
    bad "$ZL r6 held: the zone settled, and the duty did not reach its target in $SETTLE_CAP_S s"
  else
    not_measured "$ZL r6 held: the zone did not settle within $SETTLE_CAP_S s of the load's end (its readings of the last $((SETTLE_MS / 1000)) s still span more than $s mC)"
  fi
  if [ "$mode" = sim ]; then
    if [ -n "${load_pid:-}" ]; then kill -TERM "$load_pid" 2>/dev/null; wait "$load_pid" 2>/dev/null; load_pid=; fi
    stop_service
  fi
}

# --- the planted faults ---------------------------------------------------------------------------

# plants: each planted fault is one short sim run of this script, and it must turn its own check red (or, for the
# zone that never settles, NOT MEASURED) and exit with the code that goes with it. A plant that passes is a check
# that cannot fail, so this exits 0 only when every plant came out as stated.
#
#   curve       the controller's curve is 5 per mille off the board file's at 60 C: check 2 fails at that point.
#   fan-step    the fan reads back 5 per mille off the duty it holds, more than half its step: check 2's fan line fails.
#   slew        the controller falls 400 per second where the board file says 50: r3 fails (the duty jumps).
#   real-curve  the curve is 5 per mille off in the REAL phase: once the slew has caught up the duty is off the
#               board file's curve by more than a step, and r4 fails.
#   stale       the controller reads a zone that does not move, while the zone the script reads does: r5 fails.
#   no-settle   the zone never holds still after the load: r6 is NOT MEASURED, and the run is not a pass (exit 3).
plants() {
  local fails=0 name scn want code line rc
  local base=${out:-$work/plants}
  mkdir -p "$base"
  while read -r name scn code want; do
    B14_SETTLE_CAP_S=8 "$0" sim --plant "$name" --scenario "$scn" --out "$base/$name" > "$base/$name.txt" 2>&1
    rc=$?
    line=$(grep -m1 -E "$want" "$base/$name.txt")
    if [ "$rc" = "$code" ] && [ -n "$line" ]; then
      echo "RED $name: exit $rc; $line"
    else
      echo "NOT RED $name: exit $rc (expected $code), no line matching: $want; see $base/$name.txt"
      fails=$((fails + 1))
    fi
  done <<'PLANTS'
curve curve 1 ^FAIL \[file\] 2 60000 mC: duty 600 within 1 of the curve
fan-step curve 1 ^FAIL \[file\] 2 30000 mC: the fan's duty is within 2
slew real 1 ^FAIL \[real\] r3 the law
real-curve real 1 ^FAIL \[real\] r4 every demand
stale real 1 ^FAIL \[real\] r5 the demand follows
no-settle real 3 ^NOT MEASURED \[real\] r6 held
PLANTS
  if [ "$fails" = 0 ]; then
    echo "b14: every planted fault turned its own check red ($base)"
  else
    echo "b14: $fails planted fault(s) did not turn their check red ($base)"
    exit 1
  fi
}

# --- run ----------------------------------------------------------------------------------------

if [ "$mode" = plants ]; then
  plants
  exit 0
fi
if [ "$mode" = sim ]; then
  say "b14: sim, lemnosd from $bin, scratch $work${plant:+, planted fault: $plant}"
  case $scenario in
    full) phase_file; phase_real ;;
    curve) phase_file ;;
    real) phase_real ;;
    *) die "unknown scenario $scenario" ;;
  esac
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

say "b14: $pass passed, $fail failed, $notm not measured; samples in $samples; checks with the file-backed zone are [file], with the real zone [real]"
[ "$fail" -eq 0 ] || exit 1
[ "$notm" -eq 0 ] || exit 3
exit 0
