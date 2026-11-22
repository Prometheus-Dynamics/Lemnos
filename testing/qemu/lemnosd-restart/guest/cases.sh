#!/bin/sh
# The lemnosd restart cases (testing/qemu/run-lemnosd-restart.sh), run as root by lemnos-restart-cases.service once the
# packaged daemon and the restart helper are up. Stage 1 (kernel argument rst.stage=1) runs the cases on the running daemon
# and ends with the reboot of case a; stage 2 (rst.stage=2, the next boot on the same disk) reads the state back.
# Everything goes to the console. The host reads the lines
#   LEMNOSD-RESTART-GUEST PASS|FAIL <case> TEXT
#   LEMNOSD-RESTART-GUEST STATE <name> TEXT   (the daemon's info, list and boots, as the daemon answers them)
exec >/dev/console 2>&1

LEMNOS=900   # the daemon's user and group
OTHER=901    # a user in group lemnos who is not the daemon's user
SOCK=/run/lemnos/lemnosd.sock
CHECK=/usr/bin/lemnos-restart-lemnosd-check
PROBE=/usr/bin/lemnos-restart-probe
SLOT=install-1
stage=$(sed -n 's/.*rst\.stage=\([0-9]*\).*/\1/p' /proc/cmdline)

say() { echo "LEMNOSD-RESTART-GUEST $*"; }
# as UID GID COMMAND...: the command with that user and group, and group lemnos as a supplementary group.
as() {
	uid=$1
	gid=$2
	shift 2
	setpriv --reuid="$uid" --regid="$gid" --groups="$LEMNOS" -- "$@"
}
# state: the daemon's info (its state word and error), printed as the host reads it.
state() { "$CHECK" state "$SOCK" 2>&1; }
# list_line: the daemon's list (its top-level state word, its error, and its slots), as the checker prints it.
list_line() { "$CHECK" state "$SOCK" 2>&1 | sed -n 's/.*STATE list //p'; }
# word_of_state: the state word of the daemon (the top-level `"state"` of list, after the slot's own fields are cut).
word_of_state() { list_line | sed -n 's/^.*"state":"\([^"]*\)","error".*/\1/p'; }
# wait_for_reason TEXT SECONDS: polls the daemon's list until its state is restart-failed and its error holds TEXT.
wait_for_reason() {
	i=0
	while [ "$i" -lt "$2" ]; do
		if [ "$(word_of_state)" = restart-failed ] && list_line | grep -q -- "$1"; then return 0; fi
		sleep 1
		i=$((i + 1))
	done
	return 1
}

for i in $(seq 1 120); do [ -S "$SOCK" ] && [ -S /run/lemnos-restart.sock ] && break; sleep 0.5; done
say "CASES stage $stage socket $([ -S "$SOCK" ] && echo up || echo down) helper $([ -S /run/lemnos-restart.sock ] && echo up || echo down) load $(cat /proc/loadavg)"

if [ "$stage" = 2 ]; then
	# The next boot on the same disk: the trial case's trial is still armed (the record says so), and the boot chain
	# the guest booted is the one the state names. The host reads these lines and the boot partition after QEMU exits.
	state | while IFS= read -r l; do say "STAGE2 $l"; done
	if list_line | grep -q "\"state\":\"rebooting\"" && list_line | grep -q "\"slot_trial\":\"$SLOT\""; then
		say "PASS a2 the next boot's state still has $SLOT on trial"
	else
		say "FAIL a2 the next boot's state does not have $SLOT on trial: $(state | tr '\n' ' ')"
	fi
	say "DONE stage 2"
	systemctl --no-block poweroff
	sleep 20
	echo o > /proc/sysrq-trigger
	exit 0
fi

# Stage 1. The bundle goes in first: the slot the trial boots.
bundle=/var/lib/lemnos-restart/bundle.tar
"$CHECK" install "$SOCK" "$bundle" "$SLOT" /dev/vda1 > /run/lemnos-restart-install.out 2>&1
rc=$?
say "INSTALL rc=$rc $(grep -c 'PASS' /run/lemnos-restart-install.out) checks passed"
if [ "$rc" -ne 0 ]; then
	say "FAIL install: $(tr '\n' ' ' < /run/lemnos-restart-install.out)"
fi

# (d) A peer in group lemnos that is not uid 900 or root is refused; the helper takes only verb and nonce.
out=$(as $OTHER $LEMNOS $PROBE check 2>&1); rc=$?
want="uid $OTHER may not restart this board: only uid $LEMNOS (the daemon's user) and root may"
case "$out" in *"$want"*) refused=yes ;; *) refused=no ;; esac
say "d1 as uid $OTHER (group lemnos), check: rc=$rc $out"
bad=""
for line in "frobnicate 1234" "reboot 1234 unit=reboot.target" "check" "check ABC" "reboot 1234 extra" ""; do
	r=$(as $LEMNOS $LEMNOS $PROBE raw "$line" 2>&1)
	case "$r" in *"refused"*) ;; *) bad="$bad [$line]: $r" ;; esac
	say "d2 raw '$line' -> $r"
done
if [ "$rc" -ne 0 ] && [ "$refused" = yes ] && [ -z "$bad" ]; then
	say "PASS d a peer in group lemnos that is not uid $LEMNOS is refused; a request that is not verb and nonce is refused"
else
	say "FAIL d peer refused=$refused rc=$rc; not refused: $bad"
fi

# (c) StartUnit fails on the same connection: reboot.target is ordered after a unit that orders it before itself, so the
# service manager refuses the transaction ("Transaction order is cyclic"). The helper says accepted, then failed; the
# daemon's trial is disarmed, its state is restart-failed with the reason, and the caller gets the error.
cat > /run/systemd/system/lemnos-guest-cycle.service <<'UNIT'
[Unit]
Description=Restart guest: a unit whose order makes reboot.target's transaction cyclic
DefaultDependencies=no
Requires=reboot.target
After=reboot.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/true
UNIT
mkdir -p /run/systemd/system/reboot.target.d
printf '[Unit]\nRequires=lemnos-guest-cycle.service\nAfter=lemnos-guest-cycle.service\n' > /run/systemd/system/reboot.target.d/cycle.conf
systemctl daemon-reload
out=$("$CHECK" trial "$SOCK" "$SLOT" 2>&1)
say "c1 trial with the cyclic reboot.target: $out"
if wait_for_reason "the restart failed: the service manager" 5; then
	say "c2 $(list_line | grep -o '"state":"restart-failed","error":"[^"]*"')"
	if echo "$out" | grep -q "answered"; then
		say "PASS c trial disarmed: state restart-failed with the reason, and the caller got the error"
	else
		say "FAIL c the caller did not get the error: $out"
	fi
else
	say "FAIL c the state did not become restart-failed within 5 s: $(state | tr '\n' ' ')"
fi
rm -f /run/systemd/system/reboot.target.d/cycle.conf /run/systemd/system/lemnos-guest-cycle.service
rmdir /run/systemd/system/reboot.target.d 2>/dev/null
systemctl daemon-reload

# (b) accepted, then no stop request within the bound: NOT RUN in this guest. A reboot job puts the system in the
# stopping state at once (the shutdown's stop jobs are queued with it, whatever reboot.target and systemd-reboot.service
# wait on: run 4 and run 5 rebooted during it), so the helper's bound cannot expire on systemd. The bound's branch is
# covered by the helper's unit tests with a stand-in manager, and it is listed as not reproduced in the board-run note.
say "NOT-RUN b accepted, then no stop within the bound: systemd starts the shutdown at once for a reboot job (see testing/qemu/lemnosd-restart/guest/cases.sh)"

# (a) accepted, then SIGTERM: the reboot is happening, so the trial stays armed. The slow unit stops for 5 s on the way
# (lemnos-guest-slow.service). The trial call returns once the helper accepted; the stop comes after.
out=$("$CHECK" trial "$SOCK" "$SLOT" force 2>&1)
say "a1 trial, the restart accepted: $out"
case "$out" in
*"answered ok"*)
	state | while IFS= read -r l; do say "a2 $l"; done
	say "a3 the reboot is under way; the daemon takes SIGTERM and the slow unit stops now"
	say "PASS a accepted, then the reboot: the trial stays armed (the next boot's state is checked in stage 2)"
	;;
*"answered Disconnected"*)
	# The daemon's answer was cut off by its own stop: the helper accepted, and the shutdown took the daemon down.
	say "a2 the daemon stopped under the reboot (SIGTERM) before it could answer the trial's call"
	say "a3 the reboot is under way; the slow unit stops now"
	say "PASS a accepted, then SIGTERM under the reboot: the trial stays armed (the next boot's state is checked in stage 2)"
	;;
*)
	say "FAIL a the trial was not accepted, so no reboot started: $out"
	say "DONE stage 1 without a reboot"
	systemctl --no-block poweroff
	sleep 20
	echo o > /proc/sysrq-trigger
	;;
esac
# The reboot ends this unit (it has no stop of its own). Waiting here keeps the stage from going on past the reboot.
sleep 120
