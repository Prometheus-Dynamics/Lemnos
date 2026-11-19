#!/usr/bin/env bash
# Builds the stage image, links it at the load address, and checks the link against the vendor
# tool's load range. Prints the sizes. Builds for the riscv target only; run from anywhere.
#
# The checks: the entry is at the load address; the image's footprint (code, data, bss and stack, the
# whole memory it occupies from the load address) ends inside the range 0x80300000..=0x80400000, so the
# footprint is at most 0xA0000 bytes; the image has no relocations (it is linked where it runs).
#
# With --b2 it builds the B2 check build instead (feature `b2`: the FL1 stage's USB device path over a RAM
# window, testing/board-runs/K230-USB.md "B2") into its own directory, and checks two more things: the image
# reaches no SD host (no symbol of the SD code, and no instruction that forms an SD controller's base
# address), and it carries the check build's identity. The piece job's address does not apply to it.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target=riscv64imac-unknown-none-elf
out="${CARGO_TARGET_DIR:-$repo/target}"
b2=0
[ "${1:-}" = "--b2" ] && b2=1
cd "$repo"
if [ "$b2" = 1 ]; then
  # Its own target subdirectory, so the two builds' libraries never overwrite each other.
  out="$out/b2"
  CARGO_TARGET_DIR="$out" cargo build -p lemnos-k230-usbboot-image --target "$target" --release --features b2
else
  cargo build -p lemnos-k230-usbboot-image --target "$target" --release
fi
lib="$out/$target/release/liblemnos_k230_usbboot_image.a"
sysroot="$(rustc --print sysroot)"
lld="$(ls "$sysroot"/lib/rustlib/*/bin/rust-lld | head -1)"
work="${TMPDIR:-/tmp}/k230-usbboot-image"
[ "$b2" = 1 ] && work="$work-b2"
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

# One constant: the load address the host uses (testing/crates/k230-usbboot-stage/src/image.rs) must equal
# the linker script's origin and the entry. A mismatch is refused here and by the stage crate's tests.
host_load=$(grep -oE 'pub const LOAD_ADDRESS: u32 = 0x[0-9_a-fA-F]+' "$repo/testing/crates/k230-usbboot-stage/src/image.rs" | grep -oE '0x[0-9_a-fA-F]+' | tr -d _)
ld_origin=$(grep -oE '\. = 0x[0-9a-fA-F]+;' "$repo/drivers/k230-usbboot-image/image.ld" | head -1 | grep -oE '0x[0-9a-fA-F]+')
[ $((host_load)) -eq $((ld_origin)) ] || { echo "FAIL: the host's load address $host_load is not the linker origin $ld_origin"; exit 1; }
[ $((host_load)) -eq $((0x80360000)) ] || { echo "FAIL: the load address is not 0x80360000"; exit 1; }

# The entry check: the code's instruction classes and branch targets (check-entry.py, rules 1 to 5).
llvm-objdump -d --no-show-raw-insn "$work/image.elf" > "$work/image.dis"
python3 -I "$repo/drivers/k230-usbboot-image/check-entry.py" "$work/image.dis" || { echo "FAIL: the entry check refused the image"; exit 1; }

fail=0
[ $((entry)) -eq $((0x80360000)) ] || { echo "FAIL: the entry is not at the load address"; fail=1; }
[ $((load_start)) -eq $((0x80360000)) ] || { echo "FAIL: the load segment is not at the load address"; fail=1; }
[ $((end)) -le $((0x80400000)) ] || { echo "FAIL: the image runs past 0x80400000"; fail=1; }
[ "$footprint" -le $((0xA0000)) ] || { echo "FAIL: the footprint is over 0xA0000"; fail=1; }
if [ "$b2" = 0 ]; then
  [ $((end)) -le $((0x80380000)) ] || { echo "FAIL: the image runs into the piece job's address 0x80380000"; fail=1; }
else
  # No SD host in the check build: no symbol of the SD code, and no lui that forms 0x91580000 or 0x91581000
  # (the two SDHCI bases, k230.dtsi:154-176) or 0x91585000 and up.
  sd_symbols=$(grep -cE '<[^>]*(sdhci|SdhciHost|sd5Card|read_card|run_piece|run_report|stage_medium)[^>]*>:' "$work/image.dis" || true)
  sd_bases=$(grep -ciE 'lui[[:space:]]+[a-z0-9]+, (0x9158[01]|594304|594305)$' "$work/image.dis" || true)
  echo "b2: SD symbols $sd_symbols, SD base loads $sd_bases"
  [ "$sd_symbols" = 0 ] || { echo "FAIL: the check build carries SD host code"; fail=1; }
  [ "$sd_bases" = 0 ] || { echo "FAIL: the check build forms an SD controller's address"; fail=1; }
  grep -q '<lemnos_image_run>:' "$work/image.dis" || { echo "FAIL: no run entry in the check build"; fail=1; }
fi
[ "$relocs" = "0" ] || { echo "FAIL: the image has relocations"; fail=1; }
sha256sum "$work/image.bin"
[ "$fail" = "0" ] && echo "image link checks hold" || exit 1
