#!/usr/bin/env bash
# The merged-sets checker (scripts/check-merged-sets.sh) on planted trees. Each case works on its own copy of
# TREE (default: this one), narrowed to the library group, and must end with its own status and message;
# anything else is NOT CAUGHT.
#
#   testing/merged-sets/check.sh [TREE]
#
#   holds        the copy as it is                             exit 0, every merged crate matches
#   cached       the same copy again                           exit 0 from the kept result
#   manifest     shm-fdpass asks libc for extra_traits         exit 1, names shm-ring (its libc drifts)
#   list         lemnos-wire moved into the merged list        exit 1, names lemnos-wire
#   cargo-fails  a crate no manifest has, in the merged list   exit 1, the cargo call's own error
#
# The manifest case changes only features, which the lock file does not hold, so cargo --locked still
# resolves and the drift is what fails. The result is kept under CARGO_TARGET_DIR (merged-sets-tests),
# keyed on the checker, its lists and this file, so a gate where none changed spends a lookup.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
tree="$(cd "${1:-$repo}" && pwd)"
target="${CARGO_TARGET_DIR:-$repo/target}"
mkdir -p "$target/merged-sets-tests"
key=$(cat "$tree/scripts/check-merged-sets.sh" "$tree/scripts/merged-sets.sh" "$tree/scripts/unit-graph-diff.py" \
  "$here/check.sh" "$tree/Cargo.lock" | sha256sum | cut -c1-16)
if [ -f "$target/merged-sets-tests/$key.ok" ]; then
  echo "    merged-sets cases: unchanged since the last pass (key $key)"
  exit 0
fi
work="$(mktemp -d "${TMPDIR:-/tmp}/merged-sets-cases.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
fails=0

# Each case plants in a copy of its own and resolves into a target directory of its own, and the cases run
# side by side (one after another they were 87 s at load 85; side by side about one case's time). Only
# cached reads another case's kept result: it runs after holds, on holds' copy and target.
export MERGED_SETS_GROUPS=libraries
copy_for() { mkdir -p "$work/$1"; tar -C "$tree" --exclude=./target --exclude=./.git -cf - . | tar -C "$work/$1" -xf -; }
run() { CARGO_TARGET_DIR="$work/t-$2" "$work/$2/scripts/check-merged-sets.sh" >"$work/$1.out" 2>&1; echo $? >"$work/$1.rc"; }

# verdict <case> <wanted status> <needle>
verdict() {
  local rc; rc=$(cat "$work/$1.rc")
  if [ "$rc" = "$2" ] && grep -qF -- "$3" "$work/$1.out"; then
    echo "caught      $1 (exit $rc: $3)"
  else
    echo "NOT CAUGHT  $1 (exit $rc, wanted exit $2 with: $3)"
    sed 's/^/    | /' "$work/$1.out" | tail -n 8
    fails=$((fails + 1))
  fi
}

for c in holds manifest list cargo-fails; do copy_for "$c"; done
sed -i 's/^libc.workspace = true$/libc = { workspace = true, features = ["extra_traits"] }/' "$work/manifest/shm/fdpass/Cargo.toml"
sed -i 's/^libraries_alone=(lemnos-driver-rk-mpp lemnos-driver-rkjpege lemnos-identity lemnos-wire shm-sample-time)$/libraries_alone=(lemnos-driver-rk-mpp lemnos-driver-rkjpege lemnos-identity shm-sample-time)/; s/^  shm-ring shm-ring-latest /  lemnos-wire shm-ring shm-ring-latest /' "$work/list/scripts/merged-sets.sh"
sed -i 's/^  shm-ring shm-ring-latest /  shm-no-such-crate shm-ring shm-ring-latest /' "$work/cargo-fails/scripts/merged-sets.sh"
pids=()
{ run holds holds; run cached holds; } & pids+=($!)
for c in manifest list cargo-fails; do run "$c" "$c" & pids+=($!); done
wait "${pids[@]}"

verdict holds 0 "every merged crate has its alone features"
verdict cached 0 "unchanged since the last pass"
verdict manifest 1 "merged-sets: shm-ring (libraries, clippy): libc"
verdict list 1 "merged-sets: lemnos-wire (libraries, clippy): lemnos-wire@"
verdict cargo-fails 1 "package ID specification \`shm-no-such-crate\` did not match any packages"

if [ "$fails" != 0 ]; then echo "merged-sets cases: $fails NOT CAUGHT" >&2; exit 1; fi
touch "$target/merged-sets-tests/$key.ok"
echo "    merged-sets cases: all 5 caught"
