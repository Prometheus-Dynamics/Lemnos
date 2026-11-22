#!/bin/sh
# The checks of the systemd restart guest (testing/qemu/run-systemd-restart.sh), run once as root by
# lemnos-guest-driver.service. Everything goes to the console. The host reads the lines
#   RESTART-GUEST PASS n TEXT  /  RESTART-GUEST FAIL n TEXT
# and the reboot's own lines (restart-probe's "accepted" and "stopping", the slow unit's SLOW-STOP marks).
exec >/dev/console 2>&1

LEMNOS=990   # the daemon's user and group
OTHER=991    # a user in group lemnos who is not the daemon's user
PROBE=/usr/bin/lemnos-restart-probe

say() { echo "RESTART-GUEST $*"; }

# as UID GID COMMAND...: the command with that user and group, and group lemnos as a supplementary group.
as() {
	uid=$1
	gid=$2
	shift 2
	setpriv --reuid="$uid" --regid="$gid" --groups="$LEMNOS" -- "$@"
}

say "SYSTEMD $(systemctl --version | head -n 1)"

# PID 1 attaches to the system bus once dbus.socket and dbus.service are running (systemd 259.10,
# src/core/manager.c manager_dbus_is_running). The bus is a unit of the guest, so there is no re-execution here.
say "BUS systemd1 on the system bus: $(busctl --system list 2>/dev/null | grep -c '^org.freedesktop.systemd1 ') (dbus.socket $(systemctl is-active dbus.socket) dbus.service $(systemctl is-active dbus.service))"
say "BUS diag pid1 unix sockets to the system bus path: $(grep -c system_bus_socket /proc/1/net/unix 2>&1); pid1 fds: $(ls /proc/1/fd 2>/dev/null | wc -l); dbus pids: $(pgrep -x dbus-daemon 2>/dev/null | tr '\n' ' ')"
say "BUS diag busctl status: $(timeout 5 busctl --system status 2>&1 | head -2 | tr '\n' ' ')"
say "BUS diag bus socket: $(ls -l /run/dbus/system_bus_socket 2>&1)"
say "BUS systemd1 on the system bus: $(busctl --system list 2>/dev/null | grep -c '^org.freedesktop.systemd1 ') after ${bus_wait} half-seconds"

# Bus status (diagnostic): whether the system bus is up and PID 1 is on it, for the fail-before.
say "BUS journald: $(systemctl is-active systemd-journald.service 2>&1) dbus: $(systemctl is-active lemnos-guest-dbus.service 2>&1) mainpid $(systemctl show -P MainPID lemnos-guest-dbus.service 2>&1)"
say "BUS system list: $(timeout 10 busctl --system list 2>&1; echo rc=$?)"
say "BUS journal begin"
journalctl -b --no-pager -o cat 2>&1 | grep -i 'bus\|systemd1\|dbus' | head -n 30 | while IFS= read -r l; do say "BUS journal: $l"; done
say "BUS journal end"

# PROOF of the PID 1 handshake fix (run notes: /run/user/1000/tp/RST/hypothesis-batch.txt).
# A: the fate of a call sent right after BEGIN (gap 0), with a second message written 1 s later.
say "PROOF hold: $($PROBE hold 2>&1 | tr '\n' '|')"
# B: gap 0 with no Ping, 20 fresh connections: how many answered within 2 s (the baseline).
say "PROOF no-ping gap 0 x20: $($PROBE gap 0 20 2>&1 | tr '\n' ' ')"
# C: the real helper (with the Ping handshake), 20 checks as lemnos, no competitors.
# The count is of the strict output line (`restart-probe: check: ok`), not of the exit status.
ok=0; i=0; while [ $i -lt 20 ]; do i=$((i + 1)); out=$(as $LEMNOS $LEMNOS $PROBE check 2>&1); rc=$?; [ "$out" = "restart-probe: check: ok" ] && ok=$((ok + 1)); [ $i -le 3 ] && say "PROOF helper check $i rc=$rc: $out"; done
say "PROOF helper check x20, no load: $ok of 20 strict ok"
# D: the same with two competitors, both pinned to one vCPU (CPU 1, the test's own core), bounded to 90 s: they end
# on their own, by timeout; nothing is signalled.
timeout 90 taskset -c 1 bash -c 'while :; do :; done' &
timeout 90 taskset -c 1 bash -c 'while :; do :; done' &
ok=0; i=0; while [ $i -lt 20 ]; do i=$((i + 1)); out=$(as $LEMNOS $LEMNOS $PROBE check 2>&1); rc=$?; [ "$out" = "restart-probe: check: ok" ] && ok=$((ok + 1)); [ $i -le 3 ] && say "PROOF helper check under load $i rc=$rc: $out"; done
say "PROOF helper check x20, two competitors on CPU 1: $ok of 20 strict ok"
say "PROOF hold under two competitors: $($PROBE hold 2>&1 | tr '\n' '|')"
# E: the real call is sent once per request: a socat relay counts the writes of the helper's LoadUnit call (its
# length is fixed) and of its Pings, for 5 checks.
mkdir -p /run/systemd/system/lemnos-restart.service.d
printf '[Service]\nExecStart=\nExecStart=/usr/bin/lemnos-restart --user lemnos --manager /run/relay.sock\n' > /run/systemd/system/lemnos-restart.service.d/relay.conf
systemctl daemon-reload
socat -x -v UNIX-LISTEN:/run/relay.sock,fork,mode=0666 UNIX-CONNECT:/run/systemd/private 2>/run/relay.log &
sleep 1
systemctl restart lemnos-restart.service
say "PROOF relay one check: $(as $LEMNOS $LEMNOS $PROBE check 2>&1 | tr '\n' ' ')"
say "PROOF relay log head: $(head -c 2500 /run/relay.log | cut -c1-80 | tr '\n' '|')"
for i in 1 2 3 4; do as $LEMNOS $LEMNOS $PROBE check >/dev/null 2>&1; done
calls=$(grep -c '^> .*length=194 ' /run/relay.log)
pings=$(grep -c '^> .*length=' /run/relay.log)
say "PROOF relay, 5 checks: client writes of 194 bytes (the LoadUnit call) $calls; all client writes $pings"
rm -f /run/systemd/system/lemnos-restart.service.d/relay.conf
systemctl daemon-reload
systemctl restart lemnos-restart.service

# (1) As lemnos, check gets ok.
out=$(as $LEMNOS $LEMNOS $PROBE check 2>&1)
rc=$?
if [ "$rc" -eq 0 ] && [ "$out" = "restart-probe: check: ok" ]; then
	say "PASS 1 as lemnos, check: $out"
else
	say "FAIL 1 as lemnos, check rc=$rc: $out"
	# Where the helper is: its state, and what its threads are waiting on (for a helper that never answers).
	pid=$(systemctl show -P MainPID lemnos-restart.service)
	say "DIAG helper MainPID=$pid state=$(systemctl is-active lemnos-restart.service)"
	[ -n "$pid" ] && [ "$pid" -gt 0 ] && {
		say "DIAG syscall: $(cat /proc/$pid/syscall 2>&1)"
		say "DIAG wchan: $(cat /proc/$pid/wchan 2>&1)"
		say "DIAG stack: $(cat /proc/$pid/stack 2>&1 | tr '\n' ';')"
	say "DIAG journal: $(journalctl -b --no-pager 2>&1 | grep -i 'lemnos\|systemd-journald\|dbus' | tail -n 20 | tr '\n' '|')"
	}
fi

# (2) As another member of group lemnos, check is refused by uid, with the policy text.
out=$(as $OTHER $LEMNOS $PROBE check 2>&1)
rc=$?
want="uid $OTHER may not restart this board: only uid $LEMNOS (the daemon's user) and root may"
case "$out" in
*"$want"*) refused=yes ;;
*) refused=no ;;
esac
if [ "$rc" -ne 0 ] && [ "$refused" = yes ]; then
	say "PASS 2 as other, check refused by uid: $out"
else
	say "FAIL 2 as other, check rc=$rc (want refused by uid $OTHER): $out"
fi

# (3) A garbage request is refused, whatever the line says.
a=$(as $LEMNOS $LEMNOS $PROBE raw "frobnicate 1234" 2>&1)
b=$(as $LEMNOS $LEMNOS $PROBE raw "reboot zz" 2>&1)
case "$a" in *"raw: answer: refused - "*) ra=yes ;; *) ra=no ;; esac
case "$b" in *"raw: answer: refused - "*) rb=yes ;; *) rb=no ;; esac
if [ "$ra" = yes ] && [ "$rb" = yes ]; then
	say "PASS 3 garbage refused: $a | $b"
else
	say "FAIL 3 garbage not refused: $a | $b"
fi

# (4) The slow unit is active, and its stop is about 5 s with TimeoutStopSec above it.
active=$(systemctl is-active lemnos-guest-slow.service)
timeout_stop=$(systemctl show -P TimeoutStopUSec lemnos-guest-slow.service)
if [ "$active" = active ] && [ "$timeout_stop" = 30s ] && grep -q '^sleep 5$' /usr/lib/lemnos-guest/slow-stop.sh; then
	say "PASS 4 slow unit active, ExecStop sleeps 5 s, TimeoutStopUSec=$timeout_stop"
else
	say "FAIL 4 slow unit: is-active=$active TimeoutStopUSec=$timeout_stop"
fi

# Fail-before: the old path, a non-root systemctl reboot, with no helper in it. It must be refused, and the
# system must stay up (the helper's reboot comes after this).
out=$(as $LEMNOS $LEMNOS timeout 30 systemctl reboot 2>&1)
rc=$?
say "OLD-PATH systemctl reboot as lemnos rc=$rc: $(echo "$out" | tr '\n' ' ')"
case "$out" in
*"Access denied"*|*"Interactive authentication required"*) denied=yes ;;
*) denied=no ;;
esac
if [ "$rc" -ne 0 ] && [ "$denied" = yes ] && [ "$(systemctl is-active lemnos-guest-slow.service)" = active ]; then
	say "PASS fail-before the old path is refused"
else
	say "FAIL fail-before the old path was not refused (rc=$rc)"
fi

# (5) As lemnos, the helper's reboot is accepted, and then the system is stopping. The reboot's own lines
# (accepted, stopping) are the probe's, on the console above this one.
out=$(as $LEMNOS $LEMNOS $PROBE reboot 2>&1)
rc=$?
say "REBOOT-LINES $(echo "$out" | tr '\n' ' ')"
case "$out" in
*"accepted"*"stopping"*) started=yes ;;
*) started=no ;;
esac
if [ "$rc" -eq 0 ] && [ "$started" = yes ]; then
	say "PASS 5 as lemnos, reboot accepted then stopping"
else
	say "FAIL 5 as lemnos, reboot rc=$rc: $(echo "$out" | tr '\n' ' ')"
fi
