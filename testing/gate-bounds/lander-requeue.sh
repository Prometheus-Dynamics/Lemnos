#!/usr/bin/env bash
# Rehearsal of the lander's STARVED path (scripts/land/gate-attempts.sh), with stand-in gates and a temporary
# slot directory, so the real pool is never touched. Three paths, each asserted:
#   starved-once   attempt 1 exits STARVED (3), attempt 2 passes: the gate result is 0, both attempts are logged,
#                  the rerun ran with its slots held (the stand-in sees slot.1 locked), the queue is not stopped.
#   starved-twice  both attempts exit STARVED: the result is 3, the queue is stopped (marker written), a line for
#                  the coordinator line is printed, and only two attempts run.
#   red            a code red (exit 1) is not requeued: one attempt, result 1, no marker.
# Usage: testing/gate-bounds/lander-requeue.sh   (exit 0 when every path is as asserted)
set -uo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/lr.XXXXXX")"
trap 'rm -rf -- "$TMP"' EXIT
export SLOT_DIR="$TMP/slots" SLOT_TOTAL=4 LAND_RESERVE_SLOTS=2
mkdir -p "$SLOT_DIR"
for i in 1 2 3 4; do : >"$SLOT_DIR/slot.$i"; done
. "$repo/scripts/land/gate-attempts.sh"

# The stand-in gate: its mode says what each call does; it records whether slot.1 is held by someone else.
cat >"$TMP/standin-gate.sh" <<'SH'
#!/usr/bin/env bash
mode="$1"; count="$TMP_COUNT"; n=$(( $(cat "$count" 2>/dev/null || echo 0) + 1 )); echo "$n" >"$count"
if flock -n "$SLOT_DIR/slot.1" true; then held=no; else held=yes; fi
echo "stand-in gate: call $n, mode $mode, slot.1 held by another run: $held"
case "$mode:$n" in
  starved-once:1|starved-twice:*) echo "STARVED[stand-in-step]: stand-in-step failed while starved"; exit 3 ;;
  red:*) echo "stand-in-step failed: code red"; exit 1 ;;
  *) echo "gate passed"; exit 0 ;;
esac
SH
export TMP_COUNT="$TMP/count"

fails=0
run_path() {  # run_path <mode> <want-rc> <want-attempts> <want-queue-stop yes|no> <name>
  local mode="$1" want="$2" attempts="$3" stop="$4" name="$5" logdir rc out
  logdir="$TMP/$mode"; mkdir -p "$logdir"; rm -f "$TMP/count" "$TMP/QUEUE"
  export QUEUE_STOP_FILE="$TMP/QUEUE"
  out="$(gate_attempts "$logdir" bash "$TMP/standin-gate.sh" "$mode" 2>&1)"; rc=$?
  local calls; calls="$(cat "$TMP/count" 2>/dev/null || echo 0)"
  local got_stop=no; [ -s "$TMP/QUEUE" ] && got_stop=yes
  local ok=1
  [ "$rc" = "$want" ] || ok=0
  [ "$calls" = "$attempts" ] || ok=0
  [ "$got_stop" = "$stop" ] || ok=0
  printf '%s\n' "$out" | sed 's/^/    | /'
  if [ "$ok" = 1 ]; then echo "ok   $name (rc $rc, $calls attempt(s), queue stopped: $got_stop)"
  else echo "FAIL $name (rc $rc want $want; attempts $calls want $attempts; queue stopped $got_stop want $stop)"; fails=$((fails + 1)); fi
  if [ "$mode" = starved-once ]; then
    grep -q 'call 2.*slot.1 held by another run: yes' "$logdir/gate-attempt-2.log" && echo "ok   starved-once: the rerun ran with its slots held" \
      || { echo "FAIL starved-once: the rerun did not hold its slots"; fails=$((fails + 1)); }
    grep -q '== gate attempt 2' "$logdir/gate.log" && grep -q '== gate attempt 1' "$logdir/gate.log" \
      && echo "ok   starved-once: gate.log holds both attempts" || { echo "FAIL starved-once: gate.log lacks an attempt"; fails=$((fails + 1)); }
  fi
}

run_path starved-once 0 2 no "starved once then green: passes after one requeue"
run_path starved-twice 3 2 yes "starved twice: STARVED, queue stopped"
run_path red 1 1 no "code red: not requeued"

if [ "$fails" = 0 ]; then echo "lander-requeue: all paths as asserted"; exit 0; fi
echo "lander-requeue: $fails assertion(s) failed"; exit 1
