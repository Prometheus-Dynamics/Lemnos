#!/usr/bin/env python3
"""Checks kernel/lemnos_helpers.c, the Lemnos module's only C.

The file may hold comments, #include lines, one-line prototypes and one-line shims. A shim is a function
whose whole body is one line (`{ ... }`), with a comment line directly above it that names the kernel API it
wraps and why there is no Rust path. A prototype is accepted only when a shim of the same name is defined in
the same file; a prototype with no shim is a problem. Nothing else is allowed. The file stays within a size
cap, so C does not grow back into the module.

    scripts/check-kernel-helpers.py [--file PATH] [--max-lines N]
"""

import argparse
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_FILE = REPO / "kernel" / "lemnos_helpers.c"
DEFAULT_MAX_LINES = 120

# A one-line prototype: a return type, a name, a parameter list, and a semicolon.
PROTOTYPE = re.compile(r"^[A-Za-z_][A-Za-z_0-9 \t\*]*\b([A-Za-z_][A-Za-z_0-9]*)\s*\([^;{}]*\)\s*;\s*$")
# A one-line shim: a return type, a name, a parameter list, and a body on that same line.
SHIM = re.compile(r"^[A-Za-z_][A-Za-z_0-9 \t\*]*\b([A-Za-z_][A-Za-z_0-9]*)\s*\([^;{}]*\)\s*\{[^{}]*\}\s*$")


def check(path: pathlib.Path, max_lines: int) -> list[str]:
    problems: list[str] = []
    lines = path.read_text().splitlines()
    if len(lines) > max_lines:
        problems.append(f"{path}: {len(lines)} lines, over the cap of {max_lines}")
    in_block_comment = False
    previous_is_comment = False
    shims: set[str] = set()
    prototypes: dict[str, int] = {}
    for number, raw in enumerate(lines, start=1):
        line = raw.strip()
        if in_block_comment:
            in_block_comment = "*/" not in line
            previous_is_comment = True
            continue
        if not line:
            previous_is_comment = False
            continue
        if line.startswith("/*"):
            in_block_comment = "*/" not in line
            previous_is_comment = True
            continue
        if line.startswith("//"):
            previous_is_comment = True
            continue
        if line.startswith("#include "):
            previous_is_comment = False
            continue
        if match := PROTOTYPE.match(line):
            prototypes.setdefault(match.group(1), number)
            previous_is_comment = False
            continue
        if match := SHIM.match(line):
            if not previous_is_comment:
                problems.append(f"{path}:{number}: shim {match.group(1)} has no comment above it naming the API and why")
            shims.add(match.group(1))
            previous_is_comment = False
            continue
        problems.append(f"{path}:{number}: not a comment, #include, prototype or one-line shim: {line}")
        previous_is_comment = False
    for name, number in prototypes.items():
        if name not in shims:
            problems.append(f"{path}:{number}: prototype {name} has no shim of the same name in this file")
    print(f"kernel helpers: {len(lines)} lines, {len(shims)} one-line shim(s), {len(prototypes)} prototype(s)")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--file", type=pathlib.Path, default=DEFAULT_FILE)
    parser.add_argument("--max-lines", type=int, default=DEFAULT_MAX_LINES)
    args = parser.parse_args()
    problems = check(args.file, args.max_lines)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print("check-kernel-helpers: passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
