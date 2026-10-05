#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

run_workspace() {
  echo "==> [workspace] Checking formatting"
  cargo fmt --check

  echo "==> [workspace] Checking file sizes"
  "$root_dir/scripts/check-file-sizes.sh"

  echo "==> [workspace] Running tests"
  cargo test --workspace

  echo "==> [workspace] Running all-features workspace tests"
  cargo test --workspace --all-features

  echo "==> [workspace] Running all-targets all-features tests"
  cargo test --workspace --all-targets --all-features

  echo "==> [workspace] Running clippy"
  cargo clippy --workspace --all-targets --all-features -- -D warnings

  echo "==> [workspace] Building docs"
  cargo doc --workspace --no-deps
}

run_docs_and_lints() {
  echo "==> [docs-and-lints] Checking formatting"
  cargo fmt --check

  echo "==> [docs-and-lints] Checking file sizes"
  "$root_dir/scripts/check-file-sizes.sh"

  echo "==> [docs-and-lints] Running default-feature clippy"
  cargo clippy --workspace --all-targets -- -D warnings

  echo "==> [docs-and-lints] Running full-feature clippy"
  cargo clippy --workspace --all-targets --all-features -- -D warnings

  echo "==> [docs-and-lints] Running full-feature tests"
  cargo test --workspace --all-targets --all-features

  echo "==> [docs-and-lints] Building docs"
  cargo doc --workspace --no-deps
}

run_nostd() {
  echo "==> [nostd] Building no_std crates for embedded and wasm targets"
  "$root_dir/scripts/check-nostd.sh"
}

run_sizes() {
  echo "==> [sizes] Checking binary sizes against the baseline"
  "$root_dir/scripts/check-sizes.sh"
}

run_package_surface() {
  echo "==> [package-surface] Validating package surface"
  cargo package --workspace --allow-dirty --no-verify
}

usage() {
  cat <<'EOF'
Usage: ./scripts/ci.sh [workspace|docs-and-lints|nostd|sizes|package-surface|all]

Defaults to `all`, which mirrors the non-matrix jobs in `.github/workflows/ci.yml`.
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
