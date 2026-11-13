#!/usr/bin/env python3
"""Checks packaging/files.txt against the files under packaging/ (run by scripts/gate.sh).

Every file under packaging/ is listed once, and every listed source exists. Each line has four
fields (source, install path, mode, when), and `when` uses only the fixed vocabulary. An install path
is absolute or BOOT/<name>, and it must be named in the source's own header (its first 25 lines), so
a source says where it goes. Two lines may name one install path only as the U-Boot alternatives
(`uboot` with `ubi`): a board boots from one medium. Nothing is installed here.
"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIST = os.path.join(ROOT, "packaging", "files.txt")
VOCAB = {"always", "systemd", "busybox", "udev", "orion", "gadget", "kmod", "uboot", "ubi", "never"}
MODES = {"0644", "0755", "append"}
ALTERNATIVES = {frozenset({"uboot", "ubi"})}  # install paths two lines may share, by their `when`

failures = []


def fail(msg):
    failures.append(msg)


entries = {}  # source -> (lineno, install, when)
installs = {}  # install path -> [(source, when)]
with open(LIST, encoding="utf-8") as f:
    for lineno, raw in enumerate(f, 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        where = f"packaging/files.txt:{lineno}"
        if len(fields) != 4:
            fail(f"{where}: want 4 fields (source, install path, mode, when), got {len(fields)}: {line}")
            continue
        source, install, mode, when = fields
        if source in entries:
            fail(f"{where}: {source} is listed twice (first at line {entries[source][0]})")
            continue
        entries[source] = (lineno, install, when)
        if not source.startswith("packaging/") or not os.path.isfile(os.path.join(ROOT, source)):
            fail(f"{where}: {source} does not exist")
        words = when.split(",")
        bad = [w for w in words if w not in VOCAB]
        if bad:
            fail(f"{where}: {source}: unknown condition {', '.join(bad)} (vocabulary: {', '.join(sorted(VOCAB))})")
        if ("always" in words or "never" in words) and len(words) != 1:
            fail(f"{where}: {source}: `always` and `never` stand alone")
        if "never" in words:
            if install != "-":
                fail(f"{where}: {source} is `never` but names an install path")
            if mode != "-":
                fail(f"{where}: {source} is `never` but gives a mode")
            continue
        if mode not in MODES:
            fail(f"{where}: {source}: mode {mode} is not 0644, 0755 or append")
        if not (install.startswith("/") or install.startswith("BOOT/")):
            fail(f"{where}: {source}: install path {install} is not absolute or BOOT/<name>")
            continue
        installs.setdefault(install, []).append((source, frozenset(words)))
        if os.path.isfile(os.path.join(ROOT, source)):
            with open(os.path.join(ROOT, source), encoding="utf-8", errors="replace") as src:
                header = "".join([next(src, "") for _ in range(25)])
            if install not in header:
                fail(f"{where}: {source} does not name its install path {install} in its header")

for install, users in installs.items():
    if len(users) < 2:
        continue
    sets = [words for _, words in users]
    alternatives = all(frozenset(a | b) in ALTERNATIVES for i, a in enumerate(sets) for b in sets[i + 1:])
    if len(users) != 2 or not alternatives:
        names = ", ".join(s for s, _ in users)
        fail(f"install path {install} is taken by more than one line: {names}")

listed = set(entries)
for dirpath, _, names in os.walk(os.path.join(ROOT, "packaging")):
    for name in names:
        rel = os.path.relpath(os.path.join(dirpath, name), ROOT).replace(os.sep, "/")
        if rel not in listed:
            fail(f"{rel} is under packaging/ but not in packaging/files.txt (list it, or mark it never)")

if failures:
    for msg in failures:
        print(f"check-packaging-files: {msg}", file=sys.stderr)
    print(f"check-packaging-files: failed ({len(failures)})", file=sys.stderr)
    sys.exit(1)
print(f"check-packaging-files: holds ({len(entries)} files listed, every file under packaging/ is among them)")
