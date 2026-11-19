#!/usr/bin/env bash
# Fails when a consumer's Cargo.lock holds any Lemnos crate from a source or rev other than the pinned one.
# A Lemnos crate is any package whose source is the Lemnos git repository (".git" or not), or any
# lemnos-* / shm-* package from anywhere else (registry, another git, a path). Each stray is listed.
# For consumer locks only (Lemnos's own build has path crates; it uses scripts/check-orion-copies.sh).
# Reads the lock only: no build, no network. Usage: check-lemnos-pin.sh <Cargo.lock> <pinned-rev>
set -euo pipefail
lock="${1:?usage: check-lemnos-pin.sh <Cargo.lock> <pinned-rev>}"
pin="${2:?usage: check-lemnos-pin.sh <Cargo.lock> <pinned-rev>}"
[ -f "$lock" ] || { echo "lemnos-pin: no $lock" >&2; exit 2; }
[[ "$pin" =~ ^[0-9a-f]{40}$ ]] || { echo "lemnos-pin: pin must be a full 40-hex rev, got $pin" >&2; exit 2; }
rows=$(awk '
function flush() { if (name != "") print name "\t" ver "\t" (src == "" ? "path" : src) }
/^\[\[package\]\]/ { flush(); name=""; src=""; ver=""; next }
/^name = /    { name=$3; gsub(/"/, "", name) }
/^version = / { ver=$3; gsub(/"/, "", ver) }
/^source = /  { src=$3; gsub(/"/, "", src) }
END { flush() }' "$lock")
lemnos_url='^git\+https://github\.com/Prometheus-Dynamics/Lemnos(\.git)?([?#]|$)'
total=0; strays=()
while IFS=$'\t' read -r name ver src; do
  [ -n "$name" ] || continue
  if [[ "$src" =~ $lemnos_url ]]; then
    total=$((total + 1))
    [ "${src##*#}" = "$pin" ] || strays+=("$name $ver from $src")
  elif [[ "$name" == lemnos-* || "$name" == shm-* ]]; then
    total=$((total + 1)); strays+=("$name $ver from $src")
  fi
done <<< "$rows"
[ "$total" -gt 0 ] || { echo "lemnos-pin: no Lemnos crate in $lock: cannot tell" >&2; exit 2; }
if [ "${#strays[@]}" -gt 0 ]; then
  echo "lemnos-pin: FAIL in $lock, ${#strays[@]} of $total Lemnos crates are not at $pin:" >&2
  printf '  %s\n' "${strays[@]}" >&2
  exit 1
fi
echo "lemnos-pin: PASS in $lock, all $total Lemnos crates at $pin"
