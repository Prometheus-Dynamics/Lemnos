#!/usr/bin/env bash
# The FEL thunk, checked three ways: its built bytes are the pinned words, its data symbols sit at the layout's
# offsets, and the pinned bytes run correctly under user-mode emulation.
#
# For each target (armv7a-none-eabi, aarch64-unknown-none) this builds the -sys crate's staticlib, takes the bytes of
# its `.text.lemnos_fel_thunk` section from the object, and checks:
#   1. the section's length is the layout's blob length;
#   2. each data symbol sits at the offset the layout gives (table, entry, flag, watchdog words, save area);
#   3. the bytes equal the pinned words in lemnos-allwinner-fel-blob's src/pinned.rs.
# Then the harness (src/harness.rs) runs the pinned bytes for armv7 and aarch64 under qemu-user, linked statically
# by clang with the toolchain's rust-lld (scripts/emu-link.sh). A missing tool fails the script; nothing is skipped.
#
# The armv7 build needs core built from the rust-src component: it runs with -Zbuild-std=core under
# RUSTC_BOOTSTRAP=1, which needs no download.
#
#   check-blob.sh          check the build against the pins, then run the harness under emulation
#   check-blob.sh --emit   print the built words for each target, in the pinned.rs format
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
CARGO="${CARGO:-cargo}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
HERE="$ROOT/drivers/allwinner-fel-sys/scripts"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/check-blob.XXXXXX")"
trap 'rm -rf "$OUT"' EXIT
MODE=check
[ "${1:-}" = "--emit" ] && MODE=emit
LIB_NAME=liblemnos_driver_allwinner_fel_sys.a
SECTION=.text.lemnos_fel_thunk
bad=0

build() { # triple
  local triple="$1"
  if [ "$triple" = armv7a-none-eabi ]; then
    RUSTC_BOOTSTRAP=1 "$CARGO" -Zbuild-std=core rustc -p lemnos-driver-allwinner-fel-sys --target "$triple" \
      --crate-type staticlib >"$OUT/$triple.build.log" 2>&1 \
      || { cat "$OUT/$triple.build.log"; echo "check-blob: build failed for $triple"; exit 1; }
  else
    "$CARGO" rustc -p lemnos-driver-allwinner-fel-sys --target "$triple" --crate-type staticlib \
      >"$OUT/$triple.build.log" 2>&1 \
      || { cat "$OUT/$triple.build.log"; echo "check-blob: build failed for $triple"; exit 1; }
  fi
}

# Writes $OUT/<triple>.bin (the section's bytes) and $OUT/<triple>.nm (the data symbols' offsets).
extract() { # triple
  local triple="$1" dir="$OUT/$1.objs" found=0 object
  mkdir -p "$dir"
  ( cd "$dir" && ar x "$TARGET_DIR/$triple/debug/$LIB_NAME" )
  for object in "$dir"/*.o; do
    if llvm-objdump -h "$object" | grep -q "$SECTION"; then
      found=$((found + 1))
      # The section is copied as it is in the object. A relocation into it would leave a placeholder where the
      # linker writes an address, so the bytes copied here would not be the bytes that run: refuse one.
      if llvm-readobj -r "$object" | grep -q 'lemnos_fel_thunk'; then
        echo "check-blob: $triple: the thunk section has relocations; its bytes are not the bytes that run"; exit 1
      fi
      llvm-objcopy -O binary --only-section="$SECTION" "$object" "$OUT/$triple.bin"
      llvm-nm -n "$object" | grep ' lemnos_fel_' >"$OUT/$triple.nm"
    fi
  done
  [ "$found" = 1 ] || { echo "check-blob: $triple: the section is in $found objects, want 1"; exit 1; }
}

# The toolchain's rust-lld, under the name clang asks of a GNU-style linker.
mkdir -p "$OUT/bin"
RUST_LLD="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld"
[ -x "$RUST_LLD" ] || { echo "check-blob: no rust-lld in the toolchain at $RUST_LLD"; exit 1; }
ln -sf "$RUST_LLD" "$OUT/bin/ld.lld"

# Runs the harness for one target under its emulator. Prints the test summary; fails unless every test ran and passed.
emulate() { # triple clang-target runner
  local triple="$1" clang_target="$2" runner="$3" key
  key="$(printf '%s' "$triple" | tr 'a-z-' 'A-Z_')"
  command -v clang >/dev/null || { echo "check-blob: clang is needed for the emulation runs"; exit 1; }
  command -v "$runner" >/dev/null || { echo "check-blob: $runner is needed for the emulation runs"; exit 1; }
  env "CARGO_TARGET_${key}_LINKER=$HERE/emu-link.sh" "CARGO_TARGET_${key}_RUNNER=$runner" LINK_TARGET="$clang_target" \
    LD_LLD="$OUT/bin/ld.lld" \
    "$CARGO" test -p lemnos-driver-allwinner-fel-sys --lib --target "$triple" >"$OUT/$triple.test.log" 2>&1 \
    || { cat "$OUT/$triple.test.log"; echo "check-blob: the emulation run failed for $triple"; exit 1; }
  grep -E '^test |^test result' "$OUT/$triple.test.log"
  grep -q '^test result: ok. 6 passed; 0 failed' "$OUT/$triple.test.log" \
    || { echo "check-blob: $triple: the harness did not run all six tests"; exit 1; }
}

for triple in armv7a-none-eabi aarch64-unknown-none; do
  build "$triple"
  extract "$triple"
done

if [ "$MODE" = emit ]; then
  python3 -I - "$OUT" <<'PY'
import sys
out = sys.argv[1]
for name, triple in (("ARMV7", "armv7a-none-eabi"), ("AARCH64", "aarch64-unknown-none")):
    data = open(f"{out}/{triple}.bin", "rb").read()
    assert len(data) % 4 == 0, triple
    words = [int.from_bytes(data[i:i + 4], "little") for i in range(0, len(data), 4)]
    print(f"pub const {name}_WORDS: &[u32] = &[")
    for i in range(0, len(words), 8):
        print("    " + " ".join(f"0x{w:08x}," for w in words[i:i + 8]))
    print("];")
PY
  exit 0
fi

python3 -I - "$ROOT" "$OUT" <<'PY' || bad=1
import re, sys
root, out = sys.argv[1], sys.argv[2]
layout_src = open(f"{root}/drivers/allwinner-fel-blob/src/layout.rs").read()
pinned_src = open(f"{root}/drivers/allwinner-fel-blob/src/pinned.rs").read()

def layout_for(name):
    start = layout_src.index(f"pub const {name}: Layout = Layout {{")
    block = layout_src[start:layout_src.index("};", start)]
    fields = {}
    for key, value in re.findall(r"^\s+(\w+): ([^,]+),", block, re.M):
        value = value.strip().strip('"')
        fields[key] = int(value, 0) if re.fullmatch(r"0x[0-9a-fA-F]+|\d+", value) else value
    return fields

def pinned_for(name):
    start = pinned_src.index(f"pub const {name}_WORDS: &[u32] = &[")
    block = pinned_src[start:pinned_src.index("];", start)]
    body = block[block.index("= &[") + 4:]
    return [int(w, 16) for w in re.findall(r"0x[0-9a-fA-F]{8}", body)]

symbols = {
    "table_offset": "lemnos_fel_table",
    "entry_offset": "lemnos_fel_entry",
    "flag_offset": "lemnos_fel_flag",
    "wdt_addr_offset": "lemnos_fel_wdt_addr",
    "wdt_arm_offset": "lemnos_fel_wdt_arm",
    "wdt_disarm_offset": "lemnos_fel_wdt_disarm",
    "save_offset": "lemnos_fel_save",
}
bad = False
for name, triple in (("ARMV7", "armv7a-none-eabi"), ("AARCH64", "aarch64-unknown-none")):
    layout = layout_for(name)
    data = open(f"{out}/{triple}.bin", "rb").read()
    if len(data) != layout["blob_len"]:
        print(f"check-blob: {triple}: section is {len(data)} bytes, layout says {layout['blob_len']}"); bad = True
    addresses = {}
    for line in open(f"{out}/{triple}.nm").read().splitlines():
        addr, _, sym = line.split()
        addresses[sym] = int(addr, 16)
    for field, sym in symbols.items():
        if addresses.get(sym) != layout[field]:
            print(f"check-blob: {triple}: {sym} at {addresses.get(sym)}, layout {field} says {layout[field]}"); bad = True
    pinned = pinned_for(name)
    built = [int.from_bytes(data[i:i + 4], "little") for i in range(0, len(data), 4)]
    if built != pinned:
        first = next((i for i, (a, b) in enumerate(zip(built, pinned)) if a != b), min(len(built), len(pinned)))
        print(f"check-blob: {triple}: built bytes differ from the pinned words at word {first} "
              f"(built {len(built)} words, pinned {len(pinned)})"); bad = True
    else:
        print(f"check-blob: {triple}: {len(data)} bytes equal the pinned words; offsets match the layout")
sys.exit(1 if bad else 0)
PY
[ "$bad" = 0 ] || exit 1

emulate armv7-unknown-linux-musleabihf armv7-linux-musleabihf qemu-arm-static
emulate aarch64-unknown-linux-musl aarch64-linux-musl qemu-aarch64-static
echo "check-blob: PASS"
