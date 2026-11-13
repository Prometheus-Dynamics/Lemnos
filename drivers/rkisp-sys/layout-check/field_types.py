#!/usr/bin/env python3
"""Checks the generated ISP bindings (src/isp*.rs) for the invariants the byte view relies on.

    field_types.py <src dir>

For every struct in the generated modules, this checks:
- it is `#[repr(C, packed)]`, so alignment is 1 and there is no padding between its fields; or it
  is `repr(C)` with every field one integer type, which has no padding either (no field follows a
  smaller one, and the size is the field count times the width); or it is bindgen's bitfield unit;
- every field's type is an integer of a fixed width, a byte array, a bitfield unit, or another
  struct of these modules, nested to any depth: no `bool`, enum, reference, pointer, function or
  `Option`, because a byte pattern is then not always a valid value;
- no `union` or `enum` is defined.

Exit status 0 when every struct passes; the report names each struct that does not. A struct that
fails has no byte view: `Bytes` is implemented only for the structs listed in `bytes.rs`, and the
compile-time assertions there refuse any that is not packed.
"""
import re
import sys
from pathlib import Path

SCALARS = {"__u8", "__u16", "__u32", "__u64", "__s8", "__s16", "__s32", "__s64",
           "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64"}
# bindgen spells C's integer types through core::ffi.
SCALARS |= {f"::core::ffi::c_{t}" for t in (
    "char", "schar", "uchar", "short", "ushort", "int", "uint", "long", "ulong", "longlong",
    "ulonglong")}
STRUCT = re.compile(r"^pub struct (\w+)")
REPR = re.compile(r"^#\[repr\((.*)\)\]")
FIELD = re.compile(r"^\s*pub (\w+): (.+?),\s*$")
UNION = re.compile(r"^pub (union|enum) ")


def strip_array(ty):
    while True:
        m = re.fullmatch(r"\[(.+); ?\d+ ?(usize)?\]", ty.strip())
        if not m:
            return ty.strip()
        ty = m.group(1)


def main(src):
    failures = []
    structs = {}
    for path in sorted(Path(src).glob("isp*.rs")):
        lines = path.read_text().splitlines()
        repr_next = None
        current = None
        for line in lines:
            if UNION.match(line):
                failures.append(f"{path.name}: {line.strip()} is not allowed")
            m = REPR.match(line)
            if m:
                repr_next = m.group(1)
                continue
            m = STRUCT.match(line)
            if m:
                name = m.group(1)
                current = name
                structs[name] = {"file": path.name, "repr": repr_next, "fields": []}
                repr_next = None
                continue
            if current is None:
                continue
            if line.startswith("}"):
                current = None
                continue
            m = FIELD.match(line)
            if m:
                structs[current]["fields"].append((m.group(1), m.group(2)))

    def ok_type(ty, depth=0):
        ty = strip_array(ty)
        if ty in SCALARS or ty.startswith("__BindgenBitfieldUnit<"):
            return True
        if ty in structs and depth < 32:
            return all(ok_type(t, depth + 1) for _, t in structs[ty]["fields"])
        return False

    for name, info in structs.items():
        if name.startswith("__BindgenBitfieldUnit"):
            continue
        if info["repr"] is None or "packed" not in info["repr"]:
            types = {strip_array(t) for _, t in info["fields"]}
            if info["repr"] == "C" and len(types) == 1 and types <= SCALARS:
                pass
            else:
                failures.append(f"{info['file']}: {name} is not repr(C, packed): {info['repr']}")
        for fname, ty in info["fields"]:
            if not ok_type(ty):
                failures.append(f"{info['file']}: {name}.{fname}: {ty} is not a valid-for-every-bit-pattern type")

    for f in failures:
        print(f)
    print(f"{len(structs)} structs checked, {len(failures)} problems")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
