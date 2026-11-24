#!/usr/bin/env python3
"""check-features.sh's readers of cargo's output.

    features-units.py test-unit <crate>          a unit graph on stdin; prints the crate's library test
                                                 unit: its features, then each dependency unit
                                                 (package, mode, features), " | " between them
    features-units.py executable <crate> <file>  cargo's JSON messages in <file>; prints the path of the
                                                 crate's library test program

Each fails when what it looks for is not there exactly once.
"""
import json
import sys


def test_unit(crate: str) -> int:
    units = json.load(sys.stdin)["units"]
    name = crate.replace("-", "_")
    found = [u for u in units
             if u["mode"] == "test" and u["target"]["name"] == name and u["target"]["kind"][0] in ("lib", "rlib")]
    if len(found) != 1:
        print(f"features-units.py: {len(found)} library test units for {crate}, not one", file=sys.stderr)
        return 1
    (unit,) = found
    seen, todo, deps = set(), [d["index"] for d in unit["dependencies"]], []
    while todo:
        i = todo.pop()
        if i in seen:
            continue
        seen.add(i)
        u = units[i]
        deps.append(u["pkg_id"] + " " + u["mode"] + " " + ",".join(u["features"]))
        todo += [d["index"] for d in u["dependencies"]]
    print(",".join(sorted(set(unit["features"]) - {"default"})) or "-", *sorted(deps), sep=" | ")
    return 0


def executable(crate: str, path: str) -> int:
    name = crate.replace("-", "_")
    found = []
    with open(path, encoding="utf-8") as messages:
        for line in messages:
            m = json.loads(line)
            if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == name:
                found.append(m["executable"])
    if len(found) != 1:
        print(f"features-units.py: {len(found)} test programs for {crate} in {path}, not one", file=sys.stderr)
        return 1
    print(found[0])
    return 0


def main() -> int:
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "test-unit":
        return test_unit(args[1])
    if len(args) == 3 and args[0] == "executable":
        return executable(args[1], args[2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
