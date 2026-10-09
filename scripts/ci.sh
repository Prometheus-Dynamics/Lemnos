#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

# Runs one step and prints its wall time (`==> [time]` lines; a summary at
# the end lists them all).
timings=()
timed() {
  local label="$1"
  shift
  local start end
  start=$(date +%s.%N)
  "$@"
  end=$(date +%s.%N)
  local line
  line=$(awk -v s="$start" -v e="$end" -v l="$label" 'BEGIN{printf "%7.1fs  %s", e-s, l}')
  timings+=("$line")
  echo "==> [time] $line"
}

summary() {
  ((${#timings[@]})) || return 0
  echo "==> [time] summary"
  printf '  %s\n' "${timings[@]}"
}
trap summary EXIT

run_workspace() {
  echo "==> [workspace] Checking formatting"
  timed "workspace: Checking formatting" cargo fmt --check

  echo "==> [workspace] Checking file sizes"
  timed "workspace: Checking file sizes" "$root_dir/scripts/check-file-sizes.sh"

  echo "==> [workspace] Running tests"
  timed "workspace: Running tests" cargo test --workspace

  echo "==> [workspace] Running all-features workspace tests"
  timed "workspace: Running all-features workspace tests" cargo test --workspace --all-features

  echo "==> [workspace] Running all-targets all-features tests"
  timed "workspace: Running all-targets all-features tests" cargo test --workspace --all-targets --all-features

  echo "==> [workspace] Running clippy"
  timed "workspace: Running clippy" cargo clippy --workspace --all-targets --all-features -- -D warnings

  echo "==> [workspace] Building docs"
  timed "workspace: Building docs" cargo doc --workspace --no-deps
}

run_docs_and_lints() {
  echo "==> [docs-and-lints] Checking formatting"
  timed "docs-and-lints: Checking formatting" cargo fmt --check

  echo "==> [docs-and-lints] Checking file sizes"
  timed "docs-and-lints: Checking file sizes" "$root_dir/scripts/check-file-sizes.sh"

  echo "==> [docs-and-lints] Running default-feature clippy"
  timed "docs-and-lints: Running default-feature clippy" cargo clippy --workspace --all-targets -- -D warnings

  echo "==> [docs-and-lints] Running full-feature clippy"
  timed "docs-and-lints: Running full-feature clippy" cargo clippy --workspace --all-targets --all-features -- -D warnings

  echo "==> [docs-and-lints] Running full-feature tests"
  timed "docs-and-lints: Running full-feature tests" cargo test --workspace --all-targets --all-features

  echo "==> [docs-and-lints] Building docs"
  timed "docs-and-lints: Building docs" cargo doc --workspace --no-deps
}

run_nostd() {
  echo "==> [nostd] Building no_std crates for embedded and wasm targets"
  timed "nostd: Building no_std crates for embedded and wasm targets" "$root_dir/scripts/check-nostd.sh"
}

run_sizes() {
  echo "==> [sizes] Checking binary sizes against the baseline"
  timed "sizes: Checking binary sizes against the baseline" "$root_dir/scripts/check-sizes.sh"
}

run_matrix() {
  echo "==> [matrix] Running the feature matrix"
  timed "matrix: Running the feature matrix" "$root_dir/scripts/feature-matrix.sh"
}

run_package_surface() {
  echo "==> [package-surface] Validating package surface"
  timed "package-surface: Validating package surface" cargo package --workspace --allow-dirty --no-verify
}

usage() {
  cat <<'EOF'
Usage: ./scripts/ci.sh [workspace|docs-and-lints|nostd|sizes|matrix|package-surface|all]

Defaults to `all`, which mirrors the non-matrix jobs in `.github/workflows/ci.yml`;
`matrix` runs the feature-matrix job (`scripts/feature-matrix.sh`).
EOF
}

mode="${1:-all}"

case "$mode" in
  workspace)
    run_workspace
    ;;
  docs-and-lints)
    run_docs_and_lints
    ;;
  nostd)
    run_nostd
    ;;
  sizes)
    run_sizes
    ;;
  matrix)
    run_matrix
    ;;
  package-surface)
    run_package_surface
    ;;
  all)
    run_workspace
    run_docs_and_lints
    run_nostd
    run_sizes
    run_package_surface
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage
    exit 1
    ;;
esac
