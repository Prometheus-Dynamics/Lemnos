#!/usr/bin/env python3
"""Prints the bench report: next's reference set, with dev's figures beside each item.

Inputs:
  --reference  reference.tsv: the set, its methods, dev's figures and where each came from.
  --measured   measured.tsv: TSV rows (key, status, value, note) from bench.sh. The last row for a
               key wins. A key with no row is printed as not measured.
  --repo       the repository: which drivers are merged on next, and which are registered in lemnosd.

Nothing is filled in. A value appears only when a step measured it, and dev's figures are printed as
dev's, with their source, never as next's.
"""

import argparse
import pathlib


def read_reference(path):
    lines = path.read_text(encoding="utf-8").splitlines()
    head = lines[0].split("\t")
    return [dict(zip(head, line.split("\t"))) for line in lines[1:] if line]


def read_measured(path):
    rows = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        parts = (line.split("\t") + ["", "", "", ""])[:4]
        if parts[0]:
            rows[parts[0]] = parts[1:]
    return rows


def driver_problem(repo, name, builtin):
    if name in ("", "-"):
        return None
    if not (repo / "drivers" / name / "Cargo.toml").is_file():
        return f"driver {name} is not merged on next"
    if f"lemnos_driver_{name.replace('-', '_')}::" not in builtin:
        return f"driver {name} is merged but not registered in lemnosd (service/lemnosd/src/builtin.rs)"
    return None


def assess(item, repo, builtin):
    problems = []
    for name in (d.strip() for d in item["drivers"].split(",")):
        problem = driver_problem(repo, name, builtin)
        if problem:
            problems.append(problem)
    if item["method_status"] == "none":
        problems.append(f"no method on next: {item['method']}")
    if problems:
        return "blocked", problems
    if item["method_status"] == "partial":
        return "partial", [item["method"]]
    return "ok", []


def key_line(key, status, value, note):
    if status == "measured":
        text = value
    else:
        text = "not measured"
    return f"    {key:<22} {text}  ({note})"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--reference", required=True)
    parser.add_argument("--measured", required=True)
    parser.add_argument("--mode", required=True, choices=["dry-run", "board"])
    parser.add_argument("--arch", required=True)
    args = parser.parse_args()

    repo = pathlib.Path(args.repo)
    builtin = (repo / "service" / "lemnosd" / "src" / "builtin.rs").read_text(encoding="utf-8")
    items = read_reference(pathlib.Path(args.reference))
    rows = read_measured(pathlib.Path(args.measured))

    out = []
    w = out.append
    if args.mode == "dry-run":
        w("LEMNOS BENCH REPORT: DRY RUN (simulation only)")
        w("No board was used, no service was stopped, and no hardware was opened.")
        w("Nothing below is a board timing. Values marked 'build host' or 'sim host' are not")
        w("compared with dev's figures.")
    else:
        w(f"LEMNOS BENCH REPORT: BOARD RUN on the board (machine type {args.arch})")
        w("Timings are board only. Dev's figures are printed beside each item for comparison.")
    w("Dev's figures are dev's (TODO.md at ed43ff3, docs/design.md), each with its source. They are")
    w("not measured here.")

    lemnosd_keys = [key for key in rows if key.startswith("lemnosd.")]
    if lemnosd_keys:
        w("")
        w("lemnosd on the bench board file")
        for key in lemnosd_keys:
            status, value, note = rows[key]
            if key == "lemnosd.devices" and status == "measured":
                w(f"    {key}  ({note})")
                for device in value.split(";"):
                    w(f"      {device}")
            else:
                w(key_line(key, status, value, note))

    blocked = []
    partial = []
    for item in items:
        state, problems = assess(item, repo, builtin)
        if state == "blocked":
            blocked.append((item, problems))
        elif state == "partial":
            partial.append((item, problems))
        w("")
        w(f"{item['item']}  [{state}]")
        prefixes = [p.strip() for p in item["keys"].split(",")]
        keys = [key for key in rows if any(key.startswith(p) for p in prefixes)]
        w("  next:")
        if not keys:
            w("    no row for this item")
        for key in keys:
            status, value, note = rows[key]
            w(key_line(key, status, value, note))
        w("  dev:")
        w(f"    {item['dev']}")
        w(f"    source: {item['dev_source']}")
        w("  method on next:")
        w(f"    {item['method']}")
        if state == "blocked":
            w(f"  blocked: {'; '.join(problems)}")

    w("")
    w("REFERENCE ITEMS WITH NO DRIVER OR METHOD ON NEXT")
    if blocked:
        for item, problems in blocked:
            w(f"  - {item['item']}: {'; '.join(problems)}")
    else:
        w("  none")
    w("")
    w("PARTIAL (a method exists for some of the item, see its row)")
    if partial:
        for item, problems in partial:
            w(f"  - {item['item']}: {problems[0]}")
    else:
        w("  none")
    print("\n".join(out))


main()
