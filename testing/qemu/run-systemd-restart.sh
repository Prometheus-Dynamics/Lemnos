#!/usr/bin/env bash
# The restart helper under a real systemd (RESTART-UNIT). Boots the host's kernel and systemd in QEMU, with the
# helper's shipped socket and service units (packaging/systemd/lemnos-restart.{socket,service}, unchanged), and
# checks what the daemon's user sees: the helper answers, refuses, and restarts the board through the service
# manager, where the old path (a non-root `systemctl reboot`) is refused.
#
# The guest's checks are in systemd-restart/ (guest/checks.sh, units/, etc/). Each prints one line:
#   RESTART-GUEST PASS n TEXT  or  RESTART-GUEST FAIL n TEXT
#   1   as lemnos, `check` gets ok
#   2   as another member of group lemnos, `check` is refused by uid, with the policy text
#   3   a garbage request (an unknown verb, a bad nonce) is refused
#   4   a slow unit is active; its ExecStop sleeps 5 s and TimeoutStopSec is 30 s
#   fail-before  as lemnos, `systemctl reboot` (the old path, no helper) is refused: "Access denied" or
#       "Interactive authentication required", and the system stays up
#   5   as lemnos, `reboot` is accepted, then the system is stopping
# The host then checks (6): the guest exits through the reboot (QEMU's exit status 0 under -no-reboot), the
# helper's "stopping" came before the slow unit's stop began, the slow unit's stop ran its full 5 s on the
# guest's clock, and QEMU exited after that stop ended, at least SLOW_STOP_MIN s after it began on the host's.
#
# Run it as a user unit, with the CPU cap and the file limit the machine needs:
#   systemd-run --user --unit=restart-guest-1 -p LimitNOFILE=1048576 \
#     taskset -c 6-31 testing/qemu/run-systemd-restart.sh
#
# Environment (all optional):
#   CARGO_TARGET_DIR  where the helper and the probe are built (default: the repository's target/)
#   CARGO             cargo to build with (default: cargo); built with --offline --locked
#   KERNEL            the kernel to boot (default: the lander's built kernel, read in place)
#   KERNEL_LOCK       the lock the kernel tree is read under: one shared flock around the one QEMU boot,
#                     taken inside the slot pool's wrapper (default: the lander's modules-36008ba4fda2e9e0.lock)
#   SLOT_RUN          the slot pool's wrapper (`slot-run.sh N cmd...`); when set, the build and the QEMU boot go
#                     through it (6 for the build, 2 for the guest)
#   HELPER_BIN        the helper to put in the guest (default: the one built under CARGO_TARGET_DIR); the control
#                     run puts the helper without its handshake here
#   SYSTEMD           the systemd binary booted as PID 1 (default: /usr/lib/systemd/systemd); its libraries
#                     and its companions (systemd-executor, systemd-shutdown) are copied from the same host
#   WORK              scratch directory for the initramfs and the logs (default: ${TMPDIR:-/tmp}/RST/systemd-restart)
#   BUILD             1 builds the helper and the probe first, 0 uses the ones already built (default 1)
#   BUILD_BOUND       seconds for the build (default 900)
#   BOUND             seconds for the whole QEMU run, boot to exit (default 1500). A run past it fails.
#   MEM               guest memory in MiB (default 1024)
#   ACCEL             kvm or tcg (default: kvm when /dev/kvm is readable and writable, else tcg)
#   SLOW_STOP_MIN    the least stop delay the host accepts, in seconds (default 5: the guest sleeps 5 s)
#
# Exits 0 when every check passes. Exits 1 on any FAIL, on a missing check line, or when QEMU runs past BOUND.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
src="$here/systemd-restart"
CARGO="${CARGO:-cargo}"
KERNEL="${KERNEL:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0/build/arch/x86/boot/bzImage}"
KERNEL_LOCK="${KERNEL_LOCK:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0.lock}"
SLOT_RUN="${SLOT_RUN:-}"
SYSTEMD="${SYSTEMD:-/usr/lib/systemd/systemd}"
WORK="${WORK:-${TMPDIR:-/tmp}/RST/systemd-restart}"
BUILD_BOUND="${BUILD_BOUND:-900}"
BOUND="${BOUND:-1500}"
MEM="${MEM:-1024}"
SLOW_STOP_MIN="${SLOW_STOP_MIN:-5}"
if [ -z "${ACCEL:-}" ]; then
	if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=kvm; else ACCEL=tcg; fi
fi
if [ "$ACCEL" = kvm ]; then CPU=host; else CPU=max; fi
out_dir="$WORK/out"
rm -rf "$WORK"
mkdir -p "$out_dir"
stamped="$out_dir/console-stamped.log"
console="$out_dir/console.log"

die() {
	echo "run-systemd-restart: $*" >&2
	exit 1
}

# pooled N cmd...: the command through the slot pool with N slots, or directly when no pool is named.
pooled() {
	local n="$1"
	shift
	if [ -n "$SLOT_RUN" ]; then
		"$SLOT_RUN" "$n" "$@"
	else
		"$@"
	fi
}

[ -r "$KERNEL" ] || die "the kernel $KERNEL is not readable"
[ -e "$KERNEL_LOCK" ] || die "the kernel tree's lock $KERNEL_LOCK is not there"
[ -x "$SYSTEMD" ] || die "$SYSTEMD is not an executable"
for tool in qemu-system-x86_64 cpio ldd realpath timeout; do
	command -v "$tool" >/dev/null || die "$tool is needed"
done

if [ "${BUILD:-1}" = 1 ]; then
	echo "== build the helper and the probe"
	pooled 6 timeout "$BUILD_BOUND" "$CARGO" build --offline --locked -j 6 --manifest-path "$repo/Cargo.toml" \
		-p lemnos-restart -p lemnos-restart-probe
else
	echo "== the helper and the probe are not rebuilt (BUILD=0)"
fi
target="${CARGO_TARGET_DIR:-$repo/target}"
helper="${HELPER_BIN:-$target/debug/lemnos-restart}"
probe="$target/debug/lemnos-restart-probe"
[ -x "$helper" ] && [ -x "$probe" ] || die "the helper and the probe were not built under $target"

echo "== build the initramfs"
root="$WORK/root"
mkdir -p "$root"/{usr/bin,usr/sbin,usr/lib64,usr/lib/systemd/system,usr/lib/lemnos-guest,etc/systemd/system,etc/dbus-1,dev,proc,sys,run,tmp,root,var/lib/dbus}
for link in bin:usr/bin sbin:usr/sbin lib:usr/lib lib64:usr/lib64; do
	ln -s "${link#*:}" "$root/${link%%:*}"
done
ln -s bash "$root/usr/bin/sh"
# The units the packaging enables: the helper's socket, and the guest's own units. The journal's sockets are
# started with the others, as a real system starts them, so the helper's own log lines reach the console.
mkdir -p "$root/usr/lib/systemd/system/sockets.target.wants" "$root/usr/lib/systemd/system/multi-user.target.wants"
ln -s ../lemnos-restart.socket "$root/usr/lib/systemd/system/sockets.target.wants/lemnos-restart.socket"
for unit in systemd-journald.socket systemd-journald-dev-log.socket; do
	ln -s "../$unit" "$root/usr/lib/systemd/system/sockets.target.wants/$unit"
done

# Copies a file from the host to the same name in the guest. The directory is resolved (lib64 is usr/lib64) and the
# file's contents are copied under the name asked for, so a soname link keeps its soname.
inst() {
	local dir rel
	dir="$(realpath "$(dirname "$1")")"
	rel="${dir#/}/$(basename "$1")"
	mkdir -p "$root/${dir#/}"
	cp -L "$1" "$root/$rel"
	chmod 0755 "$root/$rel"
}

# Copies an ELF file and the libraries ldd lists for it (the loader's own too).
inst_elf() {
	inst "$1"
	local lib
	while read -r lib; do
		inst "$lib"
	done < <(ldd "$1" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
}

for bin in "$SYSTEMD" "$(dirname "$SYSTEMD")/systemd-executor" "$(dirname "$SYSTEMD")/systemd-shutdown" \
	/usr/bin/systemctl /usr/bin/dbus-daemon /usr/bin/setpriv /usr/bin/bash /usr/bin/timeout \
	/usr/bin/sleep /usr/bin/true /usr/bin/head /usr/bin/tr /usr/bin/grep /usr/bin/mkdir /usr/bin/cat \
	/usr/bin/mount /usr/bin/chroot /usr/bin/cp /usr/bin/journalctl /usr/bin/busctl /usr/bin/readlink /usr/bin/taskset /usr/bin/socat /usr/bin/cut /usr/lib/systemd/systemd-journald; do
	inst_elf "$bin"
done
inst_elf "$helper"
cp "$helper" "$root/usr/bin/lemnos-restart"
cp "$probe" "$root/usr/bin/lemnos-restart-probe"
chmod 0755 "$root/usr/bin/lemnos-restart" "$root/usr/bin/lemnos-restart-probe"
inst /usr/lib64/libnss_files.so.2
[ -e "$root/usr/bin/sh" ] || die "bash did not come into the guest"
# The packaged units, as shipped, and the guest's own units and checks.
for unit in "$repo/packaging/systemd/lemnos-restart.socket" "$repo/packaging/systemd/lemnos-restart.service"; do
	cp "$unit" "$root/usr/lib/systemd/system/"
done
for unit in basic sysinit multi-user sockets paths timers shutdown reboot poweroff halt kexec final umount \
	local-fs local-fs-pre swap; do
	cp "$(realpath /usr/lib/systemd/system/$unit.target)" "$root/usr/lib/systemd/system/$unit.target"
done
for unit in systemd-reboot.service systemd-journald.service systemd-journald.socket systemd-journald-dev-log.socket; do
	cp "$(realpath "/usr/lib/systemd/system/$unit")" "$root/usr/lib/systemd/system/$unit"
done
cp "$src"/units/*.service "$src"/units/*.socket "$root/usr/lib/systemd/system/"
# The guest bus: the socket listens at boot and the service starts with it (PID 1 connects to a running bus only).
ln -s ../dbus.socket "$root/usr/lib/systemd/system/sockets.target.wants/dbus.socket"
ln -s ../dbus.service "$root/usr/lib/systemd/system/multi-user.target.wants/dbus.service"
ln -s multi-user.target "$root/usr/lib/systemd/system/default.target"
for unit in lemnos-guest-slow lemnos-guest-driver; do
	ln -s "../$unit.service" "$root/usr/lib/systemd/system/multi-user.target.wants/$unit.service"
done
cp "$src/guest/checks.sh" "$src/guest/slow-stop.sh" "$root/usr/lib/lemnos-guest/"
cp "$src/guest/init" "$root/init"
chmod 0755 "$root/init"
chmod 0755 "$root/usr/lib/lemnos-guest/checks.sh" "$root/usr/lib/lemnos-guest/slow-stop.sh"
cp "$src/etc/passwd" "$src/etc/group" "$src/etc/nsswitch.conf" "$src/etc/machine-id" "$src/etc/os-release" "$root/etc/"
cp "$src/etc/dbus-system.conf" "$root/etc/dbus-1/system.conf"
(cd "$root" && find . -print0 | cpio --null -o -H newc --quiet) > "$WORK/initramfs.cpio"
rm -rf "$root"

echo "== boot $KERNEL with systemd ($ACCEL), bound ${BOUND}s, the kernel read under a shared lock for this one boot"
cmdline="console=ttyS0 loglevel=4 panic=5 systemd.log_target=console systemd.log_level=info"
# The console's lines are stamped with the host's clock as they arrive, so the stop can be timed from outside.
stamp() {
	local line
	while IFS= read -r line || [ -n "$line" ]; do
		printf '%s %s\n' "$EPOCHREALTIME" "${line%$'\r'}"
	done
	printf '%s EXIT\n' "$EPOCHREALTIME"
}
set +e
pooled 2 flock -s "$KERNEL_LOCK" timeout --kill-after=30 "$BOUND" qemu-system-x86_64 -accel "$ACCEL" -cpu "$CPU" -smp 2 -m "$MEM" \
	-nodefaults -display none -monitor none -serial stdio -no-reboot \
	-kernel "$KERNEL" -initrd "$WORK/initramfs.cpio" -append "$cmdline" </dev/null 2>&1 | stamp > "$stamped"
qstatus=${PIPESTATUS[0]}
set -e
sed -E 's/^[0-9]+\.[0-9]+ //' "$stamped" > "$console"
echo "QEMU status $qstatus; console in $console"

verdict=0
bad() {
	echo "FAIL: $*"
	verdict=1
}
if [ "$qstatus" -eq 124 ] || [ "$qstatus" -eq 137 ]; then
	bad "QEMU ran past BOUND=${BOUND}s without the reboot (status $qstatus)"
fi

echo "== the guest's checks"
systemd_line="$(grep -m1 '^RESTART-GUEST SYSTEMD ' "$console" || true)"
echo "${systemd_line:-"FAIL: no systemd version line from the guest"}"
[ -n "$systemd_line" ] || verdict=1
grep -E '^RESTART-GUEST (PASS|FAIL) ' "$console" || true
grep -E '^RESTART-GUEST OLD-PATH ' "$console" || true
for n in 1 2 3 4 5; do
	if grep -q "^RESTART-GUEST PASS $n " "$console"; then
		:
	else
		bad "check $n has no PASS line from the guest"
	fi
done
grep -q '^RESTART-GUEST PASS fail-before ' "$console" || bad "fail-before has no PASS line from the guest"
if grep -q '^RESTART-GUEST FAIL' "$console"; then
	bad "the guest reports a FAIL (above)"
fi

echo "== the host's checks (6)"
# The stamped lines are "EPOCH TEXT": the first field is the host's time of arrival. Times below are seconds
# after the first line's arrival.
zero="$(awk 'NR == 1 { print $1; exit }' "$stamped")"
stop_t="$(awk '/REBOOT-LINES.*stopping/ { print $1; exit }' "$stamped")"
begin_t="$(awk '/RESTART-GUEST SLOW-STOP-BEGIN/ { print $1; exit }' "$stamped")"
end_t="$(awk '/RESTART-GUEST SLOW-STOP-END/ { print $1; exit }' "$stamped")"
exit_t="$(awk '$2 == "EXIT" { print $1; exit }' "$stamped")"
begin_up="$(grep -m1 'SLOW-STOP-BEGIN uptime=' "$console" | sed -E 's/.*uptime=//' || true)"
end_up="$(grep -m1 'SLOW-STOP-END uptime=' "$console" | sed -E 's/.*uptime=//' || true)"
panic="$(grep -c 'Kernel panic' "$console" || true)"
if [ -z "$stop_t" ] || [ -z "$begin_t" ] || [ -z "$end_t" ] || [ -z "$exit_t" ] || [ -z "$begin_up" ] || [ -z "$end_up" ]; then
	bad "6: the reboot's lines are missing (stopping, the slow stop's begin and end, or the exit)"
else
	rel() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%.3f", b - a }'; }
	# The helper's "stopping" is read after StartUnit returns, which the manager answers once the reboot job is queued, so
	# the slow unit's stop may begin a little before the line reaches the console. The check is that the answer came before
	# the stop ended, and that the exit came after the stop.
	order_ok=$(awk -v s="$stop_t" -v e="$end_t" -v x="$exit_t" 'BEGIN { print (s <= e && e <= x) ? "yes" : "no" }')
	guest_delay=$(awk -v b="$begin_up" -v e="$end_up" 'BEGIN { printf "%.3f", e - b }')
	host_delay=$(rel "$begin_t" "$exit_t")
	echo "host (+s from the first console line): stopping at $(rel "$zero" "$stop_t"), the slow stop began at $(rel "$zero" "$begin_t") and ended at $(rel "$zero" "$end_t"), QEMU exited at $(rel "$zero" "$exit_t")"
	echo "host: the slow stop took ${guest_delay}s on the guest's clock; QEMU exited $(rel "$end_t" "$exit_t")s after the stop ended, $host_delay s after it began"
	if [ "$order_ok" != yes ]; then
		bad "6: the order is wrong: the helper's stopping after the slow stop ended, or the exit before it"
	fi
	if awk -v g="$guest_delay" -v m="$SLOW_STOP_MIN" 'BEGIN { exit !(g >= m) }'; then :; else
		bad "6: the slow unit's stop took ${guest_delay}s on the guest's clock, less than SLOW_STOP_MIN=${SLOW_STOP_MIN}s"
	fi
	if awk -v h="$host_delay" -v m="$SLOW_STOP_MIN" 'BEGIN { exit !(h >= m) }'; then :; else
		bad "6: QEMU exited ${host_delay}s after the stop began on the host, less than SLOW_STOP_MIN=${SLOW_STOP_MIN}s"
	fi
	if [ "$qstatus" -ne 0 ]; then
		bad "6: QEMU did not exit 0 through the reboot (status $qstatus)"
	fi
	if [ "$panic" -ne 0 ]; then
		bad "6: the guest panicked"
	fi
	if [ "$verdict" -eq 0 ]; then
		echo "PASS 6 the guest exited through the reboot after the slow unit's stop, and the helper said stopping first"
	fi
fi

if [ "$verdict" -eq 0 ]; then
	echo "PASS: every check passed (log: $console)"
else
	echo "FAIL: see the lines above (log: $console)"
fi
exit "$verdict"
