#!/usr/bin/env python3
"""Keeps docs/kernel-config.md honest about lemnos.ko.

1. Every CONFIG_ symbol the module's sources, Kbuild and Kconfig refer to (`CONFIG_X`,
   `IS_ENABLED(CONFIG_X)`, `$(CONFIG_X)`) has a row in the "lemnos.ko" table of
   docs/kernel-config.md.
2. Every row marked `required` is enabled (`=y`) in testing/qemu/kernel.config, the
   harness's configuration, and every row marked `must be unset` is not enabled there.

Exits 1 and says what is wrong; 0 when both hold.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "kernel-config.md"
KERNEL = ROOT / "kernel"
HARNESS = ROOT / "testing" / "qemu" / "kernel.config"
SECTION = "## Symbols of lemnos.ko"
SYMBOL = re.compile(r"(?<![A-Za-z0-9_])(CONFIG_[A-Z0-9_]+)")
ROW = re.compile(r"^\|\s*`(CONFIG_[A-Z0-9_]+)`\s*\|[^|]*\|\s*([^|]+?)\s*\|")


def referenced():
    found = {}
    for path in sorted(KERNEL.rglob("*")):
        if not path.is_file() or path.suffix == ".md":
            continue
        if path.suffix not in (".c", ".h", "") and path.name not in ("Kbuild", "Makefile", "Kconfig"):
            continue
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            for name in SYMBOL.findall(line):
                found.setdefault(name, f"{path.relative_to(ROOT)}:{number}")
    return found


def table():
    text = DOC.read_text()
    start = text.find(SECTION)
    if start < 0:
        sys.exit(f"{DOC.relative_to(ROOT)}: no section {SECTION!r}")
    body = text[start + len(SECTION):]
    end = body.find("\n## ")
    rows = {}
    for line in (body if end < 0 else body[:end]).splitlines():
        match = ROW.match(line)
        if match:
            rows[match.group(1)] = match.group(2).strip()
    return rows


def harness():
    values = {}
    for line in HARNESS.read_text().splitlines():
        match = re.match(r"^(CONFIG_[A-Z0-9_]+)=(\S+)$", line.strip())
        if match:
            values[match.group(1)] = match.group(2)
    return values


def main():
    problems = []
    rows = table()
    for name, where in referenced().items():
        if name not in rows:
            problems.append(f"{where}: {name} is not in the lemnos.ko table of {DOC.relative_to(ROOT)}")
    enabled = harness()
    for name, requirement in rows.items():
        if requirement == "required" and enabled.get(name) != "y":
            problems.append(f"{name} is required for lemnos.ko but {HARNESS.relative_to(ROOT)} does not set it to y")
        if requirement == "must be unset" and enabled.get(name) in ("y", "m"):
            problems.append(f"{name} must be unset for lemnos.ko but {HARNESS.relative_to(ROOT)} sets it")
    for problem in problems:
        print(f"kernel-config: {problem}", file=sys.stderr)
    if problems:
        return 1
    print(f"kernel-config: {len(rows)} symbols in the table; every symbol in kernel/ is listed; "
          f"the harness enables every required one")
    return 0


if __name__ == "__main__":
    sys.exit(main())
