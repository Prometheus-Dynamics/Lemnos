#!/usr/bin/env python3
"""Checks kernel/lemnos_helpers.c, the Lemnos module's only C, and
testing/qemu/test_block/lemnos_test_helpers.c, the only C of the test fixtures built beside it.

Each file may hold comments, #include lines, one-line prototypes and one-line shims. A shim is a function
whose whole body is one line (`{ ... }`), with a comment line directly above it that names the kernel API it
wraps and why there is no Rust path. A prototype is accepted only when a shim of the same name is defined in
the same file; a prototype with no shim is a problem. Nothing else is allowed. Each file stays within a size
cap, so C does not grow back into the module or its fixtures.

The fixtures' file is also held to the pure-wrapper rule: each shim's body is one call of the wrapped
API, `{ return api(args); }` or `{ api(args); }`, whose arguments are the shim's own parameters in their
order, each at most cast. A branch, a loop, arithmetic, a local or a second call is logic, and logic is
written in Rust.

    scripts/check-kernel-helpers.py                           both files, each against its own cap
    scripts/check-kernel-helpers.py --file PATH [--max-lines N] [--pure]   one file
"""

import argparse
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_MAX_LINES = 120
# The files checked when none is named, each with its cap and whether the pure-wrapper rule holds.
FILES = [
    (REPO / "kernel" / "lemnos_helpers.c", DEFAULT_MAX_LINES, False),
    (REPO / "testing" / "qemu" / "test_block" / "lemnos_test_helpers.c", 40, True),
]

# A one-line prototype: a return type, a name, a parameter list, and a semicolon.
PROTOTYPE = re.compile(r"^[A-Za-z_][A-Za-z_0-9 \t\*]*\b([A-Za-z_][A-Za-z_0-9]*)\s*\([^;{}]*\)\s*;\s*$")
# A one-line shim: a return type, a name, a parameter list, and a body on that same line.
SHIM = re.compile(r"^[A-Za-z_][A-Za-z_0-9 \t\*]*\b([A-Za-z_][A-Za-z_0-9]*)\s*\(([^;{}]*)\)\s*\{([^{}]*)\}\s*$")
# A C cast: a parenthesised type name, with any qualifiers and pointer stars.
CAST = r"(?:\(\s*[A-Za-z_][A-Za-z_0-9 \t]*\**\s*\)\s*)?"
# The pure-wrapper body: an optional return, an optional cast, one call, its argument list, a semicolon.
PURE_BODY = re.compile(r"^\s*(?:return\s+)?" + CAST + r"([A-Za-z_][A-Za-z_0-9]*)\s*\(([^()]*(?:\([^()]*\)[^()]*)*)\)\s*;\s*$")
IDENT = re.compile(r"[A-Za-z_][A-Za-z_0-9]*")


def parameter_names(parameters: str) -> list[str]:
    """The names of a C parameter list, in order: the last identifier of each parameter."""
    if parameters.strip() in ("", "void"):
        return []
    return [IDENT.findall(parameter)[-1] for parameter in parameters.split(",")]


def pure_wrapper_problem(name: str, parameters: str, body: str) -> str | None:
    """Why a shim is not one call of its API with its parameters passed through, or None when it is."""
    match = PURE_BODY.match(body)
    if not match:
        return "its body is not one call (`return api(args);` or `api(args);`)"
    arguments = [] if not match.group(2).strip() else [argument.strip() for argument in match.group(2).split(",")]
    passed = []
    for argument in arguments:
        plain = re.fullmatch(CAST + r"([A-Za-z_][A-Za-z_0-9]*)", argument)
        if not plain:
            return f"argument `{argument}` is not a parameter (at most cast)"
        passed.append(plain.group(1))
    if passed != parameter_names(parameters):
        return f"its arguments {passed} are not its parameters {parameter_names(parameters)} in order"
    if match.group(1) == name:
        return "it calls itself"
    return None


def check(path: pathlib.Path, max_lines: int, pure: bool) -> list[str]:
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
            if pure and (why := pure_wrapper_problem(match.group(1), match.group(2), match.group(3))):
                problems.append(f"{path}:{number}: shim {match.group(1)} holds logic, which belongs in Rust: {why}")
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
    parser.add_argument("--file", type=pathlib.Path)
    parser.add_argument("--max-lines", type=int, default=DEFAULT_MAX_LINES)
    parser.add_argument("--pure", action="store_true", help="hold the file to the pure-wrapper rule")
    args = parser.parse_args()
    files = [(args.file, args.max_lines, args.pure)] if args.file else FILES
    problems = [problem for path, max_lines, pure in files for problem in check(path, max_lines, pure)]
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print("check-kernel-helpers: passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
