#!/usr/bin/env bash
# The slot install under the real packaged unit (SLOT-INSTALL-INT). A QEMU guest boots the lander's kernel with
# systemd as PID 1. The guest's lemnosd is started by its packaged unit (packaging/systemd/lemnosd.service, shipped
# as it is, with a test drop-in); its system-slots driver is configured on a bay boot partition (made by
# `lemnos-ctl boot-partition`, the same image builder a board uses) on the guest's disk, and its data side is an
# ext4 partition of the same disk. The guest's client installs a bundle (`lemnos-ctl bundle`) through the
# daemon's socket while two competitors are pinned to the daemon's CPU. The host reads the console and checks:
#
#   - the install's job ends committed (status 0), with no warning at warning priority in the guest journal;
#   - the boot partition the guest left verifies with `lemnos-ctl boot-partition --verify` (the bays and the
#     pointers the boot chain reads), and the slot is in a bay of it;
#   - the guest powers off cleanly (QEMU exits 0).
#
# Fail-before: the same run with the daemon built from the tree before the fix (LEMNOSD_BIN=<its lemnosd>,
# BUILD_DAEMON=0). That run is expected to show INTERRUPTED (-4) or a deadline warning; the script reports it
# and exits 1 there, as it does on any FAIL.
#
# The guest's pieces are in testing/qemu/slot-install/ (its init, the install run, the unit and the drop-ins).
# The kernel is the lander's built kernel, read in place under a shared flock on its tree's lock, for one boot.
#
# Run it as a whole (builds the checker, the image builder and the daemon, then boots once):
#   testing/qemu/run-slot-install.sh
# Environment (all optional):
#   SLOT_RUN       the slot pool's wrapper (`slot-run.sh N cmd...`); when set, every heavy step goes through it
#                  (6 for builds, 2 for the QEMU guest: its two vCPUs, both busy with the competitors)
#   CARGO_TARGET_DIR  where the binaries are built (default: the repository's target/)
#   LABEL          the run's name, for its work directory (default: after)
#   WORK           the work directory (default: $CARGO_TARGET_DIR/slot-install-$LABEL)
#   LEMNOSD_BIN    the daemon to boot (default: the one built here, with the testing-boot-roots feature)
#   BUILD_DAEMON   1 builds the daemon, 0 uses LEMNOSD_BIN as given (default 1)
#   DAEMON_PROFILE the cargo profile the daemon is built with (default release)
#   KERNEL         the bootable image (default: the lander's kernel, qk/modules-36008ba4fda2e9e0)
#   KERNEL_LOCK    the lock the kernel tree is read under (default: qk/modules-36008ba4fda2e9e0.lock)
#   BOUND          seconds for the whole QEMU run (default 900)
#   MEM            guest memory in MiB (default 1024)
#   COMPETE_S      seconds the competitors run (default 150)
#   ACCEL          kvm or tcg (default: kvm when /dev/kvm is writable, else tcg)
#
# Exits 0 when the guest's install passed every check and the boot partition verifies. Exits 1 otherwise.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
src="$here/slot-install"
CARGO="${CARGO:-cargo}"
LABEL="${LABEL:-after}"
target="${CARGO_TARGET_DIR:-$repo/target}"
WORK="${WORK:-$target/slot-install-$LABEL}"
BUILD_DAEMON="${BUILD_DAEMON:-1}"
# The daemon's build profile: release (optimised, as the daemon's own build is; the shipped `service` profile's fat LTO
# is not needed for the timing, and the image builder and the checker are built as dev).
DAEMON_PROFILE="${DAEMON_PROFILE:-release}"
KERNEL="${KERNEL:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0/build/arch/x86/boot/bzImage}"
KERNEL_LOCK="${KERNEL_LOCK:-/mnt/nvme-wd/lemnos-targets/qk/modules-36008ba4fda2e9e0.lock}"
BOUND="${BOUND:-900}"
MEM="${MEM:-1024}"
COMPETE_S="${COMPETE_S:-600}"
SLOT_RUN="${SLOT_RUN:-}"
if [ -z "${ACCEL:-}" ]; then
	if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=kvm; else ACCEL=tcg; fi
fi
if [ "$ACCEL" = kvm ]; then CPU=host; else CPU=max; fi

die() {
	echo "run-slot-install: $*" >&2
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

for tool in qemu-system-x86_64 cpio ldd realpath timeout mke2fs sfdisk dd flock truncate; do
	command -v "$tool" >/dev/null || die "$tool is needed"
done
[ -r "$KERNEL" ] || die "the kernel $KERNEL is not readable"
[ -e "$KERNEL_LOCK" ] || die "the kernel tree's lock $KERNEL_LOCK is not there"

if [ "$BUILD_DAEMON" = 1 ]; then
	echo "== build the daemon (testing-boot-roots), the image builder and the checker"
	pooled 6 env CARGO_TARGET_DIR="$target" TMPDIR="${TMPDIR:-/tmp}" "$CARGO" build --offline -j 6 \
		--manifest-path "$repo/Cargo.toml" --profile "$DAEMON_PROFILE" -p lemnosd --features testing-boot-roots
	LEMNOSD_BIN="${LEMNOSD_BIN:-$target/$DAEMON_PROFILE/lemnosd}"
	pooled 6 env CARGO_TARGET_DIR="$target" TMPDIR="${TMPDIR:-/tmp}" "$CARGO" build --offline -j 6 \
		--manifest-path "$repo/Cargo.toml" -p lemnos-ctl -p lemnos-slot-install-check
else
	echo "== the daemon is not built here (BUILD_DAEMON=0)"
	LEMNOSD_BIN="${LEMNOSD_BIN:?BUILD_DAEMON=0 needs LEMNOSD_BIN}"
	pooled 6 env CARGO_TARGET_DIR="$target" TMPDIR="${TMPDIR:-/tmp}" "$CARGO" build --offline -j 6 \
		--manifest-path "$repo/Cargo.toml" -p lemnos-ctl -p lemnos-slot-install-check
fi
[ -x "$LEMNOSD_BIN" ] || die "the daemon $LEMNOSD_BIN is not an executable"
ctl="$target/debug/lemnos-ctl"
socs="$repo/socs"
check="$target/debug/lemnos-slot-install-check"
[ -x "$ctl" ] && [ -x "$check" ] || die "lemnos-ctl and the checker were not built under $target"

rm -rf "$WORK"
mkdir -p "$WORK/out"
cd "$WORK"

echo "== the board file (the reference board's bays, a tryboot slot device with its boot chain stood in)"
cat > board.toml <<'EOF'
format = "lemnos.board"
schema_version = 1

[board]
id = "slot-install"
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

# The board's one trust root, which the host gives the slots driver: the test key both bundles are signed with.
[security]
record = "/var/lib/lemnos/security.rec"
keys = "/etc/lemnos/slot-keys"

[[devices]]
id = "slots"
driver = "system-slots"
config = { data = "/data", boot = "/boot/firmware", record = "tryboot", boot_sources = ["/soc/mmc@7e202000=/soc/mmc@7e202000"], partuuid = "5e11a0a0-0002", boot_args = "os", reserve_mb = 8, keep = 3, device_tree = "/usr/lib/lemnos-guest/no-device-tree", vcio = "/usr/lib/lemnos-guest/no-device-tree" }
EOF

echo "== the payloads, the rootfs stand-in and the bundle (lemnos-ctl bundle, the same writer a sender uses)"
"$check" payload "$WORK/payload-base" base-1
"$check" payload "$WORK/payload-install" install-1
"$check" rootfs "$WORK/rootfs.erofs" "$WORK/verity.txt" 512
# Both bundles are signed with the documented test key (the seed 0x07 repeated), which the board trusts, as a
# signed board's are: `data-partition` refuses an unsigned first slot on a board that requires signatures.
printf '%s' "$(printf '07%.0s' $(seq 32))" > "$WORK/test-key.seed"
chmod 0600 "$WORK/test-key.seed"
echo "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c test-key-07" > "$WORK/slot-keys"
"$ctl" bundle --socs "$socs" --layout-of "$WORK/board.toml" --slot base-1 --payload "$WORK/payload-base" --firmware "$WORK/firmware" \
	--rootfs "$WORK/rootfs.erofs" --verity-table "$WORK/verity.txt" --product slot-install-os --version 0.9 \
	--key-file "$WORK/test-key.seed" --payload-out "$WORK/payload-base-out" --out "$WORK/base.tar" > "$WORK/out/base-bundle.txt" 2>&1 \
	|| { cat "$WORK/out/base-bundle.txt"; die "lemnos-ctl bundle refused the base bundle"; }
"$ctl" bundle --socs "$socs" --layout-of "$WORK/board.toml" --slot install-1 --payload "$WORK/payload-install" --firmware "$WORK/firmware" \
	--rootfs "$WORK/rootfs.erofs" --verity-table "$WORK/verity.txt" --product slot-install-os --version 1.0 \
	--key-file "$WORK/test-key.seed" --out "$WORK/bundle.tar" > "$WORK/out/bundle.txt" 2>&1 || { cat "$WORK/out/bundle.txt"; die "lemnos-ctl bundle refused the bundle"; }
"$ctl" bundle --verify "$WORK/bundle.tar" --socs "$socs" --layout-of "$WORK/board.toml" > "$WORK/out/bundle-verify.txt" 2>&1 \
	|| { cat "$WORK/out/bundle-verify.txt"; die "the bundle does not verify"; }
echo "bundle: $(stat -c %s "$WORK/bundle.tar") bytes, verifies"

echo "== the boot partition: the image builder with the base slot in bay set 0, its payload as the base bundle finalized it (the install goes to a free bay)"
min_text="$("$ctl" boot-partition --socs "$socs" --layout-of "$WORK/board.toml" --payload "$WORK/payload-base-out" --firmware "$WORK/firmware" --min-size)" || die "boot-partition --min-size refused"
min="${min_text%%$'\n'*}"
bootbytes=$(( (min + 511) / 512 * 512 ))
"$ctl" boot-partition --socs "$socs" --layout-of "$WORK/board.toml" --slot base-1 --payload "$WORK/payload-base-out" --firmware "$WORK/firmware" \
	--size "$bootbytes" --out "$WORK/boot.img" > "$WORK/out/boot-partition.txt" 2>&1 \
	|| { cat "$WORK/out/boot-partition.txt"; die "lemnos-ctl boot-partition refused the image"; }
"$ctl" boot-partition --socs "$socs" --verify "$WORK/boot.img" --layout-of "$WORK/board.toml" > "$WORK/out/boot-verify-before.txt" 2>&1 \
	|| { cat "$WORK/out/boot-verify-before.txt"; die "the new boot partition does not verify"; }
echo "boot partition: $bootbytes bytes, verifies before the install"

echo "== the disk: MBR, partition 1 the boot partition (FAT), partition 2 the data partition (ext4, to the end)"
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

echo "== the data partition's tree: lemnos-ctl data-partition, the first-flash writer, with base-1 (the guest's init gives the daemon's dirs to its user by slot-init's rule)"
rm -rf datatree
"$ctl" data-partition --socs "$socs" --layout-of "$WORK/board.toml" --install "$WORK/base.tar" --boot-image "$WORK/boot.img" \
	--keys "$WORK/slot-keys" --time 2026-11-22T12:00:00Z --data-size $((data_sectors * 512)) --out datatree > "$WORK/out/data-partition.txt" 2>&1 \
	|| { cat "$WORK/out/data-partition.txt"; die "lemnos-ctl data-partition refused the tree"; }
cat "$WORK/out/data-partition.txt"
[ -d datatree/slots/bootfiles ] || die "the first-flash tree has no boot-file store (slots/bootfiles)"
truncate -s $((data_sectors * 512)) data.img
mke2fs -q -F -t ext4 -b 4096 -I 256 -m 0 -O meta_bg,^resize_inode -E root_owner=0:0 -L lemnos-data \
	-d datatree data.img > "$WORK/out/mke2fs.txt" 2>&1 || { cat "$WORK/out/mke2fs.txt"; die "mke2fs refused the data partition"; }
dd if=data.img of=disk.img bs=512 seek=$p2 conv=notrunc status=none

echo "== the guest root (initramfs): systemd, the packaged daemon unit and its drop-ins, the checker, the stand-ins"
root="$WORK/root"
rm -rf "$root"
mkdir -p "$root"/{usr/bin,usr/sbin,usr/lib64,usr/lib/systemd/system,usr/lib/lemnos-guest/boot-roots,etc/systemd/system/lemnosd.service.d,etc/lemnos,var/lib/lemnos,etc/dbus-1,dev,proc,sys,run,tmp,root,var/lib/slot-install,data,boot/firmware}
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
# Copies an ELF file and the libraries ldd lists for it (the loader's own too).
inst_elf() {
	inst "$1"
	local lib
	while read -r lib; do
		inst "$lib"
	done < <(ldd "$1" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
}
systemd_dir="$(dirname "$(realpath /usr/lib/systemd/systemd)")"
for bin in /usr/lib/systemd/systemd "$systemd_dir/systemd-executor" "$systemd_dir/systemd-shutdown" \
	/usr/bin/systemctl /usr/bin/setpriv /usr/bin/bash /usr/bin/timeout /usr/bin/sleep /usr/bin/true \
	/usr/bin/head /usr/bin/tr /usr/bin/grep /usr/bin/sed /usr/bin/awk /usr/bin/mkdir /usr/bin/cat /usr/bin/mount \
	/usr/bin/chroot /usr/bin/cp /usr/bin/chown /usr/bin/journalctl /usr/bin/readlink /usr/bin/taskset /usr/bin/cut \
	/usr/bin/seq /usr/bin/nproc /usr/bin/stat /usr/lib/systemd/systemd-journald; do
	inst_elf "$bin"
done
# The daemon and the checker are dynamic: their libraries come with them (the loader's own too).
libs_of() {
	local lib
	while read -r lib; do
		inst "$lib"
	done < <(ldd "$1" | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; break } }')
}
libs_of "$LEMNOSD_BIN"
libs_of "$check"
cp "$LEMNOSD_BIN" "$root/usr/bin/lemnosd"
cp "$check" "$root/usr/bin/lemnos-slot-install-check"
chmod 0755 "$root/usr/bin/lemnosd" "$root/usr/bin/lemnos-slot-install-check"
inst /usr/lib64/libnss_files.so.2
inst /usr/lib64/libnss_systemd.so.2 2>/dev/null || true
[ -e "$root/usr/bin/sh" ] || die "bash did not come into the guest"

# The packaged units, as shipped, enabled as the packaging enables them, and the test's own unit and drop-ins.
cp "$repo/packaging/systemd/lemnosd.service" "$root/usr/lib/systemd/system/"
cp "$src/dropins/20-packaged-slots.conf" "$root/etc/systemd/system/lemnosd.service.d/"
cp "$src/dropins/10-slot-install.conf" "$root/etc/systemd/system/lemnosd.service.d/"
cp "$src/units/lemnos-slot-install.service" "$root/usr/lib/systemd/system/"
for unit in basic sysinit multi-user sockets paths timers shutdown reboot poweroff halt kexec final umount \
	local-fs local-fs-pre swap; do
	cp "$(realpath /usr/lib/systemd/system/$unit.target)" "$root/usr/lib/systemd/system/$unit.target"
done
for unit in systemd-reboot.service systemd-journald.service systemd-journald.socket systemd-journald-dev-log.socket; do
	cp "$(realpath "/usr/lib/systemd/system/$unit")" "$root/usr/lib/systemd/system/$unit"
done
mkdir -p "$root/usr/lib/systemd/system/multi-user.target.wants" "$root/usr/lib/systemd/system/sockets.target.wants"
ln -s ../lemnosd.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnosd.service"
ln -s ../lemnos-slot-install.service "$root/usr/lib/systemd/system/multi-user.target.wants/lemnos-slot-install.service"
for unit in systemd-journald.socket systemd-journald-dev-log.socket; do
	ln -s "../$unit" "$root/usr/lib/systemd/system/sockets.target.wants/$unit"
done
ln -s multi-user.target "$root/usr/lib/systemd/system/default.target"
cp "$src/guest/slot-install.sh" "$root/usr/lib/lemnos-guest/slot-install.sh"
chmod 0755 "$root/usr/lib/lemnos-guest/slot-install.sh"
cp "$src/guest/init" "$root/init"
chmod 0755 "$root/init"
cp "$src/etc/passwd" "$src/etc/group" "$src/etc/nsswitch.conf" "$src/etc/machine-id" "$src/etc/os-release" "$root/etc/"
cp "$src/etc/dbus-system.conf" "$root/etc/dbus-1/system.conf"
cp board.toml "$root/etc/lemnos/board.toml"
cp "$WORK/slot-keys" "$root/etc/lemnos/slot-keys"
cp "$WORK/bundle.tar" "$root/var/lib/slot-install/bundle.tar"

# The boot chain stood in: an x86 guest has no device tree, so the controller of the boot disk is a sysfs and a
# device-tree tree under the guest's /usr/lib/lemnos-guest/boot-roots. The disk and its partitions are the guest's
# own /dev/vda, /dev/vda1, /dev/vda2 (the device nodes are real; only the names of their controller are stood in).
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
rm -rf "$root"

echo "== boot $KERNEL under systemd ($ACCEL), bound ${BOUND}s, the lander's kernel read under a shared lock"
cmdline="console=ttyS0 loglevel=4 panic=5 systemd.log_target=console systemd.log_level=info"
stamp() {
	local line
	while IFS= read -r line || [ -n "$line" ]; do
		printf '%s %s\n' "$EPOCHREALTIME" "${line%$'\r'}"
	done
	printf '%s EXIT\n' "$EPOCHREALTIME"
}
stamped="$WORK/out/console-stamped.log"
console="$WORK/out/console.log"
set +e
pooled 2 flock -s "$KERNEL_LOCK" timeout --kill-after=30 "$BOUND" qemu-system-x86_64 -accel "$ACCEL" -cpu "$CPU" \
	-smp 2 -m "$MEM" -nodefaults -display none -monitor none -serial stdio -no-reboot \
	-kernel "$KERNEL" -initrd "$WORK/initramfs.cpio" -append "$cmdline" \
	-drive "file=$WORK/disk.img,format=raw,if=virtio,cache=writeback" </dev/null 2>&1 | stamp > "$stamped"
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
	bad "QEMU ran past BOUND=${BOUND}s (status $qstatus)"
fi
if [ "$qstatus" -ne 0 ] && [ "$qstatus" -ne 124 ] && [ "$qstatus" -ne 137 ]; then
	bad "QEMU did not exit 0 at the guest's power-off (status $qstatus)"
fi

# The guest's own lines: the console prefixes each with the kernel's time and the unit's name, so they are cut out from
# the word SLOT-INSTALL-GUEST on.
guest="$WORK/out/guest.txt"
grep -o 'SLOT-INSTALL-GUEST .*' "$console" > "$guest" || true
echo "== the guest's lines"
cat "$guest"
if ! grep -q '^SLOT-INSTALL-GUEST DONE ' "$guest"; then
	bad "the guest's check did not finish (no DONE line)"
fi
grep -q '^SLOT-INSTALL-GUEST DONE pass$' "$guest" || bad "the guest's checks did not all pass (above)"
if grep -q '^SLOT-INSTALL-GUEST FAIL' "$guest"; then
	bad "the guest reports a FAIL (above)"
fi
if grep -q '^SLOT-INSTALL-GUEST JOURNAL-WARNING' "$guest"; then
	bad "the guest journal has warnings at warning priority (above)"
fi
if grep -q '^SLOT-INSTALL-GUEST COMPETITORS-STOPPED' "$guest"; then :; else
	bad "the competitors' stop line is missing"
fi

echo "== the daemon's ticks in the install's window (the start trace's SLOT-TICK lines)"
ticks_in_job=$(grep -c '^SLOT-INSTALL-GUEST TICK-IN-JOB' "$guest" || true)
echo "ticks in the job's window: $ticks_in_job of $(grep -o 'TICKS-ALL [0-9]*' "$guest" | awk '{print $2}' | tail -1) SLOT-TICK lines in the daemon's journal"
if [ "$ticks_in_job" -gt 0 ]; then
	grep '^SLOT-INSTALL-GUEST TICK-IN-JOB' "$guest" | awk '
		{ for (i = 1; i < NF; i++) if ($i == "us" && $(i + 1) + 0 > max) { max = $(i + 1) + 0; line = $0 } }
		END { printf "longest tick in the job: %d us; the journal line: %s\n", max, line }' | sed 's/SLOT-INSTALL-GUEST TICK-IN-JOB //'
fi

echo "== the boot partition the guest left"
dd if="$WORK/disk.img" of="$WORK/boot-after.img" bs=512 skip=$p1 count=$boot_sectors status=none
if "$ctl" boot-partition --socs "$socs" --verify "$WORK/boot-after.img" --layout-of "$WORK/board.toml" > "$WORK/out/boot-verify-after.txt" 2>&1; then
	echo "boot partition after the install: verifies (lemnos-ctl boot-partition --verify)"
else
	cat "$WORK/out/boot-verify-after.txt"
	bad "the boot partition the guest left does not verify"
fi

echo "== the host's checks"
if [ "$verdict" -eq 0 ]; then
	echo "PASS: the install committed under the starved daemon, and the boot partition verifies (log: $console)"
else
	echo "FAIL: see the lines above (log: $console)"
fi
exit "$verdict"
