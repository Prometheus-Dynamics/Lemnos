#!/usr/bin/env bash
# The U-Boot environment's write path through the daemon's root phase, in a QEMU guest
# (testing/crates/uboot-grant-check). The guest boots a GPT disk image (a regular file, a virtio disk) with
# the boot partition and the environment partition, and runs: the unit's user refused by name on the real
# nodes; the daemon's root phase, its drop and set_default through the handed-over partition; the environment
# read back as root. The harness then checks the disk: every byte the run changed lies in the environment's
# partition.
#
#   KERNEL  the bootable image: virtio-blk and devtmpfs built in (default: this machine's kernel, read only)
#   CARGO   the cargo that builds the guest for musl and the image builder (default: cargo)
#   The guest is one vCPU that mostly waits for the kernel: it takes one slot of the pool (lt/slot-run.sh 1).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
KERNEL="${KERNEL:-/boot/vmlinuz-$(uname -r)}"
CARGO="${CARGO:-cargo}"
target=x86_64-unknown-linux-musl
targetdir="${CARGO_TARGET_DIR:-$repo/target}"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

(cd "$repo" && "$CARGO" build -q -p lemnos-uboot-grant-check --bin lemnos-uboot-grant-guest --target "$target")
(cd "$repo" && "$CARGO" build -q -p lemnos-uboot-grant-check --bin lemnos-uboot-grant-image)
image_bin="$targetdir/debug/lemnos-uboot-grant-image"
guest="$targetdir/$target/debug/lemnos-uboot-grant-guest"

# The disk is written as a file; the guest sees it as /dev/vda. The copy taken here is what the run is checked against.
"$image_bin" "$work/disk.img" >"$work/layout"
cp "$work/disk.img" "$work/disk.orig"
read -r _ first last _ _ < <(cat "$work/layout")

root="$work/root"
mkdir -p "$root"/{dev,proc,sys}
cp "$guest" "$root/init"
(cd "$root" && find . -print0 | cpio --quiet --null -o -H newc | gzip -1) >"$work/initramfs"

accel=tcg
if [[ -r /dev/kvm && -w /dev/kvm ]]; then accel=kvm; fi

status=0
timeout 300 qemu-system-x86_64 -accel "$accel" -cpu max -smp 1 -m 512 -nographic -no-reboot \
  -kernel "$KERNEL" -initrd "$work/initramfs" \
  -append "console=ttyS0 panic=-1 loglevel=4" \
  -drive "file=$work/disk.img,format=raw,if=virtio" \
  | tee "$work/console.log" || status=$?
echo "qemu: exited with status $status" >&2

if [[ $status -ne 0 ]]; then
  echo "qemu: the guest did not power off cleanly" >&2
  exit 1
fi
if ! grep -q '^uboot-grant: passed' "$work/console.log"; then
  echo "qemu: the guest's checks did not pass" >&2
  grep '^uboot-grant\|^probe\|^root phase\|^daemon\|^reader' "$work/console.log" >&2 || true
  exit 1
fi

# Every byte of the disk the run changed lies in the environment's partition (512-byte blocks first..last, 1-based offsets).
lo=$((first * 512 + 1))
hi=$(((last + 1) * 512))
read -r changed outside < <(cmp -l "$work/disk.orig" "$work/disk.img" 2>/dev/null |
  awk -v lo="$lo" -v hi="$hi" '{ n++; if ($1 < lo || $1 > hi) out++ } END { print n + 0, out + 0 }')
if [[ $outside -ne 0 ]]; then
  echo "qemu: $outside byte(s) of the disk outside the environment partition changed" >&2
  exit 1
fi
grep '^uboot-grant\|^probe\|^root phase\|^daemon\|^reader' "$work/console.log"
echo "disk: $changed byte(s) changed, all in the environment partition (blocks $first..$last)"
