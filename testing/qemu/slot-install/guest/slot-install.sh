#!/bin/bash
# The guest's install run (testing/qemu/run-slot-install.sh). Two competitors are pinned to CPU 1, the core the
# daemon runs on (its drop-in), and end by themselves after COMPETE_S seconds; they are also killed by their
# recorded PIDs when this script exits. The checker installs the bundle through the daemon's socket from CPU 0.
# Afterwards the daemon's warnings and its tick trace are printed, and the guest powers off.
exec </dev/null
ulimit -c 0
COMPETE_S=${COMPETE_S:-600}
load() { cat /proc/loadavg; }
echo "SLOT-INSTALL-GUEST START load $(load) cpus $(nproc)"
end=$((SECONDS + COMPETE_S))
taskset -c 1 bash -c "while [ \$SECONDS -lt $end ]; do :; done" &
comp1=$!
taskset -c 1 bash -c "while [ \$SECONDS -lt $end ]; do :; done" &
comp2=$!
trap 'kill -KILL "$comp1" "$comp2" 2>/dev/null; echo "SLOT-INSTALL-GUEST COMPETITORS-STOPPED load $(load)"' EXIT
echo "SLOT-INSTALL-GUEST COMPETITORS pids $comp1 $comp2 pinned to cpu 1 for ${COMPETE_S}s, load $(load)"
for i in $(seq 1 60); do [ -S /run/lemnos/lemnosd.sock ] && break; sleep 0.5; done
taskset -c 0 /usr/bin/lemnos-slot-install-check install /run/lemnos/lemnosd.sock /var/lib/slot-install/bundle.tar install-1 /dev/vda1
rc=$?
echo "SLOT-INSTALL-GUEST CHECKER rc=$rc load $(load)"
# The daemon's abandoned call may still be running (its lines come after the job's end): wait for them.
sleep 40
journalctl -b -u lemnosd --no-pager -p warning | grep -v '^-- No entries --' | sed 's/^/SLOT-INSTALL-GUEST JOURNAL-WARNING /'
journalctl -b -u lemnosd --no-pager | grep -E 'deadline|interrupted|INTERRUPT|DIAG' | sed 's/^/SLOT-INSTALL-GUEST TRACE /'
# The daemon's tick durations (SLOT-TICK, from its start trace) whose journal time falls in the job's window.
read -r win_from win_to < /var/lib/slot-install/window 2>/dev/null || { win_from=0; win_to=0; }
journalctl -b -u lemnosd --no-pager -o short-unix | grep 'SLOT-TICK' \
	| awk -v a="$win_from" -v b="$win_to" '$1 + 0 >= a && $1 + 0 <= b' | sed 's/^/SLOT-INSTALL-GUEST TICK-IN-JOB /'
echo "SLOT-INSTALL-GUEST TICKS-ALL $(journalctl -b -u lemnosd --no-pager -o short-unix | grep -c 'SLOT-TICK')"
echo "SLOT-INSTALL-GUEST END rc=$rc"
# The competitors' CPU time over the run, from /proc (clock ticks): they ran, pinned to CPU 1, while the install went on.
for pid in "$comp1" "$comp2"; do
	echo "SLOT-INSTALL-GUEST COMPETITOR pid $pid cpu-ticks $(awk '{ print $14 + $15 }' /proc/$pid/stat 2>/dev/null || echo gone-before-end)"
done
kill -KILL "$comp1" "$comp2" 2>/dev/null
echo "SLOT-INSTALL-GUEST COMPETITORS-STOPPED after the install (killed by their recorded pids)"
echo "SLOT-INSTALL-GUEST POWEROFF requested"
systemctl --no-block poweroff
# If the shutdown does not take the guest down, the kernel's sysrq poweroff does (the host still reads every line above).
sleep 20
echo "SLOT-INSTALL-GUEST POWEROFF forced through sysrq"
echo o > /proc/sysrq-trigger
exit "$rc"
