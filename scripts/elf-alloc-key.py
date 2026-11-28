#!/usr/bin/env python3
"""The key of an executable by what it loads: every SHF_ALLOC section's name, address, flags, type
and bytes (a NOBITS section, .bss, by its size), the ELF type and entry point. The build ID is left
out (it hashes the whole file, debug information included), and so is everything not loaded: debug
information, symbol tables, comments. Two builds that load the same bytes at the same addresses
have the same key; any change to code, data, a panic's file and line (in .rodata) or unwind tables
(.eh_frame) moves it.

A file that is not a 64-bit little-endian ELF, or is cut short, is keyed by its whole content: a
miss, never a false match.

    elf-alloc-key.py FILE...       prints "<key>  <file>" per file
    elf-alloc-key.py --self-test   plants (objcopy) that move the key and that must not
"""

import hashlib
import os
import shutil
import struct
import subprocess
import sys
import tempfile

FORMAT = b"elf-alloc-key 1"
SHF_ALLOC = 0x2
SHT_NOBITS = 8
LEFT_OUT = {".note.gnu.build-id"}


def sections(data):
    """(name, type, flags, addr, size, body) of every section, or None when `data` isn't a whole
    64-bit little-endian ELF."""
    if len(data) < 64 or data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        return None
    e_type, = struct.unpack_from("<H", data, 16)
    e_entry, = struct.unpack_from("<Q", data, 24)
    e_shoff, = struct.unpack_from("<Q", data, 40)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 58)
    if e_shentsize != 64 or e_shoff + e_shnum * 64 > len(data) or e_shstrndx >= e_shnum:
        return None
    headers = [struct.unpack_from("<IIQQQQIIQQ", data, e_shoff + i * 64) for i in range(e_shnum)]
    str_off, str_size = headers[e_shstrndx][4], headers[e_shstrndx][5]
    if str_off + str_size > len(data):
        return None
    strtab = data[str_off:str_off + str_size]
    out = []
    for name_off, sh_type, flags, addr, off, size, *_ in headers:
        end = strtab.find(b"\0", name_off)
        if name_off >= len(strtab) or end < 0:
            return None
        name = strtab[name_off:end].decode("utf-8", "replace")
        if sh_type == SHT_NOBITS:
            body = b""
        else:
            if off + size > len(data):
                return None
            body = data[off:off + size]
        out.append((name, sh_type, flags, addr, size, body))
    return e_type, e_entry, out


def key_bytes(data):
    parsed = sections(data)
    if parsed is None:
        return "whole-" + hashlib.sha256(data).hexdigest()
    e_type, e_entry, secs = parsed
    h = hashlib.sha256()
    h.update(FORMAT + b"\0" + f"{e_type}\0{e_entry}\0".encode())
    for name, sh_type, flags, addr, size, body in secs:
        if not flags & SHF_ALLOC or name in LEFT_OUT:
            continue
        h.update(f"{name}\0{addr}\0{flags}\0{sh_type}\0{size}\0".encode())
        h.update(hashlib.sha256(body).digest())
    return h.hexdigest()


def key(path):
    with open(path, "rb") as f:
        return key_bytes(f.read())


def self_test():
    """Plants on a real executable: the key moves for a changed loaded byte, a section made
    loadable, and a different program; it holds for debug information, a non-loaded section and
    a new build ID; non-ELF and cut-short files are keyed whole."""
    exe = shutil.which("true") or "/usr/bin/true"
    failures = []
    with tempfile.TemporaryDirectory(prefix="elfkey.") as tmp:
        base = os.path.join(tmp, "base")
        shutil.copyfile(exe, base)
        k0 = key(base)

        def plant(name, args, same):
            out = os.path.join(tmp, name)
            r = subprocess.run(["objcopy", *args, base, out], capture_output=True, text=True)
            if r.returncode != 0:
                failures.append(f"{name}: objcopy failed: {r.stderr.strip()}")
                return
            k = key(out)
            if (k == k0) != same:
                failures.append(f"{name}: key {'moved' if k != k0 else 'held'}, expected "
                                f"{'held' if same else 'moved'}")

        note = os.path.join(tmp, "note.bin")
        with open(note, "wb") as f:
            f.write(b"not loaded")
        plant("non-alloc section", ["--add-section", f".lemnos.note={note}"], True)
        plant("alloc section", ["--add-section", f".lemnos.note={note}",
                                "--set-section-flags", ".lemnos.note=alloc,load,contents,data"], False)
        plant("strip debug", ["--strip-debug"], True)
        plant("remove comment", ["--remove-section", ".comment"], True)
        # One loaded byte: the first byte of .rodata (or .text) flipped in place.
        data = bytearray(open(base, "rb").read())
        _, _, secs = sections(bytes(data))
        target = next((s for s in secs if s[0] == ".rodata" and s[4]), None) or \
            next(s for s in secs if s[0] == ".text")
        off = bytes(data).find(target[5])
        data[off] ^= 1
        if key_bytes(bytes(data)) == k0:
            failures.append(f"one byte of {target[0]}: key held, expected moved")
        # A different build ID only.
        data = bytearray(open(base, "rb").read())
        bid = next((s for s in secs if s[0] == ".note.gnu.build-id"), None)
        if bid:
            off = bytes(data).find(bid[5])
            data[off + len(bid[5]) - 1] ^= 1
            if key_bytes(bytes(data)) != k0:
                failures.append("build ID: key moved, expected held")
        if shutil.which("ls") and key(shutil.which("ls")) == k0:
            failures.append("another program: same key")
        for name, blob in [("empty", b""), ("text", b"hello\n"), ("cut short", open(base, "rb").read()[:200])]:
            if not key_bytes(blob).startswith("whole-"):
                failures.append(f"{name}: not keyed whole")
    for f in failures:
        print(f"elf-alloc-key self-test: FAIL {f}", file=sys.stderr)
    print(f"elf-alloc-key self-test: {'FAILED' if failures else 'passed'} ({len(failures)} failed)")
    return 1 if failures else 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    for path in argv:
        print(f"{key(path)}  {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
