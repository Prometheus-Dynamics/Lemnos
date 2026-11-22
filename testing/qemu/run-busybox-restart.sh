#!/usr/bin/env bash
# The restart helper under BusyBox init (RESTART-UNIT, condition 3). A QEMU guest boots the lander's kernel with a static
# BusyBox built with init (testing/qemu/build-busybox-init.sh) as PID 1. /etc/inittab starts the helper in --init mode, as
# the packaged BusyBox init script does, and the guest's cases (testing/qemu/busybox-restart/cases.sh) run as the image's
# own user and group names:
#
#   p0  PID 1 is BusyBox init
#   d1  a member of group lemnos that is not the daemon's user (uid 901) is refused by uid
#   d2  a request that is not exactly verb and nonce is refused
#   p1  the daemon's uid gets the helper's check: ok
#   r   the helper accepts the reboot (the probe's "accepted" line, after the request) and sends SIGTERM to PID 1; the
#       kernel restarts the guest ("reboot: Restarting system" after that line), and QEMU, under -no-reboot, exits 0
#
# The cases name the helper and probe by their paths in the image; a missing path fails by name. Nothing here runs on this
# PC's init or user manager: PID 1 is the guest's. There is no fail-before for this image (the old path is systemd's and has
# no BusyBox form); the case is what the helper does under BusyBox init.
#
# Environment:
#   BUSYBOX_INIT      the static BusyBox with init (required; testing/qemu/build-busybox-init.sh prints its path)
#   CARGO             cargo to build with (default: cargo); built --offline
#   CARGO_TARGET_DIR  where the helper and probe are built (default: the repository's target/)
#   WORK              the work directory, emptied at the start (default: $CARGO_TARGET_DIR/busybox-restart)
#   BUILD             1 builds the helper and the probe (default 1)
#   SLOT_RUN          the slot pool's wrapper; when set, the build and the QEMU boot go through it (6 build, 1 guest)
#   KERNEL, KERNEL_LOCK  the kernel and its tree's lock (default: the lander's, modules-36008ba4fda2e9e0); the boot holds the lock shared
#   BOUND             seconds for the QEMU boot (default 300)
#   MEM               guest memory in MiB (default 256)
#   ACCEL             kvm or tcg (default: kvm when /dev/kvm is writable, else tcg)
#
# Exits 0 when every case passed and QEMU exited 0 after the restart. Exits 1 otherwise.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
src="$here/busybox-restart"
CARGO="${CARGO:-cargo}"
target="${CARGO_TARGET_DIR:-$repo/target}"
WORK="${WORK:-$target/busybox-restart}"
BUILD="${BUILD:-1}"
SLOT_RUN="${SLOT_RUN:-}"
KERNEL="${KERNEL:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0/build/arch/x86/boot/bzImage}"
KERNEL_LOCK="${KERNEL_LOCK:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0.lock}"
BOUND="${BOUND:-300}"
MEM="${MEM:-256}"
if [ -z "${ACCEL:-}" ]; then
	if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=kvm; else ACCEL=tcg; fi
fi
if [ "$ACCEL" = kvm ]; then CPU=host; else CPU=max; fi

die() {
	echo "run-busybox-restart: $*" >&2
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

for tool in qemu-system-x86_64 cpio ldd realpath timeout flock find gzip; do
	command -v "$tool" >/dev/null || die "$tool is needed"
done
[ -n "${BUSYBOX_INIT:-}" ] || die "BUSYBOX_INIT names the static BusyBox with init (testing/qemu/build-busybox-init.sh)"
[ -x "$BUSYBOX_INIT" ] || die "$BUSYBOX_INIT is not an executable"
[ -r "$KERNEL" ] || die "the kernel $KERNEL is not readable"
[ -e "$KERNEL_LOCK" ] || die "the kernel tree's lock $KERNEL_LOCK is not there"
# The applets the image runs must be in the binary. BusyBox init is never started on this host (it is PID 1 only in the
# guest): its applet is found by the string its usage reads from, and setuidgid answers its own usage when built in.
grep -aq '/etc/inittab' "$BUSYBOX_INIT" || die "$BUSYBOX_INIT has no init applet"
probe_out="$("$BUSYBOX_INIT" setuidgid 2>&1 </dev/null || true)"
case "$probe_out" in *"applet not found"*) die "$BUSYBOX_INIT has no setuidgid applet" ;; esac

if [ "$BUILD" = 1 ]; then
	echo "== build the restart helper and its probe"
	pooled 6 env CARGO_TARGET_DIR="$target" TMPDIR="${TMPDIR:-/tmp}" "$CARGO" build --offline -j 6 \
		--manifest-path "$repo/Cargo.toml" -p lemnos-restart -p lemnos-restart-probe
fi
helper="$target/debug/lemnos-restart"
probe="$target/debug/lemnos-restart-probe"
for bin in "$helper" "$probe"; do
	[ -x "$bin" ] || die "$bin is not built"
done

empty() {
	[ -n "$1" ] && [ -d "$1" ] || return 0
	find "$1" -mindepth 1 -delete
}
empty "$WORK"
mkdir -p "$WORK/out"
root="$WORK/root"
mkdir -p "$root"/{bin,sbin,dev,proc,sys,run,tmp,etc,usr/sbin,usr/bin,etc/lemnos-restart}

# inst FILE: copies a file into the guest root at its own path, following links.
inst() {
	local dir rel
	dir="$(realpath "$(dirname "$1")")"
	rel="${dir#/}/$(basename "$1")"
	mkdir -p "$root/${dir#/}"
	cp -L "$1" "$root/$rel"
	chmod 0755 "$root/$rel"
}
# libs_of FILE: copies the libraries ldd lists for an ELF file (the loader's own too).
libs_of() {
	local lib
	while read -r lib; do
		inst "$lib"
	done < <(ldd "$1" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
}

echo "== the guest root: BusyBox init and its applets, the helper and probe with their libraries, the image's table and users"
cp "$BUSYBOX_INIT" "$root/bin/busybox"
chmod 0755 "$root/bin/busybox"
for applet in sh mount mkdir sleep setuidgid; do
	ln -s busybox "$root/bin/$applet"
done
ln -s ../bin/busybox "$root/sbin/init"
# The loader is at /lib64 by its interpreter path, and the libraries are copied under usr/lib64 (as on a usr-merged image).
mkdir -p "$root/usr/lib64"
ln -s usr/lib64 "$root/lib64"
cp "$src/init" "$root/init"
cp "$src/cases.sh" "$root/etc/cases.sh"
cp "$src/inittab" "$root/etc/inittab"
cp "$src/passwd" "$src/group" "$root/etc/"
printf 'passwd: files\ngroup: files\nshadow: files\nhosts: files\n' > "$root/etc/nsswitch.conf"
cp "$helper" "$root/usr/sbin/lemnos-restart"
cp "$probe" "$root/usr/bin/lemnos-restart-probe"
chmod 0755 "$root/init" "$root/etc/cases.sh" "$root/usr/sbin/lemnos-restart" "$root/usr/bin/lemnos-restart-probe"
libs_of "$helper"
libs_of "$probe"
# getpwnam resolves the user through NSS at run time; ldd does not list the module.
inst /usr/lib64/libnss_files.so.2
(cd "$root" && find . -print0 | cpio --null -o -H newc --quiet | gzip -1) > "$WORK/initramfs.cpio"

echo "== boot the guest: the lander's kernel, BusyBox init as PID 1, QEMU -no-reboot (a restart ends it with status 0)"
set +e
pooled 1 flock -s "$KERNEL_LOCK" timeout "$BOUND" qemu-system-x86_64 -accel "$ACCEL" -cpu "$CPU" -smp 1 -m "$MEM" \
	-nographic -no-reboot -kernel "$KERNEL" -initrd "$WORK/initramfs.cpio" \
	-append "console=ttyS0 panic=-1 loglevel=4 printk.time=1" >"$WORK/out/console.log" 2>&1
status=$?
set -e
echo "run-busybox-restart: QEMU status $status (124 is the $BOUND s timeout)"

text="$WORK/out/console.log"
sed -i 's/\r$//' "$text"
failed=0
for c in p0 d1 d2 p1; do
	if grep -q "BUSYBOX-RESTART-GUEST PASS $c " "$text"; then
		grep "BUSYBOX-RESTART-GUEST PASS $c " "$text" | head -1 | sed 's/^/  /'
	else
		echo "  no PASS $c in the console" >&2
		failed=1
	fi
done
if grep -q "BUSYBOX-RESTART-GUEST FAIL" "$text"; then
	grep "BUSYBOX-RESTART-GUEST FAIL" "$text" | sed 's/^/  /' >&2
	failed=1
fi
# r: the probe's "accepted" line after the request, then the kernel's restart after that line. The probe's answer is
# the helper's, so the host reads it, not the guest's own word.
r_line="$(grep -n "BUSYBOX-RESTART-GUEST r the reboot is requested" "$text" | head -1 | cut -d: -f1 || true)"
a_line="$(awk -v s="${r_line:-0}" 'NR > s && /restart-probe: accepted$/ { print NR; exit }' "$text" || true)"
k_line="$(grep -n "reboot: Restarting system" "$text" | head -1 | cut -d: -f1 || true)"
if [ -n "$r_line" ] && [ -n "$a_line" ] && [ -n "$k_line" ] && [ "$k_line" -gt "$a_line" ]; then
	echo "  r: the helper accepted the reboot (line $a_line), and the kernel restarted (line $k_line)"
else
	echo "  r: no accepted-then-restart (request: ${r_line:-none}, accepted: ${a_line:-none}, restart: ${k_line:-none})" >&2
	failed=1
fi
if grep -q "BUSYBOX-RESTART-GUEST r-exit\|BUSYBOX-RESTART-GUEST FAIL r2" "$text"; then
	echo "  r: the guest was still up after the request" >&2
	failed=1
fi
[ "$status" = 0 ] || { echo "  QEMU ended with status $status" >&2; failed=1; }
if [ "$failed" = 0 ]; then
	echo "PASS: every BusyBox case passed; the console is in $text"
	exit 0
fi
echo "FAIL: see $text" >&2
exit 1
