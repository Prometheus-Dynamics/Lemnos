#!/bin/bash
# target-sv.sh <dir>...: makes each cargo target dir its own btrfs subvolume.
#
# Each subvolume has its own file tree and its own tree locks. With several targets in one
# subvolume, a build's many small metadata writes wait behind every other build's.
#
# A dir that is missing is created as a subvolume. A dir that exists but is not one is removed and
# created again as an empty subvolume, and the next build fills it fresh. It is never seeded from
# the old dir, not even by a reflink copy: a target that has grown keeps every artifact it ever
# built in deps/, rustc lists deps/ on every invocation, and so a grown copy stays slow in a
# subvolume of its own. Nothing is ever mv'd into a subvolume. Off btrfs a dir is only created.
# Prints a line for each dir it changes. A subvolume is removed like any dir, with rm -rf.
set -euo pipefail
is_sv() { [ "$(stat -c %i "$1")" = 256 ]; }
for t in "$@"; do
  case "$t" in /*) ;; *) echo "target-sv: $t is not an absolute path" >&2; exit 2 ;; esac
  parent=$(dirname "$t"); mkdir -p "$parent"
  if [ "$(stat -f -c %T "$parent")" != btrfs ]; then mkdir -p "$t"; continue; fi
  if [ -d "$t" ] && is_sv "$t"; then continue; fi
  if [ -e "$t" ]; then
    [ -d "$t" ] || { echo "target-sv: $t is not a directory" >&2; exit 2; }
    rm -rf --one-file-system -- "${t:?}"
    btrfs subvolume create "$t" > /dev/null
    echo "target-sv: $t was a plain dir: removed, and created again as an empty subvolume"
  else
    btrfs subvolume create "$t" > /dev/null
    echo "target-sv: $t created as a subvolume"
  fi
done
