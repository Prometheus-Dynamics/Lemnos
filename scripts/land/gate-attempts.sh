#!/bin/bash
# gate-attempts.sh: the train's gate, run once. Sourced by l-train.sh; testing/gate-lanes/check.sh sources it with
# stand-in gates.
#
# There is no second attempt. A step that failed while the machine starved it, or that stalled, is a red like any
# other: the gate's log names it with its evidence (the load, the run-queue wait, and for a stall what each process
# was blocked on), and the train stops. Running it again until it passes would hide the cause.

# gate_attempts <logdir> <cmd...>: runs the gate once, its output in <logdir>/gate.log (the file the lander reads),
# and returns its status.
gate_attempts() {
  local logdir=$1
  shift
  "$@" >"$logdir/gate.log" 2>&1
}
