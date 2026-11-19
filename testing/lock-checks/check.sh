#!/usr/bin/env bash
# Tests the consumer-lock checkers (scripts/check-lemnos-pin.sh, scripts/check-shm-one-source.sh)
# against two fixture locks: one that passes both, and one that fails both and must list its strays.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
dir=testing/lock-checks
pin=0123456789abcdef0123456789abcdef01234567
fail=0

# run <wanted exit status> <text the output must hold> <command...>: prints the output, or reports a mismatch.
run() {
  local want=$1 text=$2 out status=0
  shift 2
  out=$("$@" 2>&1) || status=$?
  if [ "$status" != "$want" ] || ! grep -qF -- "$text" <<<"$out"; then
    echo "lock-checks: FAIL: '$*' exited $status (wanted $want) without '$text' in its output:" >&2
    echo "$out" >&2
    fail=1
  else
    echo "$out"
  fi
}

run 0 "lemnos-pin: PASS" scripts/check-lemnos-pin.sh "$dir/pinned.lock" "$pin"
run 1 "lemnos-ipc 2.0.1-dev from" scripts/check-lemnos-pin.sh "$dir/stray.lock" "$pin"
run 0 "shm-one-source: PASS" scripts/check-shm-one-source.sh "$dir/pinned.lock"
run 1 "more than one copy of:" scripts/check-shm-one-source.sh "$dir/stray.lock"
run 1 "shm-ring" scripts/check-shm-one-source.sh "$dir/stray.lock"

[ "$fail" = 0 ] || { echo "lock-checks: failed; see above" >&2; exit 1; }
echo "lock-checks: both checkers pass the pinned lock and fail the stray lock, listing its strays"
