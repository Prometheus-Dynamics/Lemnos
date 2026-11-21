#!/usr/bin/env python3
"""Checks packaging/files.txt against the files under packaging/ and socs/ (run by scripts/gate.sh).

Every file under packaging/ is listed once, every SoC map socs/<soc>.toml is listed once at
/usr/share/lemnos/socs/<soc>.toml (where lemnosd reads it) under `soc`, the map a board file on the image names, and every listed source exists. Each line has four
fields (source, install path, mode, when), and `when` uses only the fixed vocabulary. An install path
is absolute or BOOT/<name>, and it must be named in the source's own header (its first 25 lines), so
a source says where it goes. Two lines may name one install path only as the U-Boot alternatives
(`uboot` with `ubi`): a board boots from one medium. Nothing is installed here.
"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIST = os.path.join(ROOT, "packaging", "files.txt")
VOCAB = {"always", "soc", "systemd", "busybox", "udev", "orion", "gadget", "health-tcp", "slots", "security", "kmod", "uboot", "ubi", "never"}
MODES = {"0644", "0755", "append"}
ALTERNATIVES = {frozenset({"uboot", "ubi"})}  # install paths two lines may share, by their `when`
SOCS = "/usr/share/lemnos/socs"  # where lemnosd reads the SoC maps (service/lemnosd/src/main.rs)

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
        soc_map = source.startswith("socs/")
        if not (source.startswith("packaging/") or soc_map) or not os.path.isfile(os.path.join(ROOT, source)):
            fail(f"{where}: {source} does not exist")
        if soc_map and (install != f"{SOCS}/{os.path.basename(source)}" or mode != "0644" or when != "soc"):
            fail(f"{where}: {source}: a SoC map installs to {SOCS}/{os.path.basename(source)}, 0644, soc")
        if not soc_map and "soc" in when.split(","):
            fail(f"{where}: {source}: `soc` is only for the SoC maps under socs/")
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
        if not soc_map and os.path.isfile(os.path.join(ROOT, source)):
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

for name in sorted(os.listdir(os.path.join(ROOT, "socs"))):
    rel = f"socs/{name}"
    if name.endswith(".toml") and rel not in listed:
        fail(f"{rel} is a SoC map but not in packaging/files.txt (each map is listed, for the image whose board file names it)")

with open(os.path.join(ROOT, "service", "lemnosd", "src", "main.rs"), encoding="utf-8") as f:
    if f'"{SOCS}"' not in f.read():
        fail(f"service/lemnosd/src/main.rs no longer reads the SoC maps from {SOCS}")

# Each board file in testing/boards/ is checked against the drop-ins its devices need. A board
# with a system-slots device needs slots.conf, listed under `slots`, granting that device's state and
# image dirs (<data>/board/slots and <data>/slots). A board with a [security] table needs
# security.conf, listed under `security`, granting /run/board and the record's directory when the
# record is a file. Without the grant the daemon's write is refused under ProtectSystem=strict, so
# each gap fails here by the board's name.
import tomllib


def read_paths(source):
    """The ReadWritePaths of a drop-in, without the `-` that makes a path optional."""
    paths = []
    with open(os.path.join(ROOT, source), encoding="utf-8") as f:
        for line in f:
            if line.startswith("ReadWritePaths="):
                paths += [p.lstrip("-") for p in line.split("=", 1)[1].split()]
    return paths


def granted(path, paths):
    return any(path == p or path.startswith(p.rstrip("/") + "/") for p in paths)


DROPINS = {
    "slots": "packaging/systemd/lemnosd.service.d/slots.conf",
    "security": "packaging/systemd/lemnosd.service.d/security.conf",
}
for word, source in DROPINS.items():
    if source not in entries or word not in entries[source][2].split(","):
        fail(f"{source} is not listed under `{word}` in packaging/files.txt")
boards = os.path.join(ROOT, "testing", "boards")
for name in sorted(os.listdir(boards)):
    if not name.endswith(".toml"):
        continue
    with open(os.path.join(boards, name), "rb") as f:
        board = tomllib.load(f)
    where = f"testing/boards/{name}"
    for device in board.get("devices", []):
        if device.get("driver") != "system-slots":
            continue
        data = device.get("config", {}).get("data", "/data").rstrip("/")
        paths = read_paths(DROPINS["slots"]) if os.path.isfile(os.path.join(ROOT, DROPINS["slots"])) else []
        for need in (f"{data}/board/slots", f"{data}/slots"):
            if not granted(need, paths):
                fail(f"{where}: system-slots device {device.get('id')!r} writes {need}, which no slots drop-in grants ({DROPINS['slots']})")
    security = board.get("security")
    if security is not None:
        paths = read_paths(DROPINS["security"]) if os.path.isfile(os.path.join(ROOT, DROPINS["security"])) else []
        needs = ["/run/board"]
        record = security.get("record", "")
        if record and record != "boot":
            needs.append(os.path.dirname(record))
        for need in needs:
            if not granted(need, paths):
                fail(f"{where}: [security] writes {need}, which no security drop-in grants ({DROPINS['security']})")

if failures:
    for msg in failures:
        print(f"check-packaging-files: {msg}", file=sys.stderr)
    print(f"check-packaging-files: failed ({len(failures)})", file=sys.stderr)
    sys.exit(1)
print(f"check-packaging-files: holds ({len(entries)} files listed, every file under packaging/ and every SoC map is among them)")
