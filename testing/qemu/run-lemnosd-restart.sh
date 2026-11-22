#!/usr/bin/env bash
# The daemon's restart cases under the packaged units (RESTART-UNIT). A QEMU guest boots the lander's kernel with
# systemd as PID 1. Its lemnosd is started by the packaged lemnosd.service (shipped unchanged, with a test drop-in for
# the boot chain), the restart helper by the packaged lemnos-restart.socket and .service, and the system-slots driver
# is configured on a bay boot partition on the guest disk, with an installed slot. The daemon's user is lemnos, uid 900
# (the guest's passwd); the helper's policy takes uid 900 and root. The guest's cases (guest/cases.sh) run on that daemon:
#
#   d  a peer in group lemnos that is not uid 900 or root is refused; a request that is not `<verb> <nonce>` is refused
#   c  StartUnit fails on the helper's connection (reboot.target's transaction is cyclic): the helper says accepted, then
#      failed; the trial is disarmed, the state is restart-failed with the reason, and the caller gets the error
#   b  accepted, then no stop request within the helper's bound (reboot.target waits on a unit that never starts): the
#      trial is disarmed and the state says the bound passed
#   a  accepted, then SIGTERM: the reboot is under way, so the trial stays armed; a slow unit stops for 5 s first. The
#      guest then goes through the reboot (QEMU exits 0 under -no-reboot). The host verifies the boot partition, then a
#      second boot on the same disk (rst.stage=2) reads the state back: the trial is still on trial.
#
# The guest's pieces are in testing/qemu/lemnosd-restart/ (init, cases, units, drop-ins). The kernel is the lander's,
# read in place under a shared flock on its tree's lock, for each boot.
#
# Environment (all optional):
#   SLOT_RUN          the slot pool's wrapper; when set, the build and each QEMU boot go through it (6 build, 2 guest)
#   CARGO_TARGET_DIR  where the binaries are built (default: the repository's target/)
#   CARGO             cargo to build with (default: cargo); built --offline
#   WORK              the work directory, emptied at the start (default: $CARGO_TARGET_DIR/lemnosd-restart)
#   BUILD             1 builds lemnosd (testing-boot-roots), lemnos-ctl, the helper, the probe and the checker (default 1)
#   LEMNOSD_BIN       the daemon to boot (default: the one built here)
#   KERNEL, KERNEL_LOCK  the kernel and its tree's lock (default: the lander's, modules-36008ba4fda2e9e0)
#   BOUND             seconds for each QEMU boot (default 900)
#   MEM               guest memory in MiB (default 1024)
#   ACCEL             kvm or tcg (default: kvm when /dev/kvm is writable, else tcg)
#   DAEMON_PROFILE    the cargo profile the daemon is built with (default release: the debug build is too slow for the
#                     driver's call deadlines, a write of the last chunk times out)
#
# Exits 0 when every case passed and the boot partition verifies. Exits 1 otherwise.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
src="$here/lemnosd-restart"
CARGO="${CARGO:-cargo}"
target="${CARGO_TARGET_DIR:-$repo/target}"
WORK="${WORK:-$target/lemnosd-restart}"
BUILD="${BUILD:-1}"
KERNEL="${KERNEL:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0/build/arch/x86/boot/bzImage}"
KERNEL_LOCK="${KERNEL_LOCK:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0.lock}"
SLOT_RUN="${SLOT_RUN:-}"
DAEMON_PROFILE="${DAEMON_PROFILE:-release}"
BOUND="${BOUND:-900}"
MEM="${MEM:-1024}"
if [ -z "${ACCEL:-}" ]; then
	if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=kvm; else ACCEL=tcg; fi
fi
if [ "$ACCEL" = kvm ]; then CPU=host; else CPU=max; fi

die() {
	echo "run-lemnosd-restart: $*" >&2
	exit 1
}

# empty DIR: removes what is inside DIR (not DIR), when DIR is a directory. The directory is checked first.
empty() {
	[ -n "$1" ] && [ -d "$1" ] || return 0
	find "$1" -mindepth 1 -delete
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

for tool in qemu-system-x86_64 cpio ldd realpath timeout mke2fs sfdisk dd flock truncate find; do
	command -v "$tool" >/dev/null || die "$tool is needed"
done
[ -r "$KERNEL" ] || die "the kernel $KERNEL is not readable"
[ -e "$KERNEL_LOCK" ] || die "the kernel tree's lock $KERNEL_LOCK is not there"

if [ "$BUILD" = 1 ]; then
	echo "== build lemnosd (testing-boot-roots), the helper, the probe, the image builder and the checker"
	pooled 6 env CARGO_TARGET_DIR="$target" TMPDIR="${TMPDIR:-/tmp}" "$CARGO" build --offline -j 6 \
		--manifest-path "$repo/Cargo.toml" --profile "$DAEMON_PROFILE" -p lemnosd --features testing-boot-roots
	pooled 6 env CARGO_TARGET_DIR="$target" TMPDIR="${TMPDIR:-/tmp}" "$CARGO" build --offline -j 6 \
		--manifest-path "$repo/Cargo.toml" -p lemnos-ctl -p lemnos-restart -p lemnos-restart-probe -p lemnos-restart-lemnosd-check
fi
LEMNOSD_BIN="${LEMNOSD_BIN:-$target/$DAEMON_PROFILE/lemnosd}"
ctl="$target/debug/lemnos-ctl"
helper="$target/debug/lemnos-restart"
probe="$target/debug/lemnos-restart-probe"
check="$target/debug/lemnos-restart-lemnosd-check"
for bin in "$LEMNOSD_BIN" "$ctl" "$helper" "$probe" "$check"; do
	[ -x "$bin" ] || die "$bin is not built"
done
socs="$repo/socs"
empty "$WORK"
mkdir -p "$WORK/out"
cd "$WORK"

echo "== the board file (the reference board's bays, a tryboot slot device with its boot chain stood in)"
cat > board.toml <<'BOARD'
format = "lemnos.board"
schema_version = 1

[board]
id = "lemnosd-restart"
soc = "bcm2712-rp1"

[host]
dma_heaps = []

[boot]
bay_kernel_bytes = 11534336
bay_initramfs_bytes = 1048576
bay_device_tree_bytes = 90112
bay_cmdline_bytes = 4096
bay_overlay_bytes = 4096
device_tree_names = ["bcm2712-rpi-5-b.dtb"]
firmware_files = ["overlay_map.dtb", "hat_map.dtb"]
firmware_filters = ["pi5"]

[boot.eeprom]
build = "2026-05-12"
max_restarts = "not set"
reboot_on_fatal_error = true

[[devices]]
id = "slots"
driver = "system-slots"
config = { data = "/data", boot = "/boot/firmware", record = "tryboot", boot_sources = ["/soc/mmc@7e202000=/soc/mmc@7e202000"], partuuid = "5e11a0a0-0002", boot_args = "os", reserve_mb = 8, keep = 3, device_tree = "/usr/lib/lemnos-guest/no-device-tree", vcio = "/usr/lib/lemnos-guest/no-device-tree" }
BOARD

echo "== the payloads, the rootfs stand-in and the bundle (lemnos-ctl bundle, the same writer a sender uses)"
"$check" payload "$WORK/payload-base" base-1
"$check" payload "$WORK/payload-install" install-1
"$check" rootfs "$WORK/rootfs.erofs" "$WORK/verity.txt" 512
"$ctl" bundle --socs "$socs" --layout-of "$WORK/board.toml" --slot install-1 --payload "$WORK/payload-install" --firmware "$WORK/firmware" \
	--rootfs "$WORK/rootfs.erofs" --verity-table "$WORK/verity.txt" --product restart-os --version 1.0 \
	--out "$WORK/bundle.tar" > "$WORK/out/bundle.txt" 2>&1 || { cat "$WORK/out/bundle.txt"; die "lemnos-ctl bundle refused the bundle"; }
"$ctl" bundle --verify "$WORK/bundle.tar" --socs "$socs" --layout-of "$WORK/board.toml" > "$WORK/out/bundle-verify.txt" 2>&1 \
	|| { cat "$WORK/out/bundle-verify.txt"; die "the bundle does not verify"; }

echo "== the boot partition: the base slot in bay set 0 (the install goes to a free bay)"
min_text="$("$ctl" boot-partition --socs "$socs" --layout-of "$WORK/board.toml" --payload "$WORK/payload-base" --firmware "$WORK/firmware" --min-size)" || die "boot-partition --min-size refused"
min="${min_text%%$'\n'*}"
bootbytes=$(( (min + 511) / 512 * 512 ))
"$ctl" boot-partition --socs "$socs" --layout-of "$WORK/board.toml" --slot base-1 --payload "$WORK/payload-base" --firmware "$WORK/firmware" \
	--size "$bootbytes" --out "$WORK/boot.img" > "$WORK/out/boot-partition.txt" 2>&1 \
	|| { cat "$WORK/out/boot-partition.txt"; die "lemnos-ctl boot-partition refused the image"; }
"$ctl" boot-partition --socs "$socs" --verify "$WORK/boot.img" --layout-of "$WORK/board.toml" > "$WORK/out/boot-verify-before.txt" 2>&1 \
	|| { cat "$WORK/out/boot-verify-before.txt"; die "the new boot partition does not verify"; }

echo "== the disk: MBR, partition 1 the boot partition (FAT), partition 2 the data partition (ext4)"
boot_sectors=$((bootbytes / 512))
p1=2048
p2=$((p1 + boot_sectors + 2048 - ((p1 + boot_sectors) % 2048)))
data_sectors=$((256 * 2048))
disk_sectors=$((p2 + data_sectors))
truncate -s $((disk_sectors * 512)) disk.img
sfdisk --no-reread -q disk.img <<EOF
label: dos
start=$p1, size=$boot_sectors, type=c
start=$p2, size=$data_sectors, type=83
EOF
dd if=boot.img of=disk.img bs=512 seek=$p1 conv=notrunc status=none
mkdir -p datatree/board/slots datatree/slots/.incoming
truncate -s $((data_sectors * 512)) data.img
mke2fs -q -F -t ext4 -b 4096 -I 256 -m 0 -O meta_bg,^resize_inode -E root_owner=0:0 -L lemnos-data \
	-d datatree data.img > "$WORK/out/mke2fs.txt" 2>&1 || { cat "$WORK/out/mke2fs.txt"; die "mke2fs refused the data partition"; }
dd if=data.img of=disk.img bs=512 seek=$p2 conv=notrunc status=none

echo "== the guest root (initramfs): systemd, the packaged units and their drop-ins, the daemon, the helper, the checks"
root="$WORK/root"
mkdir -p "$root"/{usr/bin,usr/sbin,usr/lib64,usr/lib/systemd/system,usr/lib/lemnos-guest/boot-roots,etc/systemd/system/lemnosd.service.d,etc/lemnos,etc/dbus-1,dev,proc,sys,run,tmp,root,var/lib/lemnos-restart,data,boot/firmware}
for link in bin:usr/bin sbin:usr/sbin lib:usr/lib lib64:usr/lib64; do
	ln -s "${link#*:}" "$root/${link%%:*}"
done
ln -s bash "$root/usr/bin/sh"

# Copies a file from the host to the same name in the guest (the directory resolved: lib64 is usr/lib64).
inst() {
	local dir rel
	dir="$(realpath "$(dirname "$1")")"
	rel="${dir#/}/$(basename "$1")"
	mkdir -p "$root/${dir#/}"
	cp -L "$1" "$root/$rel"
	chmod 0755 "$root/$rel"
}
# Copies the libraries ldd lists for an ELF file (the loader's own too).
libs_of() {
	local lib
	while read -r lib; do
		inst "$lib"
	done < <(ldd "$1" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
}
systemd_dir="$(dirname "$(realpath /usr/lib/systemd/systemd)")"
for bin in /usr/lib/systemd/systemd "$systemd_dir/systemd-executor" "$systemd_dir/systemd-shutdown" \
	/usr/bin/systemctl /usr/bin/setpriv /usr/bin/bash /usr/bin/timeout /usr/bin/sleep /usr/bin/true \
	/usr/bin/head /usr/bin/tr /usr/bin/grep /usr/bin/sed /usr/bin/awk /usr/bin/mkdir /usr/bin/rm /usr/bin/rmdir \
	/usr/bin/cat /usr/bin/mount /usr/bin/chroot /usr/bin/cp /usr/bin/chown /usr/bin/journalctl /usr/bin/busctl \
	/usr/bin/readlink /usr/bin/cut /usr/bin/seq /usr/bin/stat /usr/lib/systemd/systemd-journald /usr/bin/dbus-daemon; do
	inst "$bin"
	libs_of "$bin"
done
for bin in "$LEMNOSD_BIN" "$check" "$helper" "$probe"; do
	libs_of "$bin"
done
cp "$LEMNOSD_BIN" "$root/usr/bin/lemnosd"
cp "$helper" "$root/usr/bin/lemnos-restart"
cp "$probe" "$root/usr/bin/lemnos-restart-probe"
cp "$check" "$root/usr/bin/lemnos-restart-lemnosd-check"
chmod 0755 "$root/usr/bin/lemnosd" "$root/usr/bin/lemnos-restart" "$root/usr/bin/lemnos-restart-probe" "$root/usr/bin/lemnos-restart-lemnosd-check"
inst /usr/lib64/libnss_files.so.2
[ -e "$root/usr/bin/sh" ] || die "bash did not come into the guest"

# The packaged units, as shipped: the daemon, the helper's socket and service. Then the test's drop-ins and units.
for unit in "$repo/packaging/systemd/lemnosd.service" "$repo/packaging/systemd/lemnos-restart.socket" "$repo/packaging/systemd/lemnos-restart.service"; do
	cp "$unit" "$root/usr/lib/systemd/system/"
done
cp "$src/dropins/20-packaged-slots.conf" "$src/dropins/10-restart.conf" "$root/etc/systemd/system/lemnosd.service.d/"
for unit in basic sysinit multi-user sockets paths timers shutdown reboot poweroff halt kexec final umount \
	local-fs local-fs-pre swap; do
	cp "$(realpath /usr/lib/systemd/system/$unit.target)" "$root/usr/lib/systemd/system/$unit.target"
done
for unit in systemd-reboot.service systemd-journald.service systemd-journald.socket systemd-journald-dev-log.socket \
	systemd-sysusers.service; do
	cp "$(realpath "/usr/lib/systemd/system/$unit")" "$root/usr/lib/systemd/system/$unit"
done
cp "$src"/units/* "$root/usr/lib/systemd/system/"
mkdir -p "$root/usr/lib/systemd/system/multi-user.target.wants" "$root/usr/lib/systemd/system/sockets.target.wants"
ln -s ../lemnosd.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnosd.service"
ln -s ../lemnos-restart.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnos-restart.service"
ln -s ../lemnos-restart-cases.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnos-restart-cases.service"
ln -s ../lemnos-guest-slow.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnos-guest-slow.service"
ln -s ../lemnos-guest-blocker.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnos-guest-blocker.service"
ln -s ../dbus.service "$root/usr/lib/systemd/system/multi-user.target.wants/dbus.service"
ln -s ../lemnos-restart.socket "$root/usr/lib/systemd/system/sockets.target.wants/lemnos-restart.socket"
ln -s ../dbus.socket "$root/usr/lib/systemd/system/sockets.target.wants/dbus.socket"
for unit in systemd-journald.socket systemd-journald-dev-log.socket; do
	ln -s "../$unit" "$root/usr/lib/systemd/system/sockets.target.wants/$unit"
done
ln -s multi-user.target "$root/usr/lib/systemd/system/default.target"
cp "$src/guest/cases.sh" "$root/usr/lib/lemnos-guest/cases.sh"
cp "$src/guest/slow-stop.sh" "$root/usr/lib/lemnos-guest/slow-stop.sh"
cp "$src/guest/blocker-stop.sh" "$root/usr/lib/lemnos-guest/blocker-stop.sh"
chmod 0755 "$root/usr/lib/lemnos-guest/cases.sh" "$root/usr/lib/lemnos-guest/slow-stop.sh" "$root/usr/lib/lemnos-guest/blocker-stop.sh"
cp "$src/guest/init" "$root/init"
chmod 0755 "$root/init"
cp "$src/etc/passwd" "$src/etc/group" "$src/etc/nsswitch.conf" "$src/etc/machine-id" "$src/etc/os-release" "$root/etc/"
cp "$src/etc/dbus-system.conf" "$root/etc/dbus-1/system.conf"
cp board.toml "$root/etc/lemnos/board.toml"
cp "$WORK/bundle.tar" "$root/var/lib/lemnos-restart/bundle.tar"

# The boot chain stood in (an x86 guest has no device tree): the disk's controller under the guest's
# /usr/lib/lemnos-guest/boot-roots, as run-slot-install.sh has it.
seam="$root/usr/lib/lemnos-guest/boot-roots"
sys="$seam/sys"
mkdir -p "$sys/firmware/devicetree/base/soc/mmc@7e202000" "$sys/devices/sd-host/vda/vda1" "$sys/devices/sd-host/vda/vda2" "$sys/class/block"
ln -s ../../firmware/devicetree/base/soc/mmc@7e202000 "$sys/devices/sd-host/of_node"
ln -s ../../devices/sd-host/vda "$sys/class/block/vda"
ln -s ../../devices/sd-host/vda/vda1 "$sys/class/block/vda1"
ln -s ../../devices/sd-host/vda/vda2 "$sys/class/block/vda2"
echo 1 > "$sys/devices/sd-host/vda/vda1/partition"
echo 2 > "$sys/devices/sd-host/vda/vda2/partition"
mkdir -p "$seam/chosen"
printf '/soc/mmc@7e202000\0' > "$seam/chosen/lemnos,boot-source"
(cd "$root" && find . -print0 | cpio --null -o -H newc --quiet) > "$WORK/initramfs.cpio"
empty "$root"
rmdir "$root"

# boot STAGE: one QEMU boot of the disk, under the kernel lock for this boot only; its console goes into out/.
QSTATUS_LAST=0
boot() {
	local stage="$1" stamped="$WORK/out/console-$1-stamped.log" console="$WORK/out/console-$1.log"
	local cmdline="console=ttyS0 loglevel=4 panic=5 systemd.log_target=console systemd.log_level=info rst.stage=$stage rst.cases=$CASES"
	local stamp
	stamp() {
		local line
		while IFS= read -r line || [ -n "$line" ]; do
			printf '%s %s\n' "$EPOCHREALTIME" "${line%$'\r'}"
		done
		printf '%s EXIT\n' "$EPOCHREALTIME"
	}
	echo "== boot $stage of the disk: $KERNEL under systemd ($ACCEL), bound ${BOUND}s, the kernel read under a shared lock for this boot"
	set +e
	pooled 2 flock -s "$KERNEL_LOCK" timeout --kill-after=30 "$BOUND" qemu-system-x86_64 -accel "$ACCEL" -cpu "$CPU" \
		-smp 2 -m "$MEM" -nodefaults -display none -monitor none -serial stdio -no-reboot \
		-kernel "$KERNEL" -initrd "$WORK/initramfs.cpio" -append "$cmdline" \
		-drive "file=$WORK/disk.img,format=raw,if=virtio,cache=writeback" </dev/null 2>&1 | stamp > "$stamped"
	QSTATUS_LAST=${PIPESTATUS[0]}
	set -e
	sed -E 's/^[0-9]+\.[0-9]+ //' "$stamped" > "$console"
	echo "QEMU status $QSTATUS_LAST; console in $console"
}

# CASES: the stage-1 cases to run, letters of c, b, a (default "ca": a's reboot is the end of the stage, b's is a separate run).
CASES="${CASES:-ca}"
verdict=0
bad() {
	echo "FAIL: $*"
	verdict=1
}

boot 1
q1=$QSTATUS_LAST
if [ "$q1" -eq 124 ] || [ "$q1" -eq 137 ]; then
	bad "boot 1 ran past BOUND=${BOUND}s (status $q1)"
fi
c1="$WORK/out/console-1.log"
echo "== boot 1: the guest's cases"
grep -a '^LEMNOSD-RESTART-GUEST ' "$c1" | grep -a -E 'PASS|FAIL|INSTALL|CASES' || true
for n in d $(printf '%s' "$CASES" | sed 's/./& /g'); do
	if grep -a -q "^LEMNOSD-RESTART-GUEST PASS $n " "$c1"; then :; else bad "case $n has no PASS line from the guest"; fi
done
if grep -a -q '^LEMNOSD-RESTART-GUEST FAIL' "$c1"; then bad "the guest reports a FAIL (above)"; fi
grep -a -q '^LEMNOSD-RESTART-GUEST CASES stage 1 socket up helper up' "$c1" || bad "the daemon's socket or the helper's was not up for the cases"
case "$CASES" in *a*) grep -a -q 'SLOW-STOP-END' "$c1" || bad "a: the slow unit's stop did not end on the guest's console (the reboot did not go through the stop)" ;; esac
if [ "$q1" -ne 0 ]; then bad "boot 1: QEMU did not exit 0 through the reboot (status $q1)"; fi
if grep -a -q 'Kernel panic' "$c1"; then bad "boot 1: the guest panicked"; fi

echo "== the boot partition after the reboot (case a's trial is armed, so it verifies and names the trial)"
dd if="$WORK/disk.img" of="$WORK/boot-after.img" bs=512 skip=$p1 count=$boot_sectors status=none
"$ctl" boot-partition --socs "$socs" --verify "$WORK/boot-after.img" --layout-of "$WORK/board.toml" > "$WORK/out/boot-verify-after.txt" 2>&1 \
	&& echo "boot partition after boot 1 verifies" || bad "the boot partition after boot 1 does not verify"

if [ "${CASES#*a}" != "$CASES" ]; then
boot 2
q2=$QSTATUS_LAST
c2="$WORK/out/console-2.log"
echo "== boot 2: the state read back"
grep -a '^LEMNOSD-RESTART-GUEST ' "$c2" | grep -a -E 'PASS|FAIL|STAGE2|CASES' || true
grep -a -q '^LEMNOSD-RESTART-GUEST PASS a2 ' "$c2" || bad "a2: the next boot's state does not have the trial armed"
if [ "$q2" -ne 0 ]; then bad "boot 2: QEMU did not exit 0 (status $q2)"; fi
fi

if [ "$verdict" -eq 0 ]; then
	echo "PASS: every case passed, and the boot partition verifies (logs: $WORK/out)"
else
	echo "FAIL: see the lines above (logs: $WORK/out)"
fi
exit "$verdict"
