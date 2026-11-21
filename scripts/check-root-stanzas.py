#!/usr/bin/env python3
"""Every QEMU stanza that starts lemnosd starts it as the packaged unit does (run by scripts/gate.sh).

The guest's checks are the packages testing/qemu/run.sh builds. A check starts lemnosd when one of
its source files spawns a program with lemnosd's `--socket` flag. Each such file must start it
through the unit launcher (`lemnos-unit-check exec`: the unit's capability set, system call filter
and file system sandbox, then the daemon's own drop to `--user`), or carry a `ROOT-STANZA:` line that
says why the daemon must run as root there. The stanzas still run as root are printed, so the list is
in every gate log.
"""
import os
import re
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CRATES = os.path.join(ROOT, "testing", "crates")

with open(os.path.join(ROOT, "testing", "qemu", "run.sh"), encoding="utf-8") as f:
    built = set(re.findall(r"-p (lemnos[-a-z0-9]*)", f.read()))

failures = []
as_root = []
launched = []
for crate in sorted(os.listdir(CRATES)):
    manifest = os.path.join(CRATES, crate, "Cargo.toml")
    if not os.path.isfile(manifest):
        continue
    with open(manifest, "rb") as f:
        name = tomllib.load(f).get("package", {}).get("name", "")
    if name not in built:
        continue
    for dirpath, _, names in os.walk(os.path.join(CRATES, crate, "src")):
        for file in sorted(names):
            if not file.endswith(".rs"):
                continue
            path = os.path.join(dirpath, file)
            rel = os.path.relpath(path, ROOT)
            with open(path, encoding="utf-8") as f:
                text = f.read()
            if "Command::new" not in text or '"--socket"' not in text:
                continue
            if "lemnos-unit-check" in text:
                launched.append(rel)
                continue
            reason = re.search(r"ROOT-STANZA:\s*(.+)", text)
            if reason:
                as_root.append((rel, reason.group(1).strip()))
            else:
                failures.append(
                    f"{rel} starts lemnosd without the unit launcher (lemnos-unit-check exec) and says no "
                    "ROOT-STANZA: reason"
                )

for rel in launched:
    print(f"check-root-stanzas: under the unit: {rel}")
for rel, reason in as_root:
    print(f"check-root-stanzas: as root: {rel}: {reason}")
if failures:
    for msg in failures:
        print(f"check-root-stanzas: {msg}", file=sys.stderr)
    sys.exit(1)
print(f"check-root-stanzas: holds ({len(launched)} under the unit, {len(as_root)} as root with a reason)")
