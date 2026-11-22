#!/usr/bin/env bash
# The first flash on an eMMC, in an arm64 QEMU guest (testing/crates/flasher-check): the real lemnosd and the
# bridge's stream write an xz disk image to the one whole disk under the eMMC's controller, then verify it. The
# eMMC is QEMU's `emmc` on `sdhci-pci` with two 1 MiB hardware boot areas, as the board's eMMC has; it starts
# filled with 0xa5, so a byte the flash should not touch is seen. The guest checks the image, the boot areas and
# the new partition; the harness then checks the eMMC's backing file: the boot areas (its first 2 MiB) are as they
# were, the user area starts with the image, and every byte after the image is as it was.
#
#   KERNEL  an arm64 Image built by build-kernel.sh (required)
#   CARGO   the cargo that builds the guest for aarch64-unknown-linux-musl (default: cargo)
#   The guest is two vCPUs under TCG that mostly wait for the kernel: one slot of the pool (lt/slot-run.sh 1).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
: "${KERNEL:?set KERNEL to an arm64 Image built by testing/flasher/build-kernel.sh}"
CARGO="${CARGO:-cargo}"
target=aarch64-unknown-linux-musl
targetdir="${CARGO_TARGET_DIR:-$repo/target}"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

(cd "$repo" && "$CARGO" build -q -p lemnosd -p lemnos-flasher-check --bin lemnosd --bin lemnos-flasher-guest --target "$target")

# The image: 8 MiB, an MBR with one partition at sector 2048 of 12288 sectors, and bytes that are not zero, so a
# short or misplaced write is seen. Then the build's own xz form (docs/slots.md, "The image").
python3 - "$work/image.img" <<'PY'
import hashlib, struct, sys
size = 8 << 20
start, sectors = 2048, 12288
out = bytearray()
seed = b"lemnos-first-flash-guest"
while len(out) < size:
    seed = hashlib.sha256(seed).digest()
    out += seed
out = out[:size]
entry = struct.pack("<B3sB3sII", 0, b"\0\0\0", 0x83, b"\0\0\0", start, sectors)
out[0:446] = bytes(446)
out[446:462] = entry
out[462:510] = bytes(48)
out[510:512] = b"\x55\xaa"
open(sys.argv[1], "wb").write(out)
PY
xz -6 --check=crc64 --block-size=1MiB -T2 -k "$work/image.img"
raw=$(stat -c %s "$work/image.img")
sha=$(sha256sum "$work/image.img" | cut -d' ' -f1)
printf '{"schema":1,"board":"flasher-guest","format":"disk","image":{"size":%s,"sha256":"%s"}}' "$raw" "$sha" >"$work/image.img.xz.json"
echo "$raw $sha 12288" >"$work/image.facts"

# The eMMC: 64 MiB of user area behind two 1 MiB boot areas, all 0xa5. The copy is what the run is checked against.
boot=$((1 << 20)); user=$((64 << 20))
python3 -c "import sys; open(sys.argv[1],'wb').write(b'\xa5' * int(sys.argv[2]))" "$work/emmc.img" $((2 * boot + user))
cp "$work/emmc.img" "$work/emmc.orig"

root="$work/root"
mkdir -p "$root"/{dev,proc,sys,run}
cp "$targetdir/$target/debug/lemnos-flasher-guest" "$root/init"
cp "$targetdir/$target/debug/lemnosd" "$root/lemnosd"
cp "$work/image.img.xz" "$work/image.img.xz.json" "$work/image.facts" "$root/"
(cd "$root" && find . -print0 | cpio --quiet --null -o -H newc) >"$work/initramfs"

status=0
timeout 600 qemu-system-aarch64 -machine virt -cpu cortex-a72 -smp 2 -m 512 -nographic -no-reboot \
  -kernel "$KERNEL" -initrd "$work/initramfs" \
  -device sdhci-pci -drive "if=none,id=emmc,file=$work/emmc.img,format=raw" \
  -device emmc,drive=emmc,boot-partition-size=$boot \
  -append "console=ttyAMA0 panic=-1 loglevel=4" </dev/null | tee "$work/console.log" || status=$?
echo "qemu: exited with status $status" >&2
if [[ $status -ne 0 ]]; then
  echo "qemu: the guest did not power off cleanly" >&2
  exit 1
fi
if ! grep -q '^flasher: passed' "$work/console.log"; then
  echo "qemu: the guest's checks did not pass" >&2
  exit 1
fi

# The backing file: the boot areas unchanged, the user area starting with the image, the rest unchanged.
python3 - "$work/emmc.img" "$work/emmc.orig" "$work/image.img" "$boot" <<'PY'
import sys
now, was, image = (open(p, "rb").read() for p in sys.argv[1:4])
boot = int(sys.argv[4])
areas = 2 * boot
assert now[:areas] == was[:areas], "the boot areas changed"
assert now[areas:areas + len(image)] == image, "the user area does not start with the image"
assert now[areas + len(image):] == was[areas + len(image):], "bytes after the image changed"
print(f"emmc: boot areas unchanged ({areas} bytes); user area holds the image ({len(image)} bytes); the rest unchanged")
PY
echo "flasher: passed"
