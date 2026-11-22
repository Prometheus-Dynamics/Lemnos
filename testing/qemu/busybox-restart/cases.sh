#!/bin/sh
# The BusyBox guest's restart cases (RESTART-UNIT, condition 3): run once by BusyBox init, after /etc/inittab has started
# the restart helper in --init mode. Everything goes to the console. The host reads the lines
#   BUSYBOX-RESTART-GUEST PASS|FAIL <case> TEXT
exec >/dev/console 2>&1

say() { echo "BUSYBOX-RESTART-GUEST $*"; }
HELPER=/usr/sbin/lemnos-restart
PROBE=/usr/bin/lemnos-restart-probe
SOCK=/run/lemnos-restart.sock
LEMNOS=900   # the daemon's user (the image's passwd names it lemnos)

# The paths the cases name must be in this image; a missing one fails by name, not as a refused request.
missing=""
for f in "$HELPER" "$PROBE" /bin/sh /sbin/init /bin/setuidgid /bin/mount; do
	[ -e "$f" ] || missing="$missing $f"
done
if [ -n "$missing" ]; then
	say "FAIL image: not in this image:$missing"
	exit 0
fi

read -r comm < /proc/1/comm
if [ "$comm" = init ]; then
	say "PASS p0 PID 1 is BusyBox init"
else
	say "FAIL p0 PID 1 is '$comm', not init"
fi

i=0
while [ $i -lt 60 ] && [ ! -S "$SOCK" ]; do sleep 1; i=$((i + 1)); done
if [ ! -S "$SOCK" ]; then
	say "FAIL p1 the helper's socket $SOCK did not come up in 60 s"
	exit 0
fi

# d1: a member of group lemnos that is not the daemon's user connects (the socket is group lemnos) and is refused by uid.
out=$(setuidgid other "$PROBE" check 2>&1); rc=$?
case "$out" in
*"may not restart this board"*) refused=yes ;;
*) refused=no ;;
esac
if [ "$rc" -ne 0 ] && [ "$refused" = yes ]; then
	say "PASS d1 uid 901 in group lemnos is refused: $out"
else
	say "FAIL d1 uid 901 in group lemnos was not refused (rc=$rc): $out"
fi

# d2: a request that is not exactly verb and nonce is refused, as the daemon's uid.
# (the raw form prints the helper's answer and exits 0, so the answer is what is checked)
out=$(setuidgid lemnos "$PROBE" raw "frobnicate 1234" 2>&1)
case "$out" in *"answer: refused"*) refused=yes ;; *) refused=no ;; esac
if [ "$refused" = yes ]; then
	say "PASS d2 raw 'frobnicate 1234' is refused: $out"
else
	say "FAIL d2 raw 'frobnicate 1234' was not refused: $out"
fi

# p1: the daemon's uid gets the helper's check, and the helper answers from the BusyBox init path.
out=$(setuidgid lemnos "$PROBE" check 2>&1); rc=$?
case "$out" in *"check: ok"*) ok=yes ;; *) ok=no ;; esac
if [ "$rc" -eq 0 ] && [ "$ok" = yes ]; then
	say "PASS p1 the helper's check as uid 900 answers ok under BusyBox init"
else
	say "FAIL p1 the helper's check as uid 900 (rc=$rc): $out"
fi

# r: the helper's reboot, as uid 900. The probe's own answer goes to the console as it comes (its "accepted" line is
# printed when the helper accepts, before the helper sends SIGTERM to PID 1). Init's SIGTERM then ends this shell with
# the rest, so the host reads the verdict from the console: "accepted", then the kernel's restart (QEMU -no-reboot exits 0).
say "r the reboot is requested as uid $LEMNOS; the probe's answer follows"
setuidgid lemnos "$PROBE" reboot
say "r-exit $? (printed only when the restart did not end the guest)"
sleep 60
say "FAIL r2 the guest is still up 60 s after the reboot was requested"
