#!/usr/bin/env bash
# The bridge (lemnos-orion) and Orion must share one copy of each shared-memory ring crate. Orion
# pulls the shm-* crates from this repository at a pinned revision; the root Cargo.toml patches that
# source to the workspace paths, so the bridge's build holds the workspace copies only. A second
# copy (a git one, or one of another version) gives types that do not unify: this fails on it.
#
# Usage: check-orion-copies.sh [file]   with a file of `cargo tree --prefix none -f '{p}'` lines to
# check that file instead of the real tree (the gate uses the real tree).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ $# -gt 0 ]; then
  tree=$(cat "$1")
else
  # --color never: CI forces colour, which put escape codes around the (*) marker.
  tree=$(cargo tree -p lemnos-orion -e normal,build,dev --prefix none -f '{p}' --locked --color never 2>/dev/null)
fi

# Escape codes out first, then the (*) marker: a repeated subtree is the same copy, not a second one.
shm=$(printf '%s\n' "$tree" | sed -e 's/\x1b\[[0-9;]*m//g' -e 's/ (\*)$//' | grep '^shm-' | sort -u || true)
[ -n "$shm" ] || { echo "check-orion-copies: no shm-* crate in the bridge's build: the check cannot tell" >&2; exit 1; }
status=0

# One line per crate: more than one means two versions or two sources.
dups=$(printf '%s\n' "$shm" | awk '{print $1}' | uniq -d)
if [ -n "$dups" ]; then
  echo "check-orion-copies: a second copy of: $dups" >&2
  printf '%s\n' "$shm" >&2
  status=1
fi

# Every copy must be a workspace path, never a git source.
foreign=$(printf '%s\n' "$shm" | grep -v ' v[^ ]* ([/]' || true)
if [ -n "$foreign" ]; then
  echo "check-orion-copies: a shm-* crate does not come from the workspace path:" >&2
  printf '%s\n' "$foreign" >&2
  status=1
fi

[ "$status" = 0 ] && echo "check-orion-copies: one copy of each shm-* crate in the bridge's build ($(printf '%s\n' "$shm" | wc -l) crates)"
exit "$status"
