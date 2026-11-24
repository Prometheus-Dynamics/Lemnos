#!/usr/bin/env bash
# The USB gadget across a restart under a real systemd (GADGET-KEEP). A QEMU guest boots with systemd as PID 1, the
# packaged lemnosd.service and its packaged gadget drop-in (packaging/systemd/lemnosd.service.d/gadget.conf), both
# unchanged, and a board file with the gadget on dummy_udc.0. The guest's checks (gadget-keep/checks.sh) restart the
# daemon with `systemctl restart`, kill its main process, stop it and start it, and read systemd's file descriptor store
# with `systemd-analyze fdstore`: the store survives the restart and the kill (the gadget stays on the bus, its
# network interfaces keep their indexes), and a stop empties it and takes the gadget off the bus.
#
# The QEMU harness (testing/qemu/run.sh, orion-link) checks the same daemon under a stand-in service manager, with
# traffic over the link; this script checks the real one. It is not part of the lander's run.
#
# Run it as a user unit, with the CPU cap and the file limit the machine needs:
#   systemd-run --user --unit=w-gadget-keep -p LimitNOFILE=1048576 \
#     taskset -c 6-31 env KERNEL=... KERNEL_LOCK=... testing/qemu/run-gadget-keep.sh
#
# Environment:
#   KERNEL            the kernel to boot (required): built from testing/qemu/kernel.config, which has dummy_hcd, the
#                     configfs gadget with FunctionFS and NCM, and the host's cdc_ncm
#   KERNEL_LOCK       a lock held shared around the boot (required): the kernel tree's
#   SLOT_RUN          the slot pool's wrapper; when set, the build and the QEMU boot go through it (6 build, 2 guest)
#   CARGO_TARGET_DIR  where lemnosd is built (default: the repository's target/)
#   CARGO             cargo to build with (default: cargo); built --offline
#   BUILD             1 builds lemnosd (default 1)
#   LEMNOSD_BIN       the daemon to boot (default: the one built here, release)
#   WORK              the work directory, emptied at the start (default: $CARGO_TARGET_DIR/gadget-keep)
#   BOUND             seconds for the QEMU boot (default 600)
#   MEM               guest memory in MiB (default 1024)
#   ACCEL             kvm or tcg (default: kvm when /dev/kvm is writable, else tcg)
#
# Exits 0 when every check passed and the guest powered off. Exits 1 otherwise.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
src="$here/gadget-keep"
etc="$here/lemnosd-restart/etc"
CARGO="${CARGO:-cargo}"
target="${CARGO_TARGET_DIR:-$repo/target}"
WORK="${WORK:-$target/gadget-keep}"
BUILD="${BUILD:-1}"
SLOT_RUN="${SLOT_RUN:-}"
BOUND="${BOUND:-600}"
MEM="${MEM:-1024}"
if [ -z "${ACCEL:-}" ]; then
	if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=kvm; else ACCEL=tcg; fi
fi
if [ "$ACCEL" = kvm ]; then CPU=host; else CPU=max; fi

die() {
	echo "run-gadget-keep: $*" >&2
	exit 1
}

empty() {
	[ -n "$1" ] && [ -d "$1" ] || return 0
	find "$1" -mindepth 1 -delete
}

pooled() {
	local n="$1"
	shift
	if [ -n "$SLOT_RUN" ]; then
		"$SLOT_RUN" "$n" "$@"
	else
		"$@"
	fi
}

[ -n "${KERNEL:-}" ] || die "KERNEL is needed: a kernel built from testing/qemu/kernel.config"
[ -n "${KERNEL_LOCK:-}" ] || die "KERNEL_LOCK is needed: the kernel tree's lock"
for tool in qemu-system-x86_64 cpio ldd realpath timeout flock find; do
	command -v "$tool" >/dev/null || die "$tool is needed"
done
[ -r "$KERNEL" ] || die "the kernel $KERNEL is not readable"
[ -e "$KERNEL_LOCK" ] || die "the kernel tree's lock $KERNEL_LOCK is not there"
systemd_dir="$(dirname "$(realpath /usr/lib/systemd/systemd)")"

if [ "$BUILD" = 1 ]; then
	echo "== build lemnosd"
	pooled 6 env CARGO_TARGET_DIR="$target" TMPDIR="${TMPDIR:-/tmp}" "$CARGO" build --offline -j 6 \
		--manifest-path "$repo/Cargo.toml" --release -p lemnosd
fi
LEMNOSD_BIN="${LEMNOSD_BIN:-$target/release/lemnosd}"
[ -x "$LEMNOSD_BIN" ] || die "$LEMNOSD_BIN is not built"
empty "$WORK"
mkdir -p "$WORK/out"

echo "== the guest root (initramfs): systemd, the packaged unit and its gadget drop-in, the daemon, the checks"
root="$WORK/root"
mkdir -p "$root"/{usr/bin,usr/sbin,usr/lib64,usr/lib/systemd/system,usr/lib/lemnos-guest,etc/systemd/system/lemnosd.service.d,etc/lemnos,dev,proc,sys,run,tmp,root}
for link in bin:usr/bin sbin:usr/sbin lib:usr/lib lib64:usr/lib64; do
	ln -s "${link#*:}" "$root/${link%%:*}"
done
ln -s bash "$root/usr/bin/sh"
inst() {
	local dir rel
	dir="$(realpath "$(dirname "$1")")"
	rel="${dir#/}/$(basename "$1")"
	mkdir -p "$root/${dir#/}"
	cp -L "$1" "$root/$rel"
	chmod 0755 "$root/$rel"
}
libs_of() {
	local lib
	while read -r lib; do
		inst "$lib"
	done < <(ldd "$1" 2>/dev/null | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
}
for bin in /usr/lib/systemd/systemd "$systemd_dir/systemd-executor" "$systemd_dir/systemd-shutdown" \
	/usr/bin/systemctl /usr/bin/systemd-analyze /usr/bin/bash /usr/bin/sleep /usr/bin/cat /usr/bin/grep /usr/bin/tail \
	/usr/bin/head /usr/bin/stat /usr/bin/readlink /usr/bin/basename /usr/bin/mount /usr/bin/chroot /usr/bin/cp \
	/usr/bin/mkdir /usr/bin/ls; do
	inst "$bin"
	libs_of "$bin"
done
libs_of "$LEMNOSD_BIN"
cp "$LEMNOSD_BIN" "$root/usr/bin/lemnosd"
chmod 0755 "$root/usr/bin/lemnosd"
inst /usr/lib64/libnss_files.so.2
# systemd loads libseccomp at run time for the unit's SystemCallFilter=: without it the filter is ignored.
inst /usr/lib64/libseccomp.so.2

# The packaged daemon unit and its gadget drop-in, as shipped; the test's drop-in for its log.
cp "$repo/packaging/systemd/lemnosd.service" "$root/usr/lib/systemd/system/"
cp "$repo/packaging/systemd/lemnosd.service.d/gadget.conf" "$root/etc/systemd/system/lemnosd.service.d/gadget.conf"
cp "$src/30-gadget-keep-log.conf" "$root/etc/systemd/system/lemnosd.service.d/"
for unit in basic sysinit multi-user sockets paths timers shutdown poweroff final umount local-fs local-fs-pre swap; do
	cp "$(realpath /usr/lib/systemd/system/$unit.target)" "$root/usr/lib/systemd/system/$unit.target"
done
cp "$(realpath /usr/lib/systemd/system/systemd-poweroff.service)" "$root/usr/lib/systemd/system/"
cp "$src/lemnos-gadget-keep-checks.service" "$root/usr/lib/systemd/system/"
mkdir -p "$root/usr/lib/systemd/system/multi-user.target.wants"
ln -s ../lemnosd.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnosd.service"
ln -s ../lemnos-gadget-keep-checks.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnos-gadget-keep-checks.service"
ln -s multi-user.target "$root/usr/lib/systemd/system/default.target"
cp "$src/checks.sh" "$root/usr/lib/lemnos-guest/checks.sh"
chmod 0755 "$root/usr/lib/lemnos-guest/checks.sh"
cp "$src/init" "$root/init"
chmod 0755 "$root/init"
cp "$etc/passwd" "$etc/group" "$etc/nsswitch.conf" "$etc/machine-id" "$root/etc/"
printf 'NAME="Gadget keep guest"\nID=gadget-keep-guest\n' > "$root/etc/os-release"
cat > "$root/etc/lemnos/board.toml" <<'BOARD'
format = "lemnos.board"
schema_version = 1

[board]
id = "gadget-keep"

[[devices]]
id = "identity"
driver = "system-identity"
config = { serial = "cmdline", serial_key = "lemnos.serial" }

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc:dummy_udc.0" }
config = { manufacturer = "Lemnos", product_name = "Gadget keep check", identity = "identity" }
BOARD
(cd "$root" && find . -print0 | cpio --null -o -H newc --quiet) > "$WORK/initramfs.cpio"
empty "$root"
rmdir "$root"

stamped="$WORK/out/console-stamped.log"
console="$WORK/out/console.log"
stamp() {
	local line
	while IFS= read -r line || [ -n "$line" ]; do
		printf '%s %s\n' "$EPOCHREALTIME" "${line%$'\r'}"
	done
}
cmdline="console=ttyS0 loglevel=4 panic=5 systemd.log_target=console systemd.log_level=info lemnos.serial=0123456789abcdef"
echo "== boot: $KERNEL under systemd ($ACCEL), bound ${BOUND}s"
set +e
pooled 2 flock -s "$KERNEL_LOCK" timeout --kill-after=30 "$BOUND" qemu-system-x86_64 -accel "$ACCEL" -cpu "$CPU" \
	-smp 2 -m "$MEM" -nodefaults -display none -monitor none -serial stdio -no-reboot \
	-kernel "$KERNEL" -initrd "$WORK/initramfs.cpio" -append "$cmdline" </dev/null 2>&1 | stamp > "$stamped"
status=${PIPESTATUS[0]}
set -e
sed -E 's/^[0-9]+\.[0-9]+ //' "$stamped" > "$console"
echo "QEMU status $status; console in $console"

verdict=0
bad() {
	echo "FAIL: $*"
	verdict=1
}
grep -a '^GADGET-KEEP-GUEST ' "$console" || true
grep -a -q '^GADGET-KEEP-GUEST CASES end' "$console" || bad "the guest's checks did not reach their end"
for n in 1 2 3 4 5; do
	grep -a -q "^GADGET-KEEP-GUEST PASS $n " "$console" || bad "check $n has no PASS line"
done
if grep -a -q '^GADGET-KEEP-GUEST FAIL' "$console"; then bad "the guest reports a FAIL (above)"; fi
if [ "$status" -ne 0 ]; then bad "QEMU did not exit 0 through the guest's power-off (status $status)"; fi
if grep -a -q 'Kernel panic' "$console"; then bad "the guest panicked"; fi
# The unit's system call filter must be the one systemd installs, not ignored.
if grep -a -q 'libseccomp is not available' "$console"; then bad "systemd ignored the unit's SystemCallFilter (no libseccomp in the guest)"; fi
if [ "$verdict" -eq 0 ]; then
	echo "PASS: every check passed (logs: $WORK/out)"
else
	echo "FAIL: see the lines above (logs: $WORK/out)"
fi
exit "$verdict"
