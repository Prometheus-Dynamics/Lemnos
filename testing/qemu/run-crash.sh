#!/usr/bin/env bash
# Proves the crash log (BB3, docs/slots.md "The crash log") in QEMU, with the driver's own record step.
#
#   KERNEL  the bootable image of a kernel built with CONFIG_PSTORE, _PSTORE_RAM and _PSTORE_CONSOLE =y
#           (the modules-off harness kernel: testing/qemu/build-kernel.sh --modules-off)
#   CARGO   the cargo that builds the probe for musl (default: cargo; the group's e-cargo.sh)
#   CARGO   the cargo that builds the probe and the guest's init for musl (default: cargo; the group's e-cargo.sh)
#   LOGS    where the console logs and the state images' reads are kept (default: a crash-log directory
#           under the gates' tmp directory)
#
# Cases, each a QEMU guest with a fresh state disk (ext4, the boot history and the index live there):
#   crash      the first boot arms, records its own history entry and panics with sysrq-c; panic=1 reboots the
#              guest (QEMU keeps the RAM, so the ramoops region survives); the next boot stores the record in
#              the crash index for its own boot number, cuts the text to the bound, clears pstore and powers off.
#   nomount    one probe, pstore not mounted: the probe says so in words and the boot goes on.
#   undeclared one probe with no crash log declared: no crash fields, no crash files.
#   empty      one probe, pstore mounted and empty: "no record from the last boot".
#
# Checks read the state disk with debugfs, not only the console: the stored text, the index and the history.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
KERNEL="${KERNEL:?set KERNEL to the bzImage of a kernel with CONFIG_PSTORE, CONFIG_PSTORE_RAM and CONFIG_PSTORE_CONSOLE =y}"
CARGO="${CARGO:-cargo}"
CARGO="${CARGO:-cargo}"
LOGS="${LOGS:-/mnt/nvme-wd/lemnos-gates/tmp/qemu-crash}"
target=x86_64-unknown-linux-musl
# The ramoops region: 1 MiB at 496 MiB, reserved from the guest's memory (-m 512) by memmap=. A guest only.
REGION=0x1f000000

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
accel=tcg
if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi

fail() { echo "qemu-crash: $*" >&2; exit 1; }

(cd "$repo" && "$CARGO" build -q -p lemnos-driver-system-slots --example crash_probe --target "$target")
probe="${CARGO_TARGET_DIR:-$repo/target}/$target/debug/examples/crash_probe"
[[ -x "$probe" ]] || fail "the probe was not built at $probe"
(cd "$repo" && "$CARGO" build -q -p lemnos-crash-init --target "$target")
init="${CARGO_TARGET_DIR:-$repo/target}/$target/debug/lemnos-crash-init"
[[ -x "$init" ]] || fail "the guest's init was not built at $init"

mkdir -p "$work/root"/{dev,proc,sys,sbin,state,sys/fs/pstore}
cp "$init" "$work/root/init"
cp "$probe" "$work/root/sbin/crash-probe"
(cd "$work/root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs"

mkdir -p "$LOGS"
# Runs one guest on a fresh state disk. $1 case, $2 the extra command-line words.
run_case() {
  local name="$1" extra="$2" img="$work/state-$1.img" log="$LOGS/$1.log"
  truncate -s 64M "$img"
  mkfs.ext4 -q -F "$img"
  local append="console=ttyS0 panic=1 loglevel=6 printk.time=1 memmap=1M\$$REGION ramoops.mem_address=$REGION ramoops.mem_size=0x100000 ramoops.console_size=0x40000 ramoops.record_size=0x20000 ramoops.ecc=0 $extra"
  # No -no-reboot: the crash's panic reboots the guest, and that reset keeps the RAM.
  timeout 300 qemu-system-x86_64 -accel "$accel" -cpu max -smp 2 -m 512 -nographic \
    -kernel "$KERNEL" -initrd "$work/initramfs" \
    -drive "file=$img,if=none,id=state,format=raw" -device "nvme,drive=state,serial=lemnosstate" \
    -append "$append" >"$log" 2>&1 || true
  cp "$img" "$LOGS/$name.state.img"
  echo "$log"
}

# The state disk's files, read from the image: `cat` of a path inside it, or nothing when absent.
read_state() { debugfs -R "cat $2" "$1" 2>/dev/null || true; }

# --- crash ---------------------------------------------------------------------------------------
log="$(run_case crash "")"
grep -q 'arming the crash: the first boot' "$log" || fail "the first boot did not arm the crash"
grep -q 'sysrq: Trigger a crash' "$log" || fail "the crash was not triggered"
grep -q 'the boot after the crash' "$log" || fail "the guest did not come back after the panic"
grep -q 'Kernel panic' "$log" || fail "the panic is not in the console"
grep -q 'crash_probe: boot .*: crash log: stored dmesg-ramoops-0' "$log" || fail "the boot after the crash did not store the record"
grep -q 'crash_probe: history appended: true' "$log" || fail "the boot's history entry was not written"
grep -q 'crash_probe: pstore records now: 0' "$log" || fail "pstore was not cleared"
grep -q 'crash-init: probe exited 0' "$log" || fail "the probe did not exit 0 on the crash run"
img="$LOGS/crash.state.img"
boots="$(read_state "$img" /data/board/slots/boots)"
[[ "$(printf '%s\n' "$boots" | grep -c .)" == 2 ]] || fail "the boot history does not hold two boots: $boots"
index="$(read_state "$img" /data/board/slots/crashes)"
[[ "$(printf '%s\n' "$index" | grep -c .)" == 1 ]] || fail "the crash index does not hold one row: $index"
n="$(printf '%s\n' "$index" | awk '{print $1}')"
[[ "$n" == 2 ]] || fail "the crash is linked to boot $n, not to boot 2 (the boot after the crash)"
boot_id="$(printf '%s\n' "$index" | awk '{print $2}')"
[[ -n "$boot_id" && "$boot_id" != - ]] || fail "the index row has no boot id: $index"
printf '%s\n' "$boots" | awk -v n="$n" '$1 == n' | grep -q . || fail "boot $n has no history entry"
text="$(read_state "$img" "/data/board/slots/crash/$n")"
# The tail is at most 8192 bytes; the cut line, when there is one, is the first line and not counted in it.
first="$(printf '%s\n' "$text" | head -1)"
if [[ "$first" =~ ^cut:\ [0-9]+\ bytes\ before\ this\ not\ kept$ ]]; then
  body="$(printf '%s' "$text" | tail -n +2)"
else
  body="$text"
fi
body_len="$(printf '%s' "$body" | wc -c)"
[[ "$body_len" -le 8192 ]] || fail "the stored tail is $body_len bytes, over 8192"
printf '%s' "$body" | grep -q 'sysrq' || fail "the stored text does not carry the crash (no sysrq line)"
echo "qemu-crash: crash: boot $n stores the record, pstore cleared, cut line: $first, tail $body_len bytes"

# --- nomount -------------------------------------------------------------------------------------
log="$(run_case nomount "lemnos.plain lemnos.nomount")"
grep -q 'crash log: /sys/fs/pstore is not mounted' "$log" || fail "a missing mount is not said in words"
grep -q 'crash-init: probe exited 0' "$log" || fail "the boot did not go on past a missing mount"
img="$LOGS/nomount.state.img"
[[ -z "$(read_state "$img" /data/board/slots/crashes)" ]] || fail "a missing mount wrote a crash index row"
echo "qemu-crash: nomount: said in words, the boot went on"

# --- undeclared ----------------------------------------------------------------------------------
log="$(run_case undeclared "lemnos.plain lemnos.undeclared")"
grep -q 'crash_probe: boot .*: no crash log declared' "$log" || fail "an undeclared board did not say so"
grep -q 'crash-init: probe exited 0' "$log" || fail "the undeclared boot did not go on"
img="$LOGS/undeclared.state.img"
[[ -z "$(read_state "$img" /data/board/slots/crashes)" && -z "$(read_state "$img" /data/board/slots/crash/1)" ]] || fail "an undeclared board wrote crash files"
echo "qemu-crash: undeclared: no crash files, the boot as before"

# --- empty ---------------------------------------------------------------------------------------
log="$(run_case empty "lemnos.plain")"
grep -q 'crash log: no record from the last boot' "$log" || fail "an empty pstore is not recorded as no record"
echo "qemu-crash: empty: recorded as no record"

echo "qemu-crash: passed (crash nomount undeclared empty) on $accel"
