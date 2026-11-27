#!/usr/bin/env bash
# Every crate the tail lane checks in a merged cargo call (scripts/merged-sets.sh) builds there with
# exactly the features it has when built alone: each unit of the crate's own unit graph (its
# dependencies too) is in the merged graph with the same feature set. A crate that drifts fails the
# check by name; it then moves to the matching *_alone list, or its manifest is fixed.
#
# The graphs come from cargo's --unit-graph, which only resolves (nothing is built). It is an unstable
# flag, so RUSTC_BOOTSTRAP=1 is set for those calls alone; the toolchain is pinned (rust-toolchain.toml).
# The result is kept under the target directory, keyed on every manifest, the lock file, these scripts
# and the cargo version, so a gate where no manifest changed spends a lookup. MERGED_SETS_GROUPS
# narrows the groups compared (any of: no_std desktop libraries; default all), for the checker's own
# cases (testing/merged-sets/check.sh); the gate leaves it unset.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
# shellcheck source=scripts/merged-sets.sh
source scripts/merged-sets.sh

target="${CARGO_TARGET_DIR:-target}"
cache="$target/merged-sets"
mkdir -p "$cache"
groups="${MERGED_SETS_GROUPS:-no_std desktop libraries}"
want() { [[ " $groups " == *" $1 "* ]]; }
manifests=$(find . -name Cargo.toml -not -path './target/*' -not -path './.git/*' | LC_ALL=C sort)
key=$( { cargo -V; cat Cargo.lock rust-toolchain.toml scripts/merged-sets.sh scripts/check-merged-sets.sh \
  scripts/unit-graph-diff.py; echo "$groups"; for m in $manifests; do echo "$m"; cat "$m"; done; } | sha256sum | cut -c1-16)
if [ -f "$cache/$key.ok" ]; then
  echo "    merged sets: unchanged since the last pass (key $key)"
  exit 0
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/merged-sets.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT

# Each line: <out file> <cargo arguments...>. Run several at once; each only resolves.
jobs="$work/jobs"
: >"$jobs"
add() { local out="$1"; shift; printf '%s\t%s\n' "$work/$out.json" "$*" >>"$jobs"; }
want no_std && for t in "${no_std_targets[@]}"; do
  a=(); for c in "${no_std_merged[@]}"; do a+=(-p "$c"); add "nostd-$t-$c" check -p "$c" --no-default-features --target "$t"; done
  add "nostd-$t-merged" check "${a[@]}" --no-default-features --target "$t"
done
want desktop && for t in "${desktop_targets[@]}"; do
  a=(); for c in "${portable_crates[@]}"; do a+=(-p "$c"); add "desk-$t-$c" check -p "$c" --target "$t"; done
  add "desk-$t-merged" check "${a[@]}" --target "$t"
done
want libraries && { a=(); for c in "${libraries_merged[@]}"; do
  a+=(-p "$c")
  add "libcheck-$c" check -p "$c" --no-default-features --all-targets
  add "libtest-$c" test -p "$c" --no-default-features --no-run
done
add libcheck-merged check "${a[@]}" --no-default-features --all-targets
add libtest-merged test "${a[@]}" --no-default-features --no-run; }

# graph <out> <args>: the unit graph, or the call's own error and a failure.
graph() {
  local out="$1"; shift
  # shellcheck disable=SC2086 # the arguments are crate names, targets and flags, no spaces
  if ! RUSTC_BOOTSTRAP=1 CARGO_TARGET_DIR="$cache/resolve" cargo $1 --locked -q --unit-graph -Z unstable-options \
      "${@:2}" >"$out" 2>"$out.err"; then
    echo "check-merged-sets: cargo $* failed:" >&2; cat "$out.err" >&2; return 1
  fi
}
export -f graph; export cache
# shellcheck disable=SC2016 # expanded by the inner shell
# xargs exits 123 when a call fails; the check exits 1, after the calls' own errors.
if ! tr '\t' '\n' <"$jobs" | xargs -d '\n' -n 2 -P "${MERGED_SETS_JOBS:-6}" bash -c 'graph "$0" $1'; then
  echo "check-merged-sets: a cargo call failed (its error is above)" >&2
  exit 1
fi

rc=0
compare() { python3 -I scripts/unit-graph-diff.py "$@" || rc=1; }
want no_std && for t in "${no_std_targets[@]}"; do
  args=(); for c in "${no_std_merged[@]}"; do args+=("$c=$work/nostd-$t-$c.json"); done
  compare "no_std on $t" "$work/nostd-$t-merged.json" "${args[@]}"
done
want desktop && for t in "${desktop_targets[@]}"; do
  args=(); for c in "${portable_crates[@]}"; do args+=("$c=$work/desk-$t-$c.json"); done
  compare "portable on $t" "$work/desk-$t-merged.json" "${args[@]}"
done
if want libraries; then
  args=(); for c in "${libraries_merged[@]}"; do args+=("$c=$work/libcheck-$c.json"); done
  compare "libraries, clippy" "$work/libcheck-merged.json" "${args[@]}"
  args=(); for c in "${libraries_merged[@]}"; do args+=("$c=$work/libtest-$c.json"); done
  compare "libraries, tests" "$work/libtest-merged.json" "${args[@]}"
fi
if [ "$rc" != 0 ]; then
  echo "check-merged-sets: a crate's features differ in its merged call; move it to the *_alone list in scripts/merged-sets.sh or fix its manifest" >&2
  exit 1
fi
n=$(wc -l <"$jobs")
touch "$cache/$key.ok"
echo "    merged sets: every merged crate has its alone features ($n unit graphs, key $key)"
