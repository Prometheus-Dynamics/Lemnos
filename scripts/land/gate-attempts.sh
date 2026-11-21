#!/bin/bash
# gate-attempts.sh: the train's gate with one requeue for a STARVED run. Sourced by l-train.sh; the test
# (testing/gate-bounds/lander-requeue.sh) sources it with stand-in gates.
#
# The gate exits GATE_STARVED_EXIT (3) with a STARVED[<step>] line when a step failed while starved (runnable
# and getting no CPU). That is no result, not a code red, so the gate runs once more, at the head of the queue,
# with its slots reserved before it starts. Starved is labelled and the rerun is the lander's own rule; a gate
# that is red on its own is never rerun. A second STARVED stops the queue: a marker file and a line for the coordinator.
# Nothing lands on a STARVED run: the caller treats any exit other than 0 as not landable.
GATE_STARVED_EXIT="${GATE_STARVED_EXIT:-3}"
SLOT_DIR="${SLOT_DIR:-/run/user/1000/slots}"
SLOT_TOTAL="${SLOT_TOTAL:-24}"
LAND_RESERVE_SLOTS="${LAND_RESERVE_SLOTS:-6}"

# reserve_and_run <n> <cmd...>: takes n pool slots the way train-hold.sh does (holds the TRAIN lock only while the
# slots fill, so slot-run hands out no new ones meanwhile), then runs cmd holding them. The slots are released
# when cmd ends.
reserve_and_run() {
  local want=$1 got=0 i fd train
  shift
  local -A held=()
  mkdir -p "$SLOT_DIR"
  exec {train}>"$SLOT_DIR/TRAIN"
  flock "$train"
  while [ "$got" -lt "$want" ]; do
    for i in $(seq 1 "$SLOT_TOTAL"); do
      [ "$got" -ge "$want" ] && break
      [ -n "${held[$i]:-}" ] && continue
      exec {fd}>"$SLOT_DIR/slot.$i"
      if flock -n "$fd"; then held[$i]=$fd; got=$((got + 1)); else exec {fd}>&-; fi
    done
    [ "$got" -lt "$want" ] && sleep 2
  done
  flock -u "$train"
  exec {train}>&-
  echo "requeue: reserved $got slot(s) before the rerun started"
  "$@"
  local rc=$?
  for i in "${!held[@]}"; do eval "exec ${held[$i]}>&-"; done
  return $rc
}

# gate_attempts <logdir> <cmd...>: runs the gate, attempt 1 into <logdir>/gate-attempt-1.log. On STARVED, attempt 2
# runs (reserved, into gate-attempt-2.log). <logdir>/gate.log is both attempts under headers, the file the lander
# reads. Returns 0 (passed), GATE_STARVED_EXIT (STARVED twice: queue stopped, a line for the coordinator) or the red status.
gate_attempts() {
  local logdir=$1 rc=0 rc1=0 a
  shift
  for a in 1 2; do
    if [ "$a" = 1 ]; then
      "$@" >"$logdir/gate-attempt-1.log" 2>&1; rc=$?; rc1=$rc
    else
      echo "requeue: attempt 1 was STARVED (no result); attempt 2 starts at the head of the queue with its slots reserved"
      reserve_and_run "$LAND_RESERVE_SLOTS" "$@" >"$logdir/gate-attempt-2.log" 2>&1; rc=$?
    fi
    [ "$rc" = "$GATE_STARVED_EXIT" ] || break
  done
  {
    echo "== gate attempt 1 (exit status $rc1)"
    cat "$logdir/gate-attempt-1.log"
    if [ -f "$logdir/gate-attempt-2.log" ]; then
      echo "== gate attempt 2 (exit status $rc)"
      cat "$logdir/gate-attempt-2.log"
    fi
  } >"$logdir/gate.log"
  if [ "$rc" = "$GATE_STARVED_EXIT" ]; then
    local line
    line="$(grep -hm1 -oE 'STARVED\[[^]]*\]' "$logdir/gate-attempt-2.log" "$logdir/gate-attempt-1.log" 2>/dev/null | head -1)"
    echo "STARVED TWICE: the gate was starved on both runs (${line:-no STARVED line}); the queue is stopped. For the coordinator: $(basename "$logdir") needs a quiet machine or a look at the load." >&2
    echo "STARVED TWICE $(date -Is) ${line:-}" >"${QUEUE_STOP_FILE:-/dev/null}"
  fi
  return "$rc"
}
