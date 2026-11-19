#!/usr/bin/env bash
# Fails when Cargo.lock holds more than one copy of any shm-* package (two sources, or two versions).
# Reads the lock only: no build, no network. Usage: check-shm-one-source.sh [path/to/Cargo.lock]
set -euo pipefail
lock="${1:-Cargo.lock}"
[ -f "$lock" ] || { echo "shm-one-source: no $lock" >&2; exit 2; }
pairs=$(awk '
function flush() { if (name ~ /^shm-/) print name "\t" (src == "" ? "path" : src) "\t" ver }
/^\[\[package\]\]/ { flush(); name=""; src=""; ver=""; next }
/^name = /    { name=$3; gsub(/"/, "", name) }
/^version = / { ver=$3; gsub(/"/, "", ver) }
/^source = /  { src=$3; gsub(/"/, "", src) }
END { flush() }' "$lock" | sort -u)
[ -n "$pairs" ] || { echo "shm-one-source: no shm-* package in $lock: cannot tell" >&2; exit 2; }
dups=$(printf '%s\n' "$pairs" | cut -f1 | sort | uniq -c | awk '$1 > 1 { print $2 }')
if [ -n "$dups" ]; then
  echo "shm-one-source: FAIL in $lock, more than one copy of:" >&2
  printf '%s\n' "$pairs" | awk -F'\t' 'NR == FNR { d[$1]; next } ($1 in d)' <(printf '%s\n' "$dups") - >&2
  exit 1
fi
echo "shm-one-source: PASS in $lock, one copy of each of $(printf '%s\n' "$pairs" | cut -f1 | sort -u | wc -l) shm-* packages"
