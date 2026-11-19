#!/usr/bin/env bash
# Runs a command as root in a user namespace whose ids are the caller's own, plus the caller's
# subordinate range (so a file can be owned by a group such as lemnos). Used by check.sh when the
# caller has the subordinate ids and the newuidmap and newgidmap helpers; check.sh uses sudo otherwise.
#
#   nsrun.sh <command> [arguments...]
#
# unshare starts the child, but the child may not have entered its new user namespace yet when we
# look. newuidmap and newgidmap write the maps of that namespace, so the helper waits for the child's
# user namespace to differ from ours (10 s bound, polled every 10 ms) before it maps it. Every failure
# after the child starts stops the child by its PID, and the helper exits non-zero with a message
# naming the step. The child is never left behind, and the helper never waits on a child that is gone.
set -euo pipefail
pid=""
fifo=""
own_ns="$(readlink "/proc/$$/ns/user")"

fail() {
  echo "nsrun.sh: $*" >&2
  exit 1
}

# The state letter of a process from /proc/<pid>/stat, or nothing once the process is gone. Z is a
# zombie: it has exited, and only wait can reap it.
proc_state() {
  sed -n 's/^.*) \(.\).*$/\1/p' "/proc/$1/stat" 2>/dev/null || true
}

# Stops the child by its PID: SIGTERM, then SIGKILL after 5 s if it is still running, then reaps it.
stop_child() {
  local started state
  # Seconds come from the clock, not from counting polls: a poll that forks takes longer than 10 ms.
  [[ -n "$pid" ]] || return 0
  state="$(proc_state "$pid")"
  if [[ -n "$state" && "$state" != Z ]]; then
    kill -TERM "$pid" 2>/dev/null || true
    started="$(date +%s%N)"
    while :; do
      state="$(proc_state "$pid")"
      [[ -z "$state" || "$state" == Z ]] && break
      ((($(date +%s%N) - started) < 5000000000)) || break
      sleep 0.01
    done
    if [[ -n "$state" && "$state" != Z ]]; then
      echo "nsrun.sh: child $pid ignored SIGTERM; sending SIGKILL" >&2
      kill -KILL "$pid" 2>/dev/null || true
    fi
  fi
  wait "$pid" 2>/dev/null || true
  pid=""
}

cleanup() {
  local rc=$?
  trap - EXIT
  stop_child
  exec 3>&-
  if [[ -n "$fifo" ]]; then rm -f "$fifo"; fi
  exit "$rc"
}

fifo="$(mktemp -u)"
mkfifo "$fifo"
# Opened for reading and writing, so neither side can block on the open: the child opens it for
# reading later, and the write below never waits for a reader that may have gone.
exec 3<>"$fifo"
trap 'exit 130' INT
trap 'exit 129' HUP
trap 'exit 143' TERM
trap cleanup EXIT

# The child waits for the maps before it runs the command. unshare execs the command in place, so
# $! is the process in the new namespace. The child does not inherit the fifo's descriptor.
unshare -U -m --propagation private sh -c 'read _ <"$0"; exec "$@"' "$fifo" "$@" 3>&- &
pid=$!

# The bound is measured on the clock, not by counting polls: each poll forks, so it takes longer than 10 ms.
started="$(date +%s%N)"
while :; do
  child_ns="$(readlink "/proc/$pid/ns/user" 2>/dev/null || true)"
  if [[ -n "$child_ns" && "$child_ns" != "$own_ns" ]]; then
    break
  fi
  state="$(proc_state "$pid")"
  if [[ -z "$state" || "$state" == Z ]]; then
    fail "the child (pid $pid) exited before it entered its user namespace"
  fi
  if (((($(date +%s%N) - started)) >= 10000000000)); then
    fail "the child (pid $pid) did not enter its user namespace within 10 s"
  fi
  sleep 0.01
done

if ! newuidmap "$pid" 0 "$(id -u)" 1 1 "$(awk -F: -v u="$(id -un)" '$1 == u { print $2; exit }' /etc/subuid)" 65536; then
  fail "newuidmap could not write the user map of the child (pid $pid)"
fi
if ! newgidmap "$pid" 0 "$(id -g)" 1 1 "$(awk -F: -v u="$(id -un)" '$1 == u { print $2; exit }' /etc/subgid)" 65536; then
  fail "newgidmap could not write the group map of the child (pid $pid)"
fi
echo go >&3 || fail "could not signal the child (pid $pid) to run the command"
rc=0
wait "$pid" || rc=$?
pid=""
exit "$rc"
