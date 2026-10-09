#!/usr/bin/env bash
# Runs the feature-matrix job of .github/workflows/ci.yml locally: for each
# feature set, check, test and clippy the crate. Keep the list in sync with
# the workflow.
#
#   ./scripts/feature-matrix.sh            every entry
#   ./scripts/feature-matrix.sh linux      entries whose name contains "linux"
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

lite="linux-gpio,linux-gpio-cdev,linux-pwm,linux-i2c,linux-spi,linux-hotplug,builtin-drivers,tokio"
# name|package|features (empty: none beyond --no-default-features)
entries=(
  "lemnos default|lemnos|"
  "lemnos mock|lemnos|mock"
  "lemnos mock builtin-drivers|lemnos|mock,builtin-drivers"
  "lemnos mock builtin-drivers tokio|lemnos|mock,builtin-drivers,tokio"
  "lemnos linux|lemnos|linux"
  "lemnos linux builtin-drivers|lemnos|linux,builtin-drivers"
  "lemnos linux gpio hotplug|lemnos|linux-gpio,linux-hotplug"
  "lemnos linux lite|lemnos|$lite"
  "lemnos board mock|lemnos|board,mock,builtin-drivers"
  "lemnos board linux|lemnos|board,linux,builtin-drivers"
  "lemnos-core serde|lemnos-core|serde"
  "lemnos-board linux|lemnos-board|linux"
)

filter="${1:-}"
for entry in "${entries[@]}"; do
  IFS='|' read -r name package features <<<"$entry"
  [[ -n "$filter" && "$name" != *"$filter"* ]] && continue
  args=(-p "$package" --no-default-features ${features:+--features "$features"})
  echo "==> [matrix] $name"
  start=$(date +%s.%N)
  cargo check "${args[@]}"
  cargo test "${args[@]}"
  cargo clippy "${args[@]}" --all-targets -- -D warnings
  awk -v s="$start" -v e="$(date +%s.%N)" -v n="$name" \
    'BEGIN{printf "==> [time] %7.1fs  matrix: %s\n", e-s, n}'
done
