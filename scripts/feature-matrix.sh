#!/usr/bin/env bash
# Runs the feature-matrix job of .github/workflows/ci.yml locally: for each
# feature set, clippy over all targets, and the tests for `test` entries.
# Keep the list in sync with the workflow.
#
#   ./scripts/feature-matrix.sh                 every entry
#   ./scripts/feature-matrix.sh "lemnos linux"  the entry with exactly that name
#   ./scripts/feature-matrix.sh linux           else, entries whose name contains it
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

lite="linux-gpio,linux-gpio-cdev,linux-pwm,linux-i2c,linux-spi,linux-hotplug,builtin-drivers,tokio"
# name|package|features|mode. Features: empty means none beyond
# --no-default-features. Mode `test` runs the tests and clippy; `lint` runs
# clippy over all targets only, which still compiles every test, example and
# bench (type-checked, not code-generated) for that feature set. Entries are
# `lint` when their feature set is a subset of a `test` entry here or of the
# workspace's all-features test run and changes no test's behaviour.
entries=(
  "lemnos default|lemnos||test"
  "lemnos mock|lemnos|mock|lint"
  "lemnos mock builtin-drivers|lemnos|mock,builtin-drivers|lint"
  "lemnos mock builtin-drivers tokio|lemnos|mock,builtin-drivers,tokio|lint"
  "lemnos linux|lemnos|linux|test"
  "lemnos linux builtin-drivers|lemnos|linux,builtin-drivers|lint"
  "lemnos linux gpio hotplug|lemnos|linux-gpio,linux-hotplug|lint"
  "lemnos linux lite|lemnos|$lite|test"
  "lemnos board mock|lemnos|board,mock,builtin-drivers|test"
  "lemnos board linux|lemnos|board,linux,builtin-drivers|lint"
  "lemnos-core serde|lemnos-core|serde|test"
  "lemnos-board linux|lemnos-board|linux|test"
)

export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
filter="${1:-}"
exact=false
for entry in "${entries[@]}"; do
  [[ "${entry%%|*}" == "$filter" ]] && exact=true
done
for entry in "${entries[@]}"; do
  IFS='|' read -r name package features mode <<<"$entry"
  if $exact; then
    [[ "$name" != "$filter" ]] && continue
  elif [[ -n "$filter" && "$name" != *"$filter"* ]]; then
    continue
  fi
  args=(-p "$package" --no-default-features ${features:+--features "$features"})
  echo "==> [matrix] $name ($mode)"
  start=$(date +%s.%N)
  # clippy type-checks everything `cargo check` would: no separate check.
  cargo clippy "${args[@]}" --all-targets -- -D warnings
  if [[ "$mode" == test ]]; then
    cargo test "${args[@]}"
  fi
  awk -v s="$start" -v e="$(date +%s.%N)" -v n="$name" \
    'BEGIN{printf "==> [time] %7.1fs  matrix: %s\n", e-s, n}'
done
