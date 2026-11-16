#!/usr/bin/env bash
# Builds the stage image, links it at the load address, and checks the link against the vendor
# tool's load range. Prints the sizes. Builds for the riscv target only; run from anywhere.
#
# The checks: the entry is at the load address; the image's footprint (code, data, bss and stack, the
# whole memory it occupies from the load address) ends inside the range 0x80300000..=0x80400000, so the
# footprint is at most 0xA0000 bytes; the image has no relocations (it is linked where it runs).
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target=riscv64imac-unknown-none-elf
out="${CARGO_TARGET_DIR:-$repo/target}"
cd "$repo"
cargo build -p lemnos-k230-usbboot-image --target "$target" --release
lib="$out/$target/release/liblemnos_k230_usbboot_image.a"
sysroot="$(rustc --print sysroot)"
lld="$(ls "$sysroot"/lib/rustlib/*/bin/rust-lld | head -1)"
work="${TMPDIR:-/tmp}/k230-usbboot-image"
mkdir -p "$work"
"$lld" -flavor gnu -T "$repo/drivers/k230-usbboot-image/image.ld" --gc-sections -o "$work/image.elf" "$lib"
rust-objcopy -O binary "$work/image.elf" "$work/image.bin"

entry=$(readelf -hW "$work/image.elf" | awk '/Entry point/ {print $NF}')
# The LOAD segments: VirtAddr (column 3) and MemSiz (column 6). The image's end is the largest
# VirtAddr + MemSiz; its footprint is that end less the load address.
read -r load_start end < <(readelf -lW "$work/image.elf" | awk '$1 == "LOAD" {print $3, $6}' | python3 -c '
import sys
rows = [l.split() for l in sys.stdin if l.strip()]
starts = [int(r[0], 16) for r in rows]
ends = [int(r[0], 16) + int(r[1], 16) for r in rows]
print(hex(min(starts)), hex(max(ends)))
')
bin_size=$(stat -c %s "$work/image.bin")
relocs=$(readelf -rW "$work/image.elf" | grep -c "R_RISCV" || true)
footprint=$((end - 0x80360000))

echo "entry $entry, load segment starts at $load_start, image ends at $end"
echo "file size $bin_size bytes, footprint $(printf '%#x' "$footprint") bytes from the load address"
echo "relocations $relocs"

fail=0
[ $((entry)) -eq $((0x80360000)) ] || { echo "FAIL: the entry is not at the load address"; fail=1; }
[ $((load_start)) -eq $((0x80360000)) ] || { echo "FAIL: the load segment is not at the load address"; fail=1; }
[ $((end)) -le $((0x80400000)) ] || { echo "FAIL: the image runs past 0x80400000"; fail=1; }
[ "$footprint" -le $((0xA0000)) ] || { echo "FAIL: the footprint is over 0xA0000"; fail=1; }
[ "$relocs" = "0" ] || { echo "FAIL: the image has relocations"; fail=1; }
[ "$fail" = "0" ] && echo "image link checks hold" || exit 1
