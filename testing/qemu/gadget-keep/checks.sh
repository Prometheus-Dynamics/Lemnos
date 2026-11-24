#!/bin/bash
# The gadget keep checks under a real systemd (testing/qemu/run-gadget-keep.sh), run as root by
# lemnos-gadget-keep-checks.service once the packaged lemnosd.service (with the packaged gadget drop-in) has started. The
# daemon makes the gadget on dummy_udc.0; dummy_hcd loops it back to the guest's own host controller, whose cdc_ncm
# takes the network function. Each check prints one line on the console:
#   GADGET-KEEP-GUEST PASS n TEXT  or  GADGET-KEEP-GUEST FAIL n TEXT
#   1  the daemon is up, the gadget bound, the host's cdc_ncm interface there, and systemd's store holds the gadget's
#      files (systemd-analyze fdstore: the record, ep0 and the endpoints, as lemnos-ffs.dummy_udc.0.*)
#   2  systemctl restart: a new main process; UDC never read clear; the new daemon took the gadget up from the store
#      ("taken up as it was"), set nothing up; the store still holds the files; both interfaces kept their index
#   3  SIGKILL to the main process (systemctl kill, the guest's own PID 1): Restart=always starts it again; the same
#   4  systemctl stop: the gadget is off the bus (UDC clear) and the store is empty
#   5  systemctl start: the gadget is set up and bound afresh, and the store holds its files again
# Then the guest powers off. PID 1 here is the guest's systemd; nothing in this file runs on the host's.
set -u
UDC_FILE=/sys/kernel/config/usb_gadget/lemnos-dummy_udc.0/UDC
LOG=/run/lemnosd-gadget-keep.log
WATCH=/run/gadget-keep-udc-watch
say() { echo "GADGET-KEEP-GUEST $*" > /dev/console; }
pass() { say "PASS $*"; }
fail() { say "FAIL $*"; }
check() { local n="$1" text="$2"; shift 2; if "$@"; then pass "$n $text"; else fail "$n $text"; fi; }

# within SECONDS COMMAND...: true once COMMAND is, polled every 100 ms.
within() {
	local until=$((SECONDS + $1)); shift
	while [ "$SECONDS" -le "$until" ]; do
		"$@" && return 0
		sleep 0.1
	done
	return 1
}
active() { [ "$(systemctl is-active lemnosd.service)" = active ]; }
bound() { [ "$(cat "$UDC_FILE" 2>/dev/null)" = dummy_udc.0 ]; }
clear_udc() { [ -z "$(cat "$UDC_FILE" 2>/dev/null)" ]; }
main_pid() { systemctl show -p MainPID --value lemnosd.service; }
host_if() {
	local d
	for d in /sys/class/net/*; do
		[ "$(basename "$(readlink "$d/device/driver" 2>/dev/null)")" = cdc_ncm ] && { basename "$d"; return 0; }
	done
	return 1
}
index_of() { cat "/sys/class/net/$1/ifindex" 2>/dev/null; }
store() { systemd-analyze fdstore lemnosd.service 2>&1; }
# The store's lemnos-ffs names: the record, ep0 and at least two endpoints.
store_full() {
	local s
	s="$(store)"
	grep -q 'lemnos-ffs\.dummy_udc\.0\.record' <<<"$s" && grep -q 'lemnos-ffs\.dummy_udc\.0\.ep0' <<<"$s" \
		&& [ "$(grep -c 'lemnos-ffs\.dummy_udc\.0\.ep[1-9]' <<<"$s")" -ge 2 ]
}
store_empty() { ! store | grep -q 'lemnos-ffs\.'; }
log_from() { tail -c +"$(($1 + 1))" "$LOG" 2>/dev/null; }
log_size() { stat -c %s "$LOG" 2>/dev/null || echo 0; }
said_since() { log_from "$1" | grep -q -F "$2"; }
# The UDC read every 5 ms into $WATCH: a line for each read that was not dummy_udc.0.
watch_start() {
	: > "$WATCH"
	( while :; do bound || echo "clear at $EPOCHREALTIME" >> "$WATCH"; sleep 0.005; done ) &
	WATCHER=$!
}
watch_end() { kill "$WATCHER" 2>/dev/null; wait "$WATCHER" 2>/dev/null; }
never_clear() { [ ! -s "$WATCH" ]; }

# The checks after a restart or a kill (number n), from log offset `from`, old main process `old`.
came_back() {
	local n="$1" what="$2" from="$3" old="$4" board="$5" host="$6"
	check "$n" "$what: the unit is active again under a new main process" \
		within 30 bash -c '[ "$(systemctl is-active lemnosd.service)" = active ] && p=$(systemctl show -p MainPID --value lemnosd.service) && [ "$p" != 0 ] && [ "$p" != '"$old"' ]'
	check "$n" "$what: the new daemon took the gadget up from systemd's store, as it was" \
		within 30 bash -c "tail -c +$((from + 1)) $LOG | grep -q -F '(taken up as it was)' && tail -c +$((from + 1)) $LOG | grep -q -F 'kept bound through the restart'"
	check "$n" "$what: nothing was set up again or let go" \
		bash -c "! tail -c +$((from + 1)) $LOG | grep -q -E '\(set up and bound\)|the files kept for it were let go'"
	sleep 1
	watch_end
	check "$n" "$what: UDC was never read clear (read every 5 ms)" never_clear
	[ -s "$WATCH" ] && head -3 "$WATCH" > /dev/console
	check "$n" "$what: systemd's store still holds the gadget's files" store_full
	check "$n" "$what: the board's interface kept its index ($board)" bash -c "[ \"\$(cat /sys/class/net/lemnos0/ifindex 2>/dev/null)\" = $board ]"
	check "$n" "$what: the host's cdc_ncm interface kept its index ($host)" bash -c "h=\$(for d in /sys/class/net/*; do [ \"\$(basename \"\$(readlink \$d/device/driver 2>/dev/null)\")\" = cdc_ncm ] && cat \$d/ifindex; done); [ \"\$h\" = $host ]"
}

say "CASES start"
grep -q ' /sys/kernel/config configfs ' /proc/mounts || fail "0 configfs is not mounted at /sys/kernel/config"
# 1. Up.
check 1 "the daemon is active and the gadget bound to dummy_udc.0" within 60 bash -c "[ \"\$(systemctl is-active lemnosd.service)\" = active ] && [ \"\$(cat $UDC_FILE 2>/dev/null)\" = dummy_udc.0 ]"
check 1 "the host's cdc_ncm interface came up" within 30 host_if
check 1 "systemd's store holds the gadget's files (record, ep0, endpoints)" within 30 store_full
store > /dev/console
board_index="$(index_of lemnos0)"
host_index="$(index_of "$(host_if)")"
say "INFO board lemnos0 index ${board_index:-none}, host $(host_if) index ${host_index:-none}"

# 2. systemctl restart.
old="$(main_pid)"; from="$(log_size)"
watch_start
systemctl restart lemnosd.service
came_back 2 "systemctl restart" "$from" "$old" "${board_index:-none}" "${host_index:-none}"

# 3. SIGKILL to the main process: Restart=always (RestartSec=1s) starts it again, the store kept.
old="$(main_pid)"; from="$(log_size)"
watch_start
systemctl kill --kill-whom=main --signal=SIGKILL lemnosd.service
came_back 3 "SIGKILL" "$from" "$old" "${board_index:-none}" "${host_index:-none}"

# 4. systemctl stop: off the bus, the store emptied.
systemctl stop lemnosd.service
check 4 "systemctl stop: the gadget is off the bus (UDC clear)" within 10 clear_udc
check 4 "systemctl stop: systemd's store is empty" store_empty

# 5. systemctl start: a first start again.
from="$(log_size)"
systemctl start lemnosd.service
check 5 "systemctl start: the gadget is set up and bound afresh" within 60 bash -c "bound() { [ \"\$(cat $UDC_FILE 2>/dev/null)\" = dummy_udc.0 ]; }; bound && tail -c +$((from + 1)) $LOG | grep -q -F '(set up and bound)'"
check 5 "systemctl start: systemd's store holds the gadget's files again" within 30 store_full

say "CASES end"
say "LOG the daemon's log follows"
cat "$LOG" > /dev/console
systemctl poweroff
