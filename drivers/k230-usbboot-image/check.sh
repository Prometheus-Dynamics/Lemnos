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
#
# With --b2-card it builds the B2 scratch build instead (feature `b2-card`: the same USB device path over the card,
# reachable only inside the scratch range, testing/board-runs/K230-USB.md "B2 card writes") into its own directory,
# and checks: no other storage path and no other run is linked (no symbol of the piece, the report, the pre-FL1
# stage and its media, the legacy USB driver, the probe or the check build's RAM window), the scratch storage and
# the SD host are linked, the image carries the scratch build's identity and not the check build's. The piece job's
# address does not apply to it either: no piece job is loaded beside it.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target=riscv64imac-unknown-none-elf
out="${CARGO_TARGET_DIR:-$repo/target}"
b2=0
card=0
[ "${1:-}" = "--b2" ] && b2=1
[ "${1:-}" = "--b2-card" ] && card=1
cd "$repo"
# One codegen unit: with the default sixteen, how LLVM splits the driver crate decides what is inlined where,
# so a change to code the card build never calls (the B2 build's endpoint 0 trace) moved the card image by
# 1.6 KiB and past 0x80380000, which it clears by a few hundred bytes. With one unit the card image is the
# same size with or without that code (64824 bytes both, measured), and smaller than with sixteen.
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
if [ "$b2" = 1 ]; then
  # Its own target subdirectory, so the two builds' libraries never overwrite each other.
  out="$out/b2"
  CARGO_TARGET_DIR="$out" cargo build -p lemnos-k230-usbboot-image --target "$target" --release --features b2
elif [ "$card" = 1 ]; then
  out="$out/b2-card"
  CARGO_TARGET_DIR="$out" cargo build -p lemnos-k230-usbboot-image --target "$target" --release --features b2-card
else
  cargo build -p lemnos-k230-usbboot-image --target "$target" --release
fi
lib="$out/$target/release/liblemnos_k230_usbboot_image.a"
sysroot="$(rustc --print sysroot)"
lld="$(ls "$sysroot"/lib/rustlib/*/bin/rust-lld | head -1)"
work="${TMPDIR:-/tmp}/k230-usbboot-image"
[ "$b2" = 1 ] && work="$work-b2"
[ "$card" = 1 ] && work="$work-b2-card"
mkdir -p "$work"
"$lld" -flavor gnu -T "$repo/drivers/k230-usbboot-image/image.ld" --gc-sections -o "$work/image.elf" "$lib"
rust-objcopy -O binary "$work/image.elf" "$work/image.bin"

entry=$(readelf -hW "$work/image.elf" | awk '/Entry point/ {print $NF}')
# The LOAD segments: VirtAddr (column 3) and MemSiz (column 6). The image's end is the largest
# VirtAddr + MemSiz; its footprint is that end less the load address.
segments=$(readelf -lW "$work/image.elf" | awk '$1 == "LOAD" {print $3, $6}' | python3 "$repo/drivers/k230-usbboot-image/segments.py")
read -r load_start end <<<"$segments"
bin_size=$(stat -c %s "$work/image.bin")
relocs=$(readelf -rW "$work/image.elf" | grep -c "R_RISCV" || true)
footprint=$((end - 0x80360000))

echo "entry $entry, load segment starts at $load_start, image ends at $end"
echo "file size $bin_size bytes, footprint $(printf '%#x' "$footprint") bytes from the load address"
echo "relocations $relocs"

# One constant: the load address the host uses (drivers/k230-usbboot-stage/src/image.rs) must equal
# the linker script's origin and the entry. A mismatch is refused here and by the stage crate's tests.
host_load=$(grep -oE 'pub const LOAD_ADDRESS: u32 = 0x[0-9_a-fA-F]+' "$repo/drivers/k230-usbboot-stage/src/image.rs" | grep -oE '0x[0-9_a-fA-F]+' | tr -d _)
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
if [ "$card" = 1 ]; then
  # Nothing but the scratch run: no piece, report, pre-FL1 stage or its media, legacy USB driver, probe, or check
  # build's RAM window. Symbols are matched as the linker names them (mangled).
  others=$(grep -oE '^[0-9a-f]+ <[^>]*(run_piece|piece_job|run_report|run_stage|run_probe|run_check|stage_medium|read_card|ImageMedium|SdMedium|5piece|6report|stage5Stage|5probe|9telemetry|11dwc2_legacy|8Loopback)[^>]*>:' "$work/image.dis" || true)
  scratch_syms=$(grep -cE '<[^>]*7scratch[^>]*>:' "$work/image.dis" || true)
  sd_host=$(grep -cE '<[^>]*SdhciHost[^>]*>:' "$work/image.dis" || true)
  echo "b2-card: other runs or storage paths $(printf '%s' "$others" | grep -c . || true), scratch symbols $scratch_syms, SD host symbols $sd_host"
  [ -z "$others" ] || { echo "FAIL: the scratch build links another run or storage path:"; echo "$others" | head -20; fail=1; }
  [ "$scratch_syms" -gt 0 ] || { echo "FAIL: no scratch run in the scratch build"; fail=1; }
  [ "$sd_host" -gt 0 ] || { echo "FAIL: no SD host in the scratch build"; fail=1; }
  grep -q '<lemnos_image_run>:' "$work/image.dis" || { echo "FAIL: no run entry in the scratch build"; fail=1; }
  # The feed: the watchdog kick (fl1::scratch::kick) exists once and is reached from exactly one instruction, a
  # call or tail call in the gate (VerifiedFeed's Watchdog::feed), which kicks only after a verified write round.
  # References with an offset are branches inside kick itself.
  kick_defs=$(grep -cE '^[0-9a-f]+ <[^>]*7scratch4kick[^>]*>:' "$work/image.dis" || true)
  kick_refs=$(awk '
    /^[0-9a-f]+ <[^>]*>:$/ { fn = $2; next }
    /7scratch4kick/ && !/7scratch4kick[^>]*\+0x/ { print fn }
  ' "$work/image.dis")
  kick_callers=$(printf '%s' "$kick_refs" | grep -c . || true)
  echo "b2-card: kick defined $kick_defs time(s), referenced from $kick_callers instruction(s)"
  [ "$kick_defs" = 1 ] || { echo "FAIL: the kick is not one function"; fail=1; }
  [ "$kick_callers" = 1 ] || { echo "FAIL: the kick has $kick_callers callers, not exactly one:"; echo "$kick_refs"; fail=1; }
  printf '%s' "$kick_refs" | grep -qE '12VerifiedFeed.*8Watchdog4feed' || { echo "FAIL: the kick's caller is not the gate (VerifiedFeed's feed)"; fail=1; }
  # The identities, by their build ids as the record carries them (little-endian): the scratch build's present, the
  # check build's absent.
  python3 -I - "$work/image.bin" <<'PY' || fail=1
import sys
image = open(sys.argv[1], "rb").read()
scratch = (0x4b32_3330_4232_0002).to_bytes(8, "little")
check = (0x4b32_3330_4232_0001).to_bytes(8, "little")
print(f"b2-card: scratch build id {image.count(scratch)} time(s), check build id {image.count(check)} time(s)")
if image.count(scratch) != 1 or image.count(check) != 0:
    print("FAIL: the scratch build must carry its own identity once and not the check build's")
    sys.exit(1)
PY
elif [ "$b2" = 0 ]; then
  [ $((end)) -le $((0x80380000)) ] || { echo "FAIL: the image runs into the piece job's address 0x80380000"; fail=1; }
  echo "card image: $((0x80380000 - end)) bytes spare below the piece job's address 0x80380000"
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
