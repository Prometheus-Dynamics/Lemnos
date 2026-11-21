#!/usr/bin/env bash
# Proves the gate's step bounds (scripts/gate-bounds.sh): a slow step that keeps making progress passes even when
# it is starved; a step that hangs, or spins past its CPU budget, fails and is named. Every case runs one step
# through the same unit and finish the gate uses, and checks the verdict and the words the gate prints.
#
# Contention is single-core only. The case under test and its competitors are pinned to one CPU (CORE, the last
# CPU this shell may use), so the case gets a fair share of that one core and nothing else is loaded. Each case
# starts CONTEND competitors (default 2) that spin on CORE for CONTEND_SECONDS (default 20) and no longer; they
# are stopped by PID on EXIT, never waited on. No process outside this script is started or stopped. Each case
# records the load average before and after, so the result says what the machine was doing.
#
# GATE_BOUNDS_LIB names another copy of the library to test (the fail-before runs use the wall-clock library
# from origin/next through scripts/gate.sh's old bounded()).
set -euo pipefail
# One CPU: the last one this shell may run on. The script re-runs itself pinned to it, so every child (the case,
# its competitors and cargo) inherits the pin.
if [ -z "${GB_PINNED:-}" ]; then
  core="$(taskset -pc $$ | sed 's/.*: //' | tr ',' '\n' | tail -1 | sed 's/-.*//')"
  GB_PINNED=1 CORE="$core" exec taskset -c "$core" "$0" "$@"
fi
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LIB="${GATE_BOUNDS_LIB:-$repo/scripts/gate-bounds.sh}"
CONTEND="${CONTEND:-2}"
CONTEND_SECONDS="${CONTEND_SECONDS:-20}"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/gb.XXXXXX")"
competitor_pids=()
flock_pid=0
cleanup() {
  local p
  for p in "${competitor_pids[@]}" "$flock_pid"; do [ "$p" = 0 ] || kill -KILL "$p" 2>/dev/null || true; done
  rm -rf -- "$TMP"
}
trap cleanup EXIT


# The gate's own state, as gate.sh sets it: the library uses these names.
gate_out="$TMP/out"; mkdir -p "$gate_out"
stop_lanes() { :; }
step_dir() { echo "$TMP/s$1"; }
ccache_names() { :; }
ccache_growth() { :; }
step_tmp_check() { rm -rf -- "$2"; }   # gate.sh's own check is not under test here
GATE_KILL_GRACE=1
GATE_STALL_SAMPLES=3
export GATE_KILL_GRACE GATE_STALL_SAMPLES
. "$LIB"

# Stand-ins: a CPU burn of a given CPU-seconds, a burn in pulses with sleeps between.
cat >"$TMP/burn.py" <<'PY'
import sys, time
want = float(sys.argv[1]); t = time.process_time()
while time.process_time() - t < want:
    pass
PY
cat >"$TMP/pulse.py" <<'PY'
import time
for _ in range(8):
    t = time.process_time()
    while time.process_time() - t < 0.3:
        pass
    time.sleep(1)
PY

# start_competitors: CONTEND spinners, each for CONTEND_SECONDS of its own clock (PIDs recorded; they share CORE).
start_competitors() {
  local i n="${1:-$CONTEND}"
  for i in $(seq "$n"); do
    python3 -c "import time,sys; t=time.time()
while time.time()-t < float(sys.argv[1]): pass" "$CONTEND_SECONDS" &
    competitor_pids+=($!)
  done
}
stop_competitors() {
  local p
  for p in "${competitor_pids[@]}"; do kill -KILL "$p" 2>/dev/null || true; done
  competitor_pids=()
}

fails=0
# case <name> <pass|fail> <needle> <budget> <command...>: runs the step pinned to CORE with competitors on CORE.
# "pass" needs rc 0 and "fail" needs rc nonzero; the output must contain <needle> ("-" for none).
case_run() {
  local name="$1" want="$2" needle="$3" budget="$4" out rc=0 la_before la_after
  shift 4
  la_before="$(cut -d' ' -f1-3 /proc/loadavg)"
  start_competitors
  out="$( ( unit "$name" "$name" "$budget" "$@" || true; finish "$name" ) 2>&1 )" || rc=$?
  stop_competitors
  la_after="$(cut -d' ' -f1-3 /proc/loadavg)"
  printf '%s\n' "$out" | sed 's/^/    | /'
  local ok=1
  if [ "$want" = pass ] && [ "$rc" != 0 ]; then ok=0; fi
  if [ "$want" = fail ] && [ "$rc" = 0 ]; then ok=0; fi
  if [ "$needle" != - ] && ! printf '%s\n' "$out" | grep -qF -- "$needle"; then ok=0; fi
  if [ "$ok" = 1 ]; then echo "ok   $name (rc $rc; core $CORE, $CONTEND competitors; load $la_before -> $la_after)"
  else echo "FAIL $name (rc $rc, want $want, needle: $needle; core $CORE; load $la_before -> $la_after)"; fails=$((fails + 1)); fi
}

# starved_case: a stand-in that fails after 1 CPU-s, run at nice 19 against 4 nice-0 competitors on CORE. It is
# runnable and gets almost no CPU, so most looks find it starved. The gate must print STARVED[starved-step] and
# exit with GATE_STARVED_EXIT (3): a label, not a pass, and not a code red.
starved_case() {
  local out rc=0 la_before la_after
  cat >"$TMP/burnfail.py" <<'PY'
import sys, time
want = 1.0; t = time.process_time()
while time.process_time() - t < want:
    pass
sys.exit(1)
PY
  la_before="$(cut -d' ' -f1-3 /proc/loadavg)"
  start_competitors 4
  out="$( ( unit starved-step starved-step 600 nice -n 19 python3 "$TMP/burnfail.py" || true; finish starved-step ) 2>&1 )" || rc=$?
  stop_competitors
  la_after="$(cut -d' ' -f1-3 /proc/loadavg)"
  printf '%s\n' "$out" | sed 's/^/    | /'
  if [ "$rc" = "$GATE_STARVED_EXIT" ] && printf '%s\n' "$out" | grep -qF -- "STARVED[starved-step]"; then
    echo "ok   starved-step (rc $rc = GATE_STARVED_EXIT; core $CORE, 4 competitors; load $la_before -> $la_after)"
  else
    echo "FAIL starved-step (rc $rc, want $GATE_STARVED_EXIT with STARVED[starved-step]; core $CORE; load $la_before -> $la_after)"; fails=$((fails + 1))
  fi
}

echo "== stand-ins (core $CORE, $CONTEND competitors on it)"
case_run slow-progressing pass "CPU-s" 60 python3 "$TMP/burn.py" 6
case_run pulsed-progressing pass "CPU-s" 60 python3 "$TMP/pulse.py"
case_run hang fail "stalled: no CPU progress" 600 sleep 600
case_run spin-over-budget fail "exceeded its CPU budget of 3 CPU-s" 3 python3 -c 'while True: pass'
# Short children, each gone before the next look: 20 burns of 0.5 CPU-s are 10 CPU-s in all, over a budget of 5.
case_run short-children-over-budget fail "exceeded its CPU budget of 5 CPU-s" 5 bash -c 'for i in $(seq 20); do python3 "$1" 0.5; done' _ "$TMP/burn.py"
starved_case

echo "== real commands (core $CORE)"
case_run cargo-metadata pass "CPU-s" 180 cargo metadata --locked --format-version 1 --manifest-path "$repo/Cargo.toml"
# A real cargo waits on the build directory's lock: blocked on a lock it uses no CPU and is runnable on nothing,
# so it is stalled. The lock is held by a flock this script starts, and stopped by PID on EXIT.
mkdir -p "$TMP/target/debug"
flock -x "$TMP/target/debug/.cargo-lock" -c 'sleep 600' & flock_pid=$!
sleep 1
case_run cargo-blocked-on-lock fail "stalled: no CPU progress" 600 env CARGO_TARGET_DIR="$TMP/target" cargo check -q --locked --manifest-path "$repo/Cargo.toml" -p lemnos-abi
kill -KILL "$flock_pid" 2>/dev/null || true; flock_pid=0

if [ "$fails" = 0 ]; then echo "gate-bounds: all cases as expected"; exit 0; fi
echo "gate-bounds: $fails case(s) not as expected"; exit 1
