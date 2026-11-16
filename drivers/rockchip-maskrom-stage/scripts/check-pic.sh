#!/usr/bin/env bash
# Position independence of the 472 loader image, proved by comparing two links of the same objects.
#
# The shipped image: the -sys crate's staticlib (the prologue in stage471.S first, then the Rust body and
# the stage crate), linked as a static PIE with --no-dynamic-linker and loader472.ld. The image's link base
# is the symbol BASE. The flat image the boot ROM loads starts at BASE: its first byte is the prologue.
#
# Checks (like with like: two PIE links of the same objects, same script, BASE=0 and BASE=X):
#   1. only R_AARCH64_RELATIVE relocations; .got and .got.plt are discarded (absent or zero size);
#   2. the image starts at BASE and the entry is BASE (the first instruction is the prologue);
#   (a) the flat images at BASE=0 and BASE=X are byte-identical before the relocation table, outside the
#       relocated slots (code and read-only data do not depend on the base);
#   (b) the two relocation tables have the same offsets (image-relative), and every addend differs by X;
#   (c) the BASE=0 image with its table applied at load base X equals the BASE=X image with its table applied
#       at load base X, before the table.
# The code that applies the table at load time is reloc::apply_relative (reloc.rs, host-tested); the
# arithmetic here mirrors it for the comparison.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
CARGO="${CARGO:-cargo}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
TRIPLE=aarch64-unknown-none
SCRIPT="$ROOT/drivers/rockchip-maskrom-stage/src/loader472.ld"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/check-pic.XXXXXX")"
trap 'rm -rf "$OUT"' EXIT
bad=0
X1=0x00a00000
X2=0x60010000

$CARGO rustc -p lemnos-driver-rockchip-maskrom-sys --target "$TRIPLE" --crate-type staticlib \
  -- -C relocation-model=pic >"$OUT/build.log" 2>&1 || { cat "$OUT/build.log"; echo "check-pic: build failed"; exit 1; }
LIB="$TARGET_DIR/$TRIPLE/debug/liblemnos_driver_rockchip_maskrom_sys.a"
[ -f "$LIB" ] || { echo "check-pic: no staticlib at $LIB"; exit 1; }

# The 471 stub tail-jumps to lemnos_ddr_blob (the bench's DDR init, not in this repository): a stand-in.
link_at() { # base, out
  aarch64-linux-gnu-ld -pie --no-dynamic-linker -z noseparate-code -T "$SCRIPT" \
    -e lemnos_loader472_entry --gc-sections --defsym=BASE="$1" --defsym=lemnos_ddr_blob=lemnos_arm_and_spin \
    -o "$2" "$LIB" 2>"$2.log" || { cat "$2.log"; echo "check-pic: link at $1 failed"; exit 1; }
}
link_at 0 "$OUT/b0.elf"
link_at "$X1" "$OUT/bx1.elf"
link_at "$X2" "$OUT/bx2.elf"

# 1. Relocations and sections.
if aarch64-linux-gnu-readelf -rW "$OUT/b0.elf" | awk '/R_AARCH64|R_X86/ { print $3 }' | grep -v '^R_AARCH64_RELATIVE$' | grep -q .; then
  echo "check-pic: a relocation other than R_AARCH64_RELATIVE is in the image"; bad=1
else
  echo "check-pic: only R_AARCH64_RELATIVE relocations: $(aarch64-linux-gnu-readelf -rW "$OUT/b0.elf" | grep -c R_AARCH64_RELATIVE)"
fi
# The linker keeps empty .got and .got.plt headers (its own, for the PIE), but no object emits a GOT-type
# relocation, so nothing reads them: they must hold only zeros, and no relocation may target them.
if aarch64-linux-gnu-readelf -rW "$LIB" | grep -qE 'R_AARCH64_(ADR_GOT|LD64_GOT|MOVW_GOT)|GOT'; then
  echo "check-pic: an object has a GOT-type relocation"; bad=1
fi
aarch64-linux-gnu-objcopy -O binary "$OUT/b0.elf" "$OUT/f0"
python3 -I - "$OUT/b0.elf" "$OUT/f0" <<'PY' || bad=1
import re, subprocess, sys
elf, flat_path = sys.argv[1], sys.argv[2]
text = subprocess.run(["aarch64-linux-gnu-readelf", "-SW", elf], capture_output=True, text=True).stdout
pat = re.compile(r"\[\s*\d+\]\s+(\S+)\s+\S+\s+([0-9a-f]+)\s+[0-9a-f]+\s+([0-9a-f]+)")
secs = {m.group(1): (int(m.group(2), 16), int(m.group(3), 16)) for m in map(pat.search, text.splitlines()) if m}
flat = open(flat_path, "rb").read()
rel = subprocess.run(["aarch64-linux-gnu-readelf", "-rW", elf], capture_output=True, text=True).stdout
targets = [int(l.split()[0], 16) for l in rel.splitlines() if "R_AARCH64_RELATIVE" in l]
bad = []
for name in (".got", ".got.plt", ".igot.plt", ".dynamic", ".dynsym"):
    addr, size = secs.get(name, (0, 0))
    if not size:
        continue
    if any(flat[addr + i] for i in range(size)):
        bad.append(f"{name} at {addr:#x} holds non-zero bytes")
    if any(addr <= t < addr + size for t in targets):
        bad.append(f"a relocation targets {name}")
if bad:
    print("check-pic: GOT or dynamic sections are not empty: " + "; ".join(bad)); sys.exit(1)
print("check-pic: GOT and dynamic sections hold only zeros, unreferenced: " +
      ", ".join(f"{n} {secs[n][1]:#x}" for n in (".got", ".got.plt") if n in secs))
PY

# A flat image: the loaded bytes from the first allocated section, which must be the base.
first_alloc() {
  aarch64-linux-gnu-readelf -SW "$1" | python3 -I -c '
import re, sys
pat = re.compile(r"\[\s*\d+\]\s+(\S*)\s+(\S+)\s+([0-9a-f]+)\s+[0-9a-f]+\s+[0-9a-f]+\s+[0-9a-f]+\s+(\S*)")
best = None
for line in sys.stdin:
    m = pat.search(line)
    if m and "A" in m.group(4) and (best is None or int(m.group(3), 16) < best):
        best = int(m.group(3), 16)
print(hex(best))
'
}
flat() { # elf, out, base
  aarch64-linux-gnu-objcopy -O binary "$1" "$2"
  start=$(first_alloc "$1")
  [ "$(( start ))" -eq "$(( $3 ))" ] || { echo "check-pic: first loaded section $start is not the base $3"; bad=1; }
}
flat "$OUT/b0.elf" "$OUT/f0" 0
flat "$OUT/bx1.elf" "$OUT/fx1" "$X1"
flat "$OUT/bx2.elf" "$OUT/fx2" "$X2"

# 2. The entry is the image's first byte.
entry=$(aarch64-linux-gnu-readelf -h "$OUT/b0.elf" | awk '/Entry point/ { print $4 }')
echo "check-pic: entry $entry, image base 0x0"
[ "$(( entry ))" -eq 0 ] || { echo "check-pic: the entry is not at the image's first byte"; bad=1; }

# The comparisons, in python: tables from readelf -rW, flats from objcopy. Addresses from the link are
# absolute (they include the base); every offset compared here is image-relative (address minus base).
python3 -I - "$OUT" "$OUT/b0.elf" "$OUT/bx1.elf" "$OUT/bx2.elf" "$X1" "$X2" <<'PY' || bad=1
import subprocess, sys
out, b0, bx1, bx2, x1s, x2s = sys.argv[1:]
X1, X2 = int(x1s, 0), int(x2s, 0)
WORD = 2**64 - 1

def readelf(flag, elf):
    return subprocess.run(["aarch64-linux-gnu-readelf", flag, elf], capture_output=True, text=True).stdout

def table(elf, base):
    rows = []
    for line in readelf("-rW", elf).splitlines():
        parts = line.split()
        if len(parts) >= 4 and parts[2] == "R_AARCH64_RELATIVE":
            rows.append((int(parts[0], 16) - base, int(parts[3], 16) - base))  # (image offset, target offset)
    return rows

def table_start(elf, base):
    for line in readelf("-sW", elf).splitlines():
        parts = line.split()
        if parts and parts[-1] == "__rela_start":
            return int(parts[1], 16) - base
    raise SystemExit("check-pic: __rela_start not found")

def apply(image, rows, load_base):
    # reloc::apply_relative: slot = load_base + addend, where addend is the link-relative target.
    img = bytearray(image)
    for off, target in rows:
        img[off:off+8] = ((load_base + target) & WORD).to_bytes(8, "little")
    return bytes(img)

f0 = open(f"{out}/f0", "rb").read()
fx1 = open(f"{out}/fx1", "rb").read()
fx2 = open(f"{out}/fx2", "rb").read()
t0, t1, t2 = table(b0, 0), table(bx1, X1), table(bx2, X2)
ts0, ts1, ts2 = table_start(b0, 0), table_start(bx1, X1), table_start(bx2, X2)
ok = True
print(f"check-pic: {len(t0)} relocation slots; table at image offset {ts0:#x}")
if not (ts0 == ts1 == ts2) or not (len(t0) == len(t1) == len(t2)):
    print("check-pic: the tables differ in size or position across bases"); ok = False

# The slots' 8 bytes are the only bytes allowed to depend on the base before the table.
slots = set()
for off, _ in t0:
    slots.update(range(off, off + 8))

# (a) before the table, outside the relocated slots, the flat images are byte-identical
for name, flat in (("X1", fx1), ("X2", fx2)):
    diff = [i for i in range(ts0) if i not in slots and flat[i] != f0[i]]
    if diff:
        print(f"check-pic: (a) {name}: differs from BASE=0 outside the slots, first at {diff[0]:#x}"); ok = False
    else:
        print(f"check-pic: (a) {name}: identical to BASE=0 before the table outside the {len(t0)} slots ({ts0:#x} bytes)")

# (b) the tables have the same image offsets, and every target differs by exactly the base's own shift
for name, t, base in (("X1", t1, X1), ("X2", t2, X2)):
    if [o for o, _ in t] != [o for o, _ in t0] or [a for _, a in t] != [a for _, a in t0]:
        print(f"check-pic: (b) {name}: the table is not the BASE=0 table, image-relative"); ok = False
    else:
        print(f"check-pic: (b) {name}: same image offsets; link addends differ by exactly {base:#x}")

# (c) the BASE=0 image with its table applied at load base X equals the BASE=X image with its table applied
#     at load base X, before the table.
for name, flat, t, X in (("X1", fx1, t1, X1), ("X2", fx2, t2, X2)):
    a = apply(f0, t0, X)[:ts0]
    b = apply(flat, t, X)[:ts0]
    if a == b:
        print(f"check-pic: (c) {name}: BASE=0 image relocated to {X:#x} equals the BASE={X:#x} image relocated to {X:#x}")
    else:
        diff = next(i for i in range(ts0) if a[i] != b[i])
        print(f"check-pic: (c) {name}: differ at {diff:#x}"); ok = False
sys.exit(0 if ok else 1)
PY

[ "$bad" -eq 0 ] && echo "check-pic: ok"
exit "$bad"
