#!/bin/bash
# slot-run.sh N cmd...: runs cmd on N CPUs of this machine's slot pool, taken for as long as it runs.
# The pool is SLOT_TOTAL lock files under SLOT_DIR (default 24 under /run/user/<uid>/slots); slot i is CPU
# SLOT_CPU0+i-1 (default 8, so CPUs 8-31: inside the machine's cap of 6-31). N is 1 to 8. A run takes the
# pool's TRAIN lock while it fills its slots, so two waiters never hold part of the pool each. The slots' locks stay open in
# cmd (exec), so they are released when cmd and every process that inherited them have ended.
set -uo pipefail
SLOT_DIR="${SLOT_DIR:-/run/user/$(id -u)/slots}"
SLOT_TOTAL="${SLOT_TOTAL:-24}"
SLOT_CPU0="${SLOT_CPU0:-8}"
n=${1:-}
[[ "$n" =~ ^[1-8]$ ]] && [ $# -ge 2 ] || { echo "usage: slot-run.sh N cmd... (N is 1 to 8)" >&2; exit 2; }
shift
mkdir -p "$SLOT_DIR"
exec {train}>"$SLOT_DIR/TRAIN"
# All N at once or none: a shortfall gives back what it took and tries again, so two waiters never hold
# part of the pool each.
cpus=()
while [ ${#cpus[@]} -lt "$n" ]; do
  cpus=(); fds=()
  flock "$train"
  for i in $(seq 1 "$SLOT_TOTAL"); do
    [ ${#cpus[@]} -ge "$n" ] && break
    exec {fd}>"$SLOT_DIR/slot.$i"
    if flock -n "$fd"; then cpus+=($((SLOT_CPU0 + i - 1))); fds+=("$fd"); else exec {fd}>&-; fi
  done
  flock -u "$train"
  if [ ${#cpus[@]} -lt "$n" ]; then
    for fd in "${fds[@]}"; do exec {fd}>&-; done
    sleep 2
  fi
done
exec {train}>&-
list=$(IFS=,; echo "${cpus[*]}")
exec taskset -c "$list" "$@"
