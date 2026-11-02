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

  echo "==> [docs-and-lints] Checking for product names outside the allowlist"
  timed "docs-and-lints: product names" "$root_dir/scripts/check-names.sh"

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

# lemnosd and lemnos-ctl (its client, which links lemnos-ipc) as shipped in the service profile, for
# the K230D (riscv64) and Flame (RV1106G2, Cortex-A7) boards. Each binary is stripped and its size
# reported per target. The CI workflow sets the cross linkers (docs/development.md).
run_cross32() {
  local targets=(riscv64gc-unknown-linux-gnu armv7-unknown-linux-gnueabihf)
  local target bin file
  echo "==> [cross32] Building lemnosd and lemnos-ctl (service profile) for riscv64 and armv7"
  for target in "${targets[@]}"; do
    timed "cross32: build lemnosd lemnos-ctl $target" \
      cargo build --locked --profile service -p lemnosd -p lemnos-ctl --target "$target"
  done

  echo "==> [cross32] Checking the binaries are stripped and reporting sizes"
  timed "cross32: stripped sizes" bash -c '
    target_dir="$1"; shift
    for target in "$@"; do
      for bin in lemnosd lemnos-ctl; do
        file="$target_dir/$target/service/$bin"
        if readelf -S --wide "$file" | grep -q "[.]symtab"; then
          echo "error: $file is not stripped" >&2
          exit 1
        fi
        printf "%-34s %-12s %10s bytes\n" "$target" "$bin" "$(stat -c %s "$file")"
      done
    done' _ "${CARGO_TARGET_DIR:-$root_dir/target}" "${targets[@]}"

  # Smoke test under qemu-user when the runner is configured (the CI cross job sets it): lemnosd
  # starts its CLI and exits 0 on --help, which exercises the dynamic loader and libc.
  local var="CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_RUNNER"
  if [[ -n "${!var:-}" ]]; then
    echo "==> [cross32] lemnosd --help under the riscv64 runner"
    timed "cross32: lemnosd --help riscv64 under qemu" bash -c \
      "${!var} ${CARGO_TARGET_DIR:-$root_dir/target}/riscv64gc-unknown-linux-gnu/service/lemnosd --help >/dev/null"
  fi
}

usage() {
  cat <<'EOF'
Usage: ./scripts/ci.sh [workspace|docs-and-lints|nostd|sizes|matrix|cross32|all]

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
  cross32)
    run_cross32
    ;;
  all)
    run_docs_and_lints
    run_workspace
    run_nostd
    run_sizes
    run_cross32
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage
    exit 1
    ;;
esac
