#!/usr/bin/env bash
# The merged-sets checker (scripts/check-merged-sets.sh) on planted trees. Each case works on a copy of
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

copy="$work/copy"
mkdir -p "$copy"
tar -C "$tree" --exclude=./target --exclude=./.git -cf - . | tar -C "$copy" -xf -
export MERGED_SETS_GROUPS=libraries
# Each case resolves into a target directory of its own, so no case reads another's kept result
# unless it means to (cached).
run() { CARGO_TARGET_DIR="$work/t-$1" "$copy/scripts/check-merged-sets.sh" >"$work/$1.out" 2>&1; echo $?; }

# verdict <case> <status> <wanted status> <needle>
verdict() {
  if [ "$2" = "$3" ] && grep -qF -- "$4" "$work/$1.out"; then
    echo "caught      $1 (exit $2: $4)"
  else
    echo "NOT CAUGHT  $1 (exit $2, wanted exit $3 with: $4)"
    sed 's/^/    | /' "$work/$1.out" | tail -n 8
    fails=$((fails + 1))
  fi
}

rc=$(run holds); verdict holds "$rc" 0 "every merged crate has its alone features"
mv "$work/t-holds" "$work/t-cached"
rc=$(run cached); verdict cached "$rc" 0 "unchanged since the last pass"

sed -i 's/^libc.workspace = true$/libc = { workspace = true, features = ["extra_traits"] }/' "$copy/shm/fdpass/Cargo.toml"
mv "$work/t-cached" "$work/t-manifest"
rc=$(run manifest); verdict manifest "$rc" 1 "merged-sets: shm-ring (libraries, clippy): libc"
cp "$tree/shm/fdpass/Cargo.toml" "$copy/shm/fdpass/Cargo.toml"

sed -i 's/^libraries_alone=(lemnos-driver-rk-mpp lemnos-driver-rkjpege lemnos-identity lemnos-wire shm-sample-time)$/libraries_alone=(lemnos-driver-rk-mpp lemnos-driver-rkjpege lemnos-identity shm-sample-time)/; s/^  shm-ring shm-ring-latest /  lemnos-wire shm-ring shm-ring-latest /' "$copy/scripts/merged-sets.sh"
rc=$(run list); verdict list "$rc" 1 "merged-sets: lemnos-wire (libraries, clippy): lemnos-wire@"
cp "$tree/scripts/merged-sets.sh" "$copy/scripts/merged-sets.sh"

sed -i 's/^  shm-ring shm-ring-latest /  shm-no-such-crate shm-ring shm-ring-latest /' "$copy/scripts/merged-sets.sh"
rc=$(run cargo-fails); verdict cargo-fails "$rc" 1 "package ID specification \`shm-no-such-crate\` did not match any packages"

if [ "$fails" != 0 ]; then echo "merged-sets cases: $fails NOT CAUGHT" >&2; exit 1; fi
touch "$target/merged-sets-tests/$key.ok"
echo "    merged-sets cases: all 5 caught"
