#!/usr/bin/env bash
# The /data rules of bench.sh --real. The board runs `list` and `gone` (fed on stdin with
# `bash -s --`); the PC runs `room` and `diff`. Each subcommand is the one code path the run uses.
#
#   data-check.sh list DIR
#       One line per entry under DIR (recursive, one filesystem, DIR itself not listed):
#       path, type, size in bytes, mtime in seconds, and the sha256 of each regular file up to 1 MiB
#       (`-` for the others). Sorted, so two listings compare line by line.
#
#   data-check.sh room NAME TOTAL AVAIL SIZE
#       States the scratch file NAME, its SIZE in bytes, and the partition's TOTAL and AVAIL bytes.
#       Refuses by NAME (exit 1, nothing written) if SIZE is over the bound, or if less than half of
#       TOTAL would stay free after the write.
#
#   data-check.sh diff BEFORE AFTER
#       Prints each difference between two listings. Exit 1 if there is any.
#
#   data-check.sh gone PATH
#       Exit 0 only if nothing is at PATH (not a file, not a link, not a directory).
set -euo pipefail

max_bytes=$((64 * 1024 * 1024))
hash_max=1048576

usage() {
  echo "usage: data-check.sh list DIR | room NAME TOTAL AVAIL SIZE | diff BEFORE AFTER | gone PATH" >&2
  exit 2
}

list() {
  local dir=$1 rel type size mtime hash
  [ -d "$dir" ] || {
    echo "data-check: $dir is not a directory" >&2
    return 1
  }
  find "$dir" -xdev -mindepth 1 -printf '%P\t%y\t%s\t%T@\n' | sort |
    while IFS=$'\t' read -r rel type size mtime; do
      hash=-
      if [ "$type" = f ] && [ "$size" -le "$hash_max" ]; then
        hash=$(sha256sum -- "$dir/$rel" 2>/dev/null | cut -d' ' -f1) || true
        [ -n "$hash" ] || hash=unreadable
      fi
      printf '%s\t%s\t%s\t%s\t%s\n' "$rel" "$type" "$size" "$mtime" "$hash"
    done
}

room() {
  local name=$1 total=$2 avail=$3 size=$4 after
  after=$((avail - size))
  echo "scratch file $name: $size bytes; partition $total bytes, $avail free now, $after free after the write (at least $((total / 2)) must stay free)"
  if [ "$size" -gt "$max_bytes" ]; then
    echo "refused: $name is $size bytes, over the bound of $max_bytes bytes; nothing was written" >&2
    return 1
  fi
  if [ $((after * 2)) -lt "$total" ]; then
    echo "refused: $name would leave $after of $total bytes free, less than half the partition; nothing was written" >&2
    return 1
  fi
}

diff_listings() {
  if diff -u -- "$1" "$2"; then
    echo "no difference between the listings"
  else
    echo "difference outside the scratch file: see the lines above" >&2
    return 1
  fi
}

gone() {
  if [ -e "$1" ] || [ -L "$1" ]; then
    echo "still there: $1" >&2
    return 1
  fi
  echo "gone: $1"
}

case ${1:-} in
  list) [ $# -eq 2 ] || usage; list "$2" ;;
  room) [ $# -eq 5 ] || usage; room "$2" "$3" "$4" "$5" ;;
  diff) [ $# -eq 3 ] || usage; diff_listings "$2" "$3" ;;
  gone) [ $# -eq 2 ] || usage; gone "$2" ;;
  *) usage ;;
esac
