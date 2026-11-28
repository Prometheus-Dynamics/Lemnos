#!/usr/bin/env bash
# The shipped programs (packaging/shipped.txt) built by scripts/service-build.sh in two cargo homes,
# each with its own target directory, are byte-identical, and hold neither home's path nor this
# checkout's. The second home is made here: the first one's downloaded crates and git databases,
# linked in (copied across filesystems), and nothing else, so its sources unpack under a path of its own (--offline).
#
#   scripts/check-shipped-paths.sh [--target <triple>]   default: the host
#
# Cost: two cold service builds of the seven programs (fat LTO). CI runs it as its own job.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
home_a="${CARGO_HOME:-$HOME/.cargo}"
work="$(mktemp -d "${TMPDIR:-/tmp}/shipped-paths.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
home_b="$work/home-b"
mkdir -p "$home_b/registry" "$home_b/git"
# Linked where the two are on one filesystem, copied where not.
take() { [ -d "$home_a/$1" ] || return 0; rm -rf -- "${home_b:?}/$1"; cp -al -- "$home_a/$1" "$home_b/$1" 2>/dev/null ||
  { rm -rf -- "${home_b:?}/$1"; cp -a -- "$home_a/$1" "$home_b/$1"; }; }
take registry/cache
take registry/index
take git/db
[ -f "$home_a/config.toml" ] && cp "$home_a/config.toml" "$home_b/"
progs=$(grep -v '^#' packaging/shipped.txt | awk 'NF{print $1}')
[ -n "$progs" ] || { echo "check-shipped-paths: packaging/shipped.txt names no program" >&2; exit 1; }
args=()
for p in $progs; do args+=(-p "$p"); done
export CARGO_INCREMENTAL=0
CARGO_HOME="$home_a" CARGO_TARGET_DIR="$work/t-a" scripts/service-build.sh "$@" "${args[@]}"
CARGO_HOME="$home_b" CARGO_TARGET_DIR="$work/t-b" scripts/service-build.sh --offline "$@" "${args[@]}"
triple=""
prev=""
for a in "$@"; do [ "$prev" = --target ] && triple="$a"; prev="$a"; done
failed=0
for p in $progs; do
  a="$work/t-a/${triple:+$triple/}service/$p" b="$work/t-b/${triple:+$triple/}service/$p"
  for f in "$a" "$b"; do
    for path in "$home_a" "$home_b" "$root"; do
      if grep -qaF -- "$path" "$f"; then
        echo "check-shipped-paths: $p (${f#"$work/"}) holds the builder's path $path" >&2; failed=1
      fi
    done
  done
  if cmp -s "$a" "$b"; then
    echo "    $p: identical from two homes ($(stat -c %s "$a") bytes, $(sha256sum <"$a" | cut -c1-16))"
  else
    echo "check-shipped-paths: $p differs between the two homes" >&2; failed=1
  fi
done
[ "$failed" = 0 ] || { echo "check-shipped-paths: failed; see above" >&2; exit 1; }
echo "check-shipped-paths: the shipped programs are the same from two homes, and name neither"
