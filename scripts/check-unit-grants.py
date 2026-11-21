#!/usr/bin/env python3
"""Checks the grants lemnosd's packaged unit makes against what its drivers use (run by scripts/gate.sh).

The table is packaging/systemd/grants.toml: one row per thing the daemon may touch that the unit or
the udev rules grant (a device node, a directory, a capability, a syscall group, an address family),
or that a driver reaches through the base unit alone (`how = "none"`), or that the image or the
board's generated fragment grants (`image`, `generated`). Each row names the crates that need it.

The checks, all against the source as it is:

1. The source. Every absolute path literal in the production source of the crates lemnosd links
   (its closure of path dependencies; tests, examples and `#[cfg(test)]` blocks are left out) is
   either covered by a row that names that crate, or the gate fails and names the file and line.
   A driver that gains a path nobody listed fails here, read or write.
2. The rows. Every row's crates exist, and every row is needed: a path row needs a literal of one
   of its crates, and a capability, syscall or address-family row needs its `needle` in one of them.
   A row nothing needs fails: the unit grants nothing no driver needs.
3. The grants. Every grant the packaged files make (the group lines in packaging/udev/70-lemnos.rules;
   ReadWritePaths, RuntimeDirectory, CapabilityBoundingSet, SystemCallFilter and RestrictAddressFamilies
   in packaging/systemd/lemnosd.service and its drop-ins) has a row that names it exactly, in that file,
   and a row names only a grant the file makes. A drop-in's row carries the `when` of its line in
   packaging/files.txt, so it is checked against the boards that install the drop-in.
4. The reach. A udev row's node is matched by the rule's KERNEL and SUBSYSTEM. A write row (`access =
   "rw"`) on a path the unit leaves read-only (ProtectSystem=strict, ProtectKernelTunables=yes) needs a
   ReadWritePaths grant that covers it, or it is under /dev or /run.

Limits, stated here so the table is not read as more than it is: the access of a row is declared by
its author, not inferred from the code; syscall and address-family rows are checked for presence and
for a needle, not for every call a driver makes (the QEMU stanza `unit` runs the real unit with its
filter); and a row `image` is the image's to grant, so the gate lists it and does not verify it.
"""
import fnmatch
import glob
import os
import re
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TABLE = "packaging/systemd/grants.toml"
UNIT = "packaging/systemd/lemnosd.service"
UDEV = "packaging/udev/70-lemnos.rules"
MDEV = "packaging/init/mdev.conf.fragment"
FILES = "packaging/files.txt"
DAEMON = "service/lemnosd"
# The namespaces a driver's path can be in. A literal under one of them is a path the unit must grant or
# the base unit must allow.
NAMESPACES = ("/dev", "/sys", "/proc", "/run", "/data", "/var", "/etc", "/boot", "/usr")
KINDS = {"device", "root", "path", "file", "cap", "syscall", "family"}
HOWS = {"udev", "unit", "drop-in", "none", "image", "generated", "root-phase", "dynamic"}
# A `dynamic` row is a path the source builds from sysfs or from the board's config (no literal to match):
# it is declared with its code site, and it must be tagged `open` with the work that owns it, since the
# package grants it nothing. The gate fails on a row that is neither granted (its `how` is one of the
# grants) nor tagged open.
ACCESS = {"ro", "rw"}
# What the unit's own mount namespace leaves writable, for a path with no ReadWritePaths grant over it. It is
# unit-check's `writable` (testing/crates/unit-check/src/namespace.rs), in the same order: a writable path first;
# with ProtectKernelTunables=yes the tunables table is read-only (TUNABLES, the same list); /sys/fs/cgroup is
# read-only with ProtectControlGroups=yes; /dev, /proc and /sys are writable; anything else is read-only
# (ProtectSystem=strict). /sys is writable: systemd 258 and 259 skip the apivfs bind that would make it
# read-only, so the table's /sys entry is never applied (PWM-SYSFS-UNIT's guest run on 259.10).
TUNABLES = (
    "/proc/acpi", "/proc/apm", "/proc/asound", "/proc/bus", "/proc/fs", "/proc/irq", "/proc/latency_stats",
    "/proc/mtrr", "/proc/scsi", "/proc/sys", "/proc/sysrq-trigger", "/proc/timer_stats",
    "/sys/kernel/debug", "/sys/kernel/tracing",
)
WRITABLE_BASE = ("/dev", "/run", "/proc", "/sys")
# The unit's own file that the root phase's grants are wired in (the call names in its `wired` fields).
PRIVILEGE = "service/lemnosd/src/privilege.rs"
# Directives whose values are grants (the unit and drop-ins). A value's tokens are checked one by one.
GRANT_DIRECTIVES = ("ReadWritePaths", "RuntimeDirectory", "CapabilityBoundingSet", "SystemCallFilter",
                    "RestrictAddressFamilies")

failures = []
lists = {"image": [], "generated": [], "open": []}


def fail(msg):
    failures.append(msg)


def rel(path):
    return os.path.relpath(path, ROOT).replace(os.sep, "/")


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read()


# --- the closure of crates lemnosd links -------------------------------------------------------------

def dependency_dirs(manifest, workspace):
    """The path dependencies of one manifest, normal and per-target, with `workspace = true` resolved."""
    data = tomllib.loads(read(manifest))
    specs = list(data.get("dependencies", {}).items())
    for target in data.get("target", {}).values():
        specs += list(target.get("dependencies", {}).items())
    out = []
    for name, spec in specs:
        if not isinstance(spec, dict):
            continue
        if spec.get("workspace") is True:
            spec = workspace.get(name, {})
        if "path" in spec:
            out.append(os.path.normpath(os.path.join(os.path.dirname(manifest), spec["path"])))
    return out


def closure():
    workspace = tomllib.loads(read("Cargo.toml")).get("workspace", {}).get("dependencies", {})
    seen, todo = set(), [DAEMON]
    while todo:
        crate = todo.pop()
        if crate in seen or crate.startswith("testing/"):
            continue
        seen.add(crate)
        todo += dependency_dirs(os.path.join(crate, "Cargo.toml"), workspace)
    return sorted(seen)


# --- literals ----------------------------------------------------------------------------------------

LITERAL = re.compile(r'"((?:[^"\\]|\\.)*)"')
CONST = re.compile(r'const\s+([A-Za-z_][A-Za-z0-9_]*)\s*:\s*&(?:\'static\s+)?str\s*=\s*"((?:[^"\\]|\\.)*)"\s*;')


def is_test_file(path):
    name = os.path.basename(path)
    return (name in ("tests.rs", "test.rs") or name.endswith("_tests.rs") or name.endswith("_test.rs")
            or "/tests/" in path or "/examples/" in path or "/benches/" in path)


def strip_test_blocks(lines):
    """The lines with every `#[cfg(test)]` item blanked (line numbers kept)."""
    out = list(lines)
    i = 0
    while i < len(lines):
        if lines[i].strip().startswith("#[cfg(test)]") or lines[i].strip().startswith("#[cfg(all(test"):
            depth, started, j = 0, False, i + 1
            while j < len(lines):
                code = re.sub(r'"(?:[^"\\]|\\.)*"', '""', lines[j]).split("//")[0]
                if not started and ";" in code and "{" not in code:
                    break  # `mod tests;`: one line
                for ch in code:
                    if ch == "{":
                        depth += 1
                        started = True
                    elif ch == "}":
                        depth -= 1
                if started and depth <= 0:
                    break
                j += 1
            for k in range(i, min(j, len(lines) - 1) + 1):
                out[k] = ""
            i = j + 1
            continue
        i += 1
    return out


def normalize(literal):
    """A literal as a path: `{...}` holes become `*`, a trailing slash is dropped."""
    path = re.sub(r"\{[^}]*\}", "*", literal)
    return path.rstrip("/") if path != "/" else path


def is_namespace_path(path):
    return any(path == ns or path.startswith(ns + "/") for ns in NAMESPACES) and path.count("/") >= 1


def crate_text(crate):
    """The production source of one crate, tests and examples left out, `#[cfg(test)]` blocks blanked."""
    parts = []
    for dirpath, _, names in os.walk(os.path.join(ROOT, crate, "src")):
        for name in names:
            r = rel(os.path.join(dirpath, name))
            if name.endswith(".rs") and not is_test_file(r):
                parts += strip_test_blocks(read(r).split("\n"))
    return "\n".join(parts)


def hits_in(crate):
    """Every (file, line, normalized path) for the production literals of one crate."""
    found = []
    for dirpath, _, names in os.walk(os.path.join(ROOT, crate, "src")):
        for name in names:
            if not name.endswith(".rs"):
                continue
            path = os.path.join(dirpath, name)
            r = rel(path)
            if is_test_file(r):
                continue
            text = read(r)
            # `const NAME: &str = "value";` in the same file: a `{NAME}` hole in a format string is that value.
            consts = {m.group(1): m.group(2) for m in CONST.finditer(text)}
            lines = strip_test_blocks(text.split("\n"))
            for n, line in enumerate(lines, 1):
                if line.lstrip().startswith("//"):
                    continue
                for m in LITERAL.finditer(line):
                    literal = re.sub(r"\{([A-Za-z_][A-Za-z0-9_]*)\}", lambda h: consts.get(h.group(1), h.group(0)), m.group(1))
                    path_text = normalize(literal)
                    if is_namespace_path(path_text):
                        found.append((r, n, path_text))
    return found


def owner(path, crates):
    """The closure crate that holds a file: the longest directory that is its prefix."""
    best = None
    for crate in crates:
        if path.startswith(crate + "/") and (best is None or len(crate) > len(best)):
            best = crate
    return best


def covers(hit, row):
    """A hit is covered by a row whose path is the same or a pattern that matches it. A `path` row (a
    directory, or a node family) also covers what is under it; a `root` row is one directory alone."""
    row_path = row["path"].rstrip("/")
    if hit == row_path or fnmatch.fnmatchcase(hit, row_path):
        return True
    return row.get("kind") != "root" and hit.startswith(row_path + "/")


# --- the packaged grants -----------------------------------------------------------------------------

def unit_grant_tokens():
    """(file, token, kind, value) for each grant the unit and its drop-ins make."""
    out = []
    for path in [UNIT] + dropins():
        for raw in read(path).split("\n"):
            line = raw.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            if key not in GRANT_DIRECTIVES:
                continue
            for word in value.split():
                # a leading `-` (optional) or `~` (negation, not a grant) is not part of the name
                if word.startswith("~"):
                    continue
                name = word.lstrip("-")
                out.append((path, f"{key}:{name}", key, name))
    return out


def dropins():
    """The drop-ins of the unit, relative to the root, in name order."""
    return [rel(d) for d in sorted(glob.glob(os.path.join(ROOT, "packaging/systemd/lemnosd.service.d/*.conf")))]


def udev_grant_lines():
    """The rules that grant a group: each stripped line with GROUP="lemnos" (comments and blank lines skipped)."""
    out = []
    for raw in read(UDEV).split("\n"):
        line = raw.strip()
        if line and not line.startswith("#") and 'GROUP="lemnos"' in line:
            out.append(" ".join(line.split()))
    return out


def code_lines(path):
    """The lines of a source file that are not comments (a line that starts with `//`, doc comments included)."""
    return "\n".join(line for line in read(path).split("\n") if not line.lstrip().startswith("//"))


def files_when():
    """The `when` words of each installed file, from packaging/files.txt."""
    out = {}
    for raw in read(FILES).split("\n"):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        if len(fields) == 4:
            out[fields[0]] = set(fields[3].split(","))
    return out


def mdev_lines():
    """The lines of the BusyBox fragment that are not comments, whitespace-normalized."""
    return {" ".join(raw.split()) for raw in read(MDEV).split("\n") if raw.strip() and not raw.lstrip().startswith("#")}


def udev_fields(line):
    """(SUBSYSTEM, KERNEL) of one rule line; KERNEL is None when it names none."""
    sub = re.search(r'SUBSYSTEM=="([^"]*)"', line)
    ker = re.search(r'KERNEL=="([^"]*)"', line)
    return (sub.group(1) if sub else None, ker.group(1) if ker else None)


# --- the table ---------------------------------------------------------------------------------------

def load_table():
    with open(os.path.join(ROOT, TABLE), "rb") as f:
        data = tomllib.load(f)
    rows = data.get("grant", [])
    if not rows:
        fail(f"{TABLE}: no rows")
    return rows


def check_schema(rows):
    for i, row in enumerate(rows, 1):
        where = f"{TABLE}: row {i} ({row.get('path', '?')})"
        for key in ("path", "kind", "access", "how", "drivers", "reason"):
            if key not in row:
                fail(f"{where}: no `{key}`")
        if row.get("kind") not in KINDS:
            fail(f"{where}: kind {row.get('kind')!r} is not one of {sorted(KINDS)}")
        if row.get("access") not in ACCESS:
            fail(f"{where}: access {row.get('access')!r} is not ro or rw")
        if row.get("how") not in HOWS:
            fail(f"{where}: how {row.get('how')!r} is not one of {sorted(HOWS)}")
        if not isinstance(row.get("drivers"), list) or not row.get("drivers"):
            fail(f"{where}: `drivers` must name the crates that need it")
        for crate in row.get("drivers") or []:
            if not os.path.isdir(os.path.join(ROOT, crate)):
                fail(f"{where}: driver {crate} is not a crate directory")
        if row.get("how") in ("udev", "unit", "drop-in") and ("via" not in row or "token" not in row):
            fail(f"{where}: a `{row.get('how')}` grant names its `via` file and `token`")
        if row.get("how") in ("image", "generated") and not row.get("reason", "").strip():
            fail(f"{where}: an `{row.get('how')}` grant needs its reason")
        if row.get("how") in ("dynamic", "root-phase"):
            # The code site that builds the path: a file under the repository and a text it must hold.
            site, needle = row.get("site"), row.get("needle")
            if not site or not needle:
                fail(f"{where}: a `{row.get('how')}` row names its code `site` (a file) and `needle` (text in it)")
            elif not os.path.isfile(os.path.join(ROOT, site)):
                fail(f"{where}: its site {site} is not a file")
            elif needle not in read(site):
                fail(f"{where}: its site {site} no longer holds {needle!r}: the row is stale")
        if row.get("how") == "root-phase":
            # A root-phase grant is made only if the root phase calls it: the call (`wired`) is in the privilege file.
            wired = row.get("wired")
            if not wired:
                fail(f"{where}: a `root-phase` row names the call that makes its grant (`wired`)")
            elif not os.path.isfile(os.path.join(ROOT, PRIVILEGE)):
                fail(f"{where}: {PRIVILEGE} is not there, so nothing wires the grant")
            elif not re.search(r"(?<![A-Za-z0-9_])" + re.escape(wired) + r"(?![A-Za-z0-9_])", code_lines(PRIVILEGE)):
                # A whole name, in code: `grant_before_drop_removed` is not the call `grant_before_drop`, and a comment
                # that names the call does not make it.
                fail(f"{where}: the root phase does not call `{wired}` ({PRIVILEGE}): the grant is never made")
        if row.get("how") == "dynamic" and not row.get("open", "").strip():
            fail(f"{where}: a `dynamic` path is granted by no package line: tag it `open` with the work that owns it")
        if row.get("how") == "none" and row.get("access") == "rw" and not base_writable(row.get("path", "").rstrip("/"), unit_flags()):
            fail(f"{where}: `none` is for what the base unit allows: read-only, or write under /dev or /run")


def check_grants(rows, when):
    """Each packaged grant has its row; each row names a grant the files make."""
    tokens = unit_grant_tokens()
    udev = udev_grant_lines()
    made = {(f, t) for f, t, _, _ in tokens}
    made |= {(UDEV, line) for line in udev}
    for row in rows:
        how = row.get("how")
        if how not in ("udev", "unit", "drop-in"):
            continue
        via, token = row.get("via"), row.get("token")
        if (via, token) not in made:
            fail(f"{TABLE}: row {row['path']} names {via} `{token}`, which the packaged files do not make")
            continue
        if how == "udev":
            sub, ker = udev_fields(token)
            if sub is None or row.get("subsystem") != sub:
                fail(f"{TABLE}: row {row['path']}: its rule's SUBSYSTEM is {sub!r}, the row says {row.get('subsystem')!r}")
            node = row.get("node")
            if ker is not None and (node is None or not fnmatch.fnmatchcase(node, ker)):
                fail(f"{TABLE}: row {row['path']}: node {node!r} is not matched by the rule's KERNEL {ker!r}")
            if node is not None and not covers("/dev/" + node, row):
                fail(f"{TABLE}: row {row['path']}: its node /dev/{node} is not under the row")
            # The BusyBox image's counterpart: the same node, as an mdev line the fragment holds.
            want = f"{row.get('mdev')} root:lemnos 0660"
            if row.get("mdev") is None or want not in mdev_lines():
                fail(f"{TABLE}: row {row['path']}: no mdev line `{want}` in {MDEV}")
            if not row["path"].startswith("/dev/"):
                fail(f"{TABLE}: row {row['path']}: a udev grant covers a /dev node")
        elif how == "unit" or how == "drop-in":
            key, name = token.split(":", 1)
            # a RuntimeDirectory names a directory under /run; a path grant names the path itself; a
            # capability, syscall group or address family is named by its own text.
            if key == "RuntimeDirectory":
                name = "/run/" + name
            if row.get("kind") in ("path", "file", "root") and row["path"].rstrip("/") != name.rstrip("/"):
                fail(f"{TABLE}: row {row['path']} names the grant {token}; its path must be {name}")
            if row.get("kind") in ("cap", "syscall", "family") and row["path"] != token.split(":", 1)[1]:
                fail(f"{TABLE}: row {row['path']} names the grant {token}; its path must be {token.split(':', 1)[1]}")
            if how == "drop-in":
                installed = when.get(via)
                want = row.get("when")
                if installed is None or want is None or want not in installed:
                    fail(f"{TABLE}: row {row['path']}: its drop-in {via} is not installed under `{want}` in {FILES}")
    claimed = {f"{r.get('mdev')} root:lemnos 0660" for r in rows if r.get("how") == "udev"}
    for line in sorted(mdev_lines()):
        if line.endswith("root:lemnos 0660") and line not in claimed:
            fail(f"{MDEV}: `{line}` grants a group and no row declares it (its udev row's `mdev`)")
    named = {(r.get("via"), r.get("token")) for r in rows if r.get("how") in ("udev", "unit", "drop-in")}
    for f, t in sorted(made):
        if (f, t) not in named:
            fail(f"{f}: grants `{t}`, which no row in {TABLE} declares")


def check_source(rows, crates):
    """Each production path literal is covered by a row for its crate; each row is needed."""
    per_crate = {}
    for crate in crates:
        per_crate[crate] = hits_in(crate)
    needed = [False] * len(rows)
    for crate, found in per_crate.items():
        for file, line, path in found:
            hit_rows = [i for i, row in enumerate(rows) if crate in row.get("drivers", [])
                        and covers(path, row)]
            if not hit_rows:
                fail(f"{file}:{line}: {crate} uses {path}, which no row in {TABLE} covers for {crate}")
            for i in hit_rows:
                needed[i] = True
    for i, row in enumerate(rows):
        needle = row.get("needle")
        if needle:
            for crate in row.get("drivers", []):
                if needle in crate_text(crate):
                    needed[i] = True
        if not needed[i]:
            fail(f"{TABLE}: row {row['path']} ({row['kind']}, {row['how']}) is needed by none of {', '.join(row.get('drivers', []))}")
        if row["how"] in ("image", "generated"):
            lists[row["how"]].append(row)
        if row["how"] == "dynamic":
            lists["open"].append(row)
    return per_crate


def under(path, root):
    """`path` is `root` or below it, by whole components (namespace.rs `under`)."""
    root = root.rstrip("/")
    return path == root or path.startswith(root + "/")


def unit_flags():
    """The protection settings the unit and its drop-ins make (the last assignment wins, as systemd reads them)."""
    flags = {}
    for path in [UNIT] + dropins():
        for raw in read(path).split("\n"):
            line = raw.strip()
            if "=" in line and not line.startswith("#"):
                key, value = line.split("=", 1)
                if key in ("ProtectKernelTunables", "ProtectControlGroups"):
                    flags[key] = value.strip()
    return flags


def base_writable(path, flags):
    """Whether the unit's namespace leaves `path` writable with no ReadWritePaths over it (namespace.rs `writable`)."""
    tunables = flags.get("ProtectKernelTunables") == "yes"
    cgroups = flags.get("ProtectControlGroups") == "yes"
    if tunables and any(under(path, t) for t in TUNABLES):
        return False
    if under(path, "/sys/fs/cgroup"):
        return not cgroups
    return any(under(path, w) for w in WRITABLE_BASE)


def check_reach(rows):
    """A write row needs a path the unit leaves writable: the namespace's own writable paths (`base_writable`),
    or a ReadWritePaths grant over it."""
    flags = unit_flags()
    writable = [r["path"].rstrip("/") for r in rows if r.get("how") in ("unit", "drop-in") and r.get("kind") == "path"]
    for row in rows:
        if row.get("access") != "rw" or row.get("kind") not in ("path", "file", "root"):
            continue
        if row.get("how") == "dynamic":
            continue  # tagged open: its owner grants it, and the gate lists it
        p = row["path"].rstrip("/")
        if base_writable(p, flags):
            continue
        if not any(under(p, w) for w in writable):
            fail(f"{TABLE}: row {row['path']} is rw, and the unit leaves it read-only (no ReadWritePaths grant covers it)")


def main(argv):
    global ROOT
    if len(argv) == 2 and argv[0] == "--root":
        ROOT = os.path.abspath(argv[1])
    elif argv:
        print("usage: check-unit-grants.py [--root DIR]", file=sys.stderr)
        return 2
    rows = load_table()
    check_schema(rows)
    when = files_when()
    check_grants(rows, when)
    crates = closure()
    if not crates:
        fail("the closure of service/lemnosd is empty")
    check_source(rows, crates)
    check_reach(rows)
    for line in udev_grant_lines():
        if not any(r.get("how") == "udev" and r.get("token") == line for r in rows):
            fail(f"{UDEV}: `{line}` grants a group and no row declares it")
    if failures:
        for msg in failures:
            print(f"check-unit-grants: {msg}", file=sys.stderr)
        print(f"check-unit-grants: failed ({len(failures)})", file=sys.stderr)
        return 1
    for how in ("image", "generated"):
        for row in lists[how]:
            print(f"check-unit-grants: not granted by this package ({how}): {row['path']}: {row['reason']}")
    for row in lists["open"]:
        print(f"check-unit-grants: open ({row['open']}): {row['path']} built at {row['site']}: {row['reason']}")
    print(f"check-unit-grants: holds ({len(rows)} rows, {len(crates)} crates in the daemon's closure, "
          f"{sum(1 for r in rows if r['how'] in ('udev', 'unit', 'drop-in'))} package grants)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
