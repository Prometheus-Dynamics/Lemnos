#!/usr/bin/env python3
"""Reads `cargo metadata --format-version 1` on stdin and prints the workspace's packages, sorted.

    cargo-packages.py           one name per line
    cargo-packages.py --dirs    "<name> <directory of its Cargo.toml>" per line

An empty or unreadable listing is an error, never an empty answer: a caller that loops over the
packages would otherwise check nothing and pass.
"""
import json
import sys


def main() -> int:
    dirs = sys.argv[1:] == ["--dirs"]
    if sys.argv[1:] not in ([], ["--dirs"]):
        print(__doc__, file=sys.stderr)
        return 2
    packages = json.load(sys.stdin)["packages"]
    if not packages:
        print("cargo-packages.py: cargo metadata listed no packages", file=sys.stderr)
        return 1
    for p in sorted(packages, key=lambda p: p["name"]):
        if dirs:
            print(p["name"], p["manifest_path"].rsplit("/", 1)[0])
        else:
            print(p["name"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
