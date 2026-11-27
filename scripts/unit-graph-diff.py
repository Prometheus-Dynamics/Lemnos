#!/usr/bin/env python3
"""unit-graph-diff.py GROUP MERGED.json CRATE=ALONE.json...

Compares cargo unit graphs (cargo ... --unit-graph): every unit of each crate built alone must be in the
merged graph with the same features. Prints one line per difference, naming the crate, and exits 1 if
there is any; prints nothing and exits 0 if none.
"""
import collections
import json
import sys


def units(path):
    out = collections.defaultdict(set)
    with open(path, encoding="utf-8") as f:
        graph = json.load(f)
    for u in graph["units"]:
        key = (u["pkg_id"], u["target"]["kind"][0], u["mode"], u["target"]["name"])
        out[key].add(tuple(sorted(u["features"])))
    return out


def name(pkg_id):
    # "path+file:///...#lemnos-wire@2.0.1-dev" or "registry+...#syn@3.0.10"
    return pkg_id.rsplit("#", 1)[-1]


def main():
    group, merged_path, alone = sys.argv[1], sys.argv[2], sys.argv[3:]
    merged = units(merged_path)
    bad = 0
    for arg in alone:
        crate, path = arg.split("=", 1)
        for key, feats in sorted(units(path).items()):
            got = merged.get(key)
            what = f"{name(key[0])} {key[1]}/{key[2]} {key[3]}"
            if got is None:
                print(f"merged-sets: {crate} ({group}): {what} is built alone but not in the merged call")
                bad += 1
            elif got != feats:
                show = lambda s: " | ".join(",".join(f) or "(none)" for f in sorted(s))
                print(f"merged-sets: {crate} ({group}): {what} has features [{show(feats)}] alone, [{show(got)}] merged")
                bad += 1
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
