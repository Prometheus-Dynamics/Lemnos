#!/usr/bin/env bash
# The check every PR into next must pass. CI runs exactly this.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

step() {
  local label="$1" start
  shift
  echo "==> $label"
  start=$SECONDS
  "$@"
  echo "    $label: $((SECONDS - start)) s"
}

step format cargo fmt --all --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step tests cargo test --workspace

# Added by PR A2.
if [[ -x scripts/check-boundaries.sh ]]; then
  step boundaries scripts/check-boundaries.sh
fi

echo "gate passed"
