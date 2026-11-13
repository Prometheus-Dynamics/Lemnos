#!/usr/bin/env bash
# Runs a command as root in a user namespace whose ids are the caller's own, plus the caller's
# subordinate range (so a file can be owned by a group such as lemnos). Used by check.sh when the
# caller has the subordinate ids and the newuidmap and newgidmap helpers; check.sh uses sudo otherwise.
#
#   nsrun.sh <command> [arguments...]
set -euo pipefail
fifo="$(mktemp -u)"
mkfifo "$fifo"
# The child waits for the maps before it runs the command. unshare execs the command in place, so
# $! is the process in the new namespace, and newuidmap and newgidmap can write its maps.
unshare -U -m --propagation private sh -c 'read _ <"$0"; exec "$@"' "$fifo" "$@" &
pid=$!
newuidmap "$pid" 0 "$(id -u)" 1 1 "$(awk -F: -v u="$(id -un)" '$1 == u { print $2; exit }' /etc/subuid)" 65536
newgidmap "$pid" 0 "$(id -g)" 1 1 "$(awk -F: -v u="$(id -un)" '$1 == u { print $2; exit }' /etc/subgid)" 65536
echo go >"$fifo"
rc=0
wait "$pid" || rc=$?
rm -f "$fifo"
exit "$rc"
