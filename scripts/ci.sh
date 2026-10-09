#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

# A CI run starts from a clean or cached target dir and never edits sources
# between builds: incremental compilation only adds work and disk there.
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# Runs one step and prints its wall and CPU time (`==> [time]` lines; a
# summary at the end lists them all). CPU time is the work itself; wall time
# also counts waiting on disk and network.
timings=()
# Sets cpu_now to the CPU seconds (user + system) used by finished child
# processes so far. (`times` must run in this shell, not in a subshell.)
times_file="$(mktemp)"
child_cpu() {
  times >"$times_file"
  cpu_now=$(awk 'NR==2 {
    n = split($0, f, " "); s = 0
    for (i = 1; i <= n; i++) { split(f[i], p, "m"); s += p[1] * 60 + p[2] }
    print s }' "$times_file")
}

timed() {
  local label="$1"
  shift
  local start end cpu0 cpu1
  start=$(date +%s.%N)
  child_cpu
  cpu0=$cpu_now
  "$@"
  end=$(date +%s.%N)
  child_cpu
  cpu1=$cpu_now
  local line
  line=$(awk -v s="$start" -v e="$end" -v c0="$cpu0" -v c1="$cpu1" -v l="$label" \
    'BEGIN{printf "%7.1fs wall %8.1fs cpu  %s", e-s, c1-c0, l}')
  timings+=("$line")
  echo "==> [time] $line"
}

summary() {
  rm -f "$times_file"
  ((${#timings[@]})) || return 0
  echo "==> [time] summary"
  printf '  %s\n' "${timings[@]}"
}
trap summary EXIT

# The workspace job: every test, plus the full-feature lints the tests'
# build shares. `cargo test --all-targets` skips doctests, so they run on
# their own; together the two all-features runs cover what `cargo test
# --workspace --all-features` would.
run_workspace() {
  echo "==> [workspace] Running tests"
  timed "workspace: tests (default features)" cargo test --workspace

  echo "==> [workspace] Running all-targets all-features tests"
  timed "workspace: tests (all features, all targets)" cargo test --workspace --all-targets --all-features

  echo "==> [workspace] Running all-features doctests"
  timed "workspace: doctests (all features)" cargo test --workspace --all-features --doc

  echo "==> [workspace] Running clippy"
  timed "workspace: clippy (all features)" cargo clippy --workspace --all-targets --all-features -- -D warnings
}

# The docs-and-lints job: formatting, file sizes, default-feature clippy and
# docs (the full-feature clippy and tests are the workspace job's).
run_docs_and_lints() {
  echo "==> [docs-and-lints] Checking formatting"
  timed "docs-and-lints: fmt" cargo fmt --check

  echo "==> [docs-and-lints] Checking file sizes"
  timed "docs-and-lints: file sizes" "$root_dir/scripts/check-file-sizes.sh"

  echo "==> [docs-and-lints] Running default-feature clippy"
  timed "docs-and-lints: clippy (default features)" cargo clippy --workspace --all-targets -- -D warnings

  echo "==> [docs-and-lints] Building docs"
  timed "docs-and-lints: docs" cargo doc --workspace --no-deps
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

usage() {
  cat <<'EOF'
Usage: ./scripts/ci.sh [workspace|docs-and-lints|nostd|sizes|matrix|all]

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
  all)
    run_docs_and_lints
    run_workspace
    run_nostd
    run_sizes
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage
    exit 1
    ;;
esac
