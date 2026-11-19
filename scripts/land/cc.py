#!/usr/bin/env python3
"""consumer-compile helper (lander tooling, scripts/land; the consumer data is read from LAND_CC_DATA).

  trigger REPO BASE CAND NAMES...  which consumers a change can break; TRIGGER / AFFECT / RESULT yes|no lines
  locknames LOCKFILE               Lemnos-sourced packages of a Cargo.lock: "name<TAB>version"
  shim TREE SHIMS                  add the features named in SHIMS ("package<TAB>feature") where missing (idempotent)
  prepare PRISTINE WORK MDIR TREE LOCKNAMES STUBS
                                   copy a consumer (changed files only) and write its [patch] block for Lemnos
  keys JSONLOG                     normalized error keys of a `cargo check --message-format=json` log
  newerrs BASEKEYS CANDKEYS        multiset difference: NEW and FIXED error keys
  addfeat TREE_ROOT ERRFILE STUBS        add the features a resolution error names to the Lemnos tree copy (exit 1: none)
  autofields JSONLOG FIELDS OUTEDITS  E0063 missing fields with a scratch value -> insert edits (prints the count)
  fix WORK FIXFILE                 apply one consumer rev's scratch edits (JSON list of file/old/new/why)
  coverage JSONLOG METADATA        workspace members checked / failed / not reached (behind a failed dependency)
"""
import collections, json, os, re, subprocess, sys, tomllib

PATCH_URL = "https://github.com/Prometheus-Dynamics/Lemnos.git"
PATCH_BEGIN = "# CC-PATCH-BEGIN (consumer-compile scratch only)"
PATCH_END = "# CC-PATCH-END"

# A changed line counts as possibly public when it is a declaration, an attribute, a signature line,
# a field or variant line, or a macro line. Plain statements in bodies do not count. Doc comments and
# blank lines do not count. A line that matches none of these is not public; anything unsure counts.
SURFACE = re.compile(
    r"^\s*("
    r"pub\b|#!?\[|enum\b|struct\b|union\b|trait\b|type\b|impl\b|mod\b|use\b|extern\b|"
    r"macro_rules!|const\b|static\b|(async\s+)?(unsafe\s+)?(extern\s+\"C\"\s+)?fn\b|where\b|->|"
    r"\)\s*(->.*)?(\{|where\b.*)?\s*$|\$|"
    r"[A-Z][A-Za-z0-9_]*\s*([({,]|$)|"
    r"[a-z_][A-Za-z0-9_]*\s*:(?!:)"
    r")"
)
TEST_DIRS = {"tests", "benches", "examples"}


def git(repo, *args):
    r = subprocess.run(["git", "-C", repo, *args], capture_output=True)
    return r.returncode, r.stdout.decode(errors="replace")


def manifest_path(dirpath):
    return f"{dirpath}/Cargo.toml" if dirpath else "Cargo.toml"


def manifest_name(repo, rev, dirpath):
    rc, out = git(repo, "show", f"{rev}:{manifest_path(dirpath)}")
    if rc:
        return None
    try:
        return (tomllib.loads(out).get("package") or {}).get("name")
    except tomllib.TOMLDecodeError:
        return None


def has_manifest(repo, rev, dirpath):
    rc, _ = git(repo, "cat-file", "-e", f"{rev}:{manifest_path(dirpath)}")
    return rc == 0


def is_test_path(path):
    parts = path.split("/")
    base = parts[-1]
    return (any(p in TEST_DIRS for p in parts[:-1]) or base == "tests.rs"
            or base.endswith("_tests.rs") or base.endswith("_test.rs"))


def owner_crate(repo, base, cand, path):
    """(crate name, crate dir) of the nearest package manifest above path, looked up in cand, then base."""
    d = os.path.dirname(path)
    while True:
        for rev in (cand, base):
            if has_manifest(repo, rev, d):
                name = manifest_name(repo, rev, d)
                if name:
                    return name, d
        if not d:
            return None, None
        d = os.path.dirname(d)


def cmd_trigger(repo, base, cand, names_files):
    locked = {}  # Lemnos crate name -> consumers that lock it
    for nf in names_files:
        cons = os.path.basename(nf).rsplit("-", 1)[0]
        for line in open(nf):
            name = line.split("\t")[0].strip()
            if name:
                locked.setdefault(name, []).append(cons)
    affected = {}  # consumer -> set of crates that reach it
    reasons = []
    _, files = git(repo, "diff", "--name-only", "--no-renames", base, cand)
    files = [f for f in files.splitlines() if f]
    checked = 0
    for path in files:
        if path.endswith("Cargo.lock"):
            continue
        if path == "Cargo.toml":
            reasons.append("TRIGGER workspace Cargo.toml changed (all consumers)")
            for cons_list in locked.values():
                for c in cons_list:
                    affected.setdefault(c, set()).add("(workspace Cargo.toml)")
            continue
        if not path.endswith((".rs", "Cargo.toml")) or is_test_path(path):
            continue
        crate, _cdir = owner_crate(repo, base, cand, path)
        if crate is None or crate not in locked:
            continue
        checked += 1
        why = None
        if path.endswith("Cargo.toml"):
            why = "manifest changed"
        else:
            _, diff = git(repo, "diff", "-U0", "--no-renames", base, cand, "--", path)
            for dl in diff.splitlines():
                if dl[:1] not in ("+", "-") or dl.startswith(("+++", "---")):
                    continue
                body = dl[1:]
                s = body.strip()
                if not s or s.startswith("//"):
                    continue
                if SURFACE.search(body):
                    why = f"{'added' if dl[0] == '+' else 'removed'}: {s[:100]}"
                    break
        if why:
            reasons.append(f"TRIGGER {crate} {path}: {why}")
            for c in locked[crate]:
                affected.setdefault(c, set()).add(crate)
    for line in reasons:
        print(line)
    for c in sorted(affected):
        for crate in sorted(affected[c]):
            print(f"AFFECT {c} {crate}")
    print(f"CHECKED {checked} changed files of locked crates")
    print("RESULT " + ("yes" if affected else "no"))


def cmd_locknames(lockfile):
    data = tomllib.load(open(lockfile, "rb"))
    seen = {}
    for p in data.get("package", []):
        if "Prometheus-Dynamics/Lemnos" in p.get("source", ""):
            seen[p["name"]] = p.get("version", "0.0.0")
    for n in sorted(seen):
        print(f"{n}\t{seen[n]}")


def find_package_manifest(tree, pkg):
    for root, dirs, files in os.walk(tree):
        dirs[:] = [d for d in dirs if d not in (".git", "target")]
        if "Cargo.toml" in files:
            try:
                t = tomllib.load(open(os.path.join(root, "Cargo.toml"), "rb"))
            except tomllib.TOMLDecodeError:
                continue
            if (t.get("package") or {}).get("name") == pkg:
                return os.path.join(root, "Cargo.toml")
    return None


def cmd_shim(tree, shims):
    done = []
    # Scratch: the tree's workspace version is 2.0.1-dev, a prerelease that the consumers' "^2.0.0" does not
    # match. Set it to 2.0.0 in the tree copy (every consumer sees the same change, base and candidate alike).
    root = os.path.join(tree, "Cargo.toml")
    rt = open(root).read()
    if re.search(r'^version = "2\.0\.1-dev"', rt, re.M):
        rt = re.sub(r'^version = "2\.0\.1-dev"', 'version = "2.0.0"', rt, count=1, flags=re.M)
        open(root, "w").write(rt)
        done.append("workspace version 2.0.1-dev -> 2.0.0")
    for line in open(shims):
        if not line.strip() or line.startswith("#"):
            continue
        pkg, feat = line.rstrip("\n").split("\t")
        mf = find_package_manifest(tree, pkg)
        if mf is None:
            continue
        t = tomllib.load(open(mf, "rb"))
        if feat in (t.get("features") or {}):
            continue
        text = open(mf).read()
        if re.search(r"^\[features\]\s*$", text, re.M):
            text = re.sub(r"^(\[features\]\s*)$", lambda m: m.group(1) + "\n" + feat + " = []", text,
                          count=1, flags=re.M)
        else:
            text = text.rstrip("\n") + f"\n\n[features]\n{feat} = []\n"
        open(mf, "w").write(text)
        done.append(f"{pkg}:{feat}")
    print("shim: " + (", ".join(done) if done else "nothing to add"))


def sync(src, dst):
    """Make dst equal src, rewriting only the files whose content differs (keeps warm mtimes)."""
    os.makedirs(dst, exist_ok=True)
    keep = set()
    for root, dirs, files in os.walk(src):
        dirs[:] = [d for d in dirs if d != ".git"]
        for f in files:
            s = os.path.join(root, f)
            rel = os.path.relpath(s, src)
            keep.add(rel)
            d = os.path.join(dst, rel)
            os.makedirs(os.path.dirname(d), exist_ok=True)
            with open(s, "rb") as a:
                sb = a.read()
            if os.path.exists(d):
                with open(d, "rb") as b:
                    if b.read() == sb:
                        continue
            with open(d, "wb") as b:
                b.write(sb)
    for root, dirs, files in os.walk(dst, topdown=False):
        for f in files:
            rel = os.path.relpath(os.path.join(root, f), dst)
            if rel not in keep:
                os.remove(os.path.join(root, f))


def requested_features(manifests):
    """name -> set of features that any listed manifest asks of a dependency of that name."""
    want = {}
    for mf in manifests:
        if not os.path.isfile(mf):
            continue
        try:
            t = tomllib.load(open(mf, "rb"))
        except tomllib.TOMLDecodeError:
            continue
        tables = [t.get("dependencies"), t.get("dev-dependencies"), t.get("build-dependencies"),
                  (t.get("workspace") or {}).get("dependencies")]
        tables += [(v or {}).get("dependencies") for v in (t.get("target") or {}).values()]
        for tab in tables:
            for key, spec in (tab or {}).items():
                if isinstance(spec, dict):
                    feats = spec.get("features") or []
                    real = spec.get("package", key)
                    want.setdefault(real, set()).update(feats)
    return want


def stub(stubs, name, version, features):
    tag = "-".join(sorted(features)) or "none"
    d = os.path.join(stubs, f"{name}-{version}-{re.sub(r'[^A-Za-z0-9_-]', '_', tag)[:80]}")
    if not os.path.exists(os.path.join(d, "Cargo.toml")):
        os.makedirs(os.path.join(d, "src"), exist_ok=True)
        feat = "".join(f'{f} = []\n' for f in sorted(features))
        with open(os.path.join(d, "Cargo.toml"), "w") as f:
            f.write(f'[package]\nname = "{name}"\nversion = "{version}"\nedition = "2021"\n'
                    f'# consumer-compile stand-in: this package is not on the Lemnos tree compiled\n'
                    f'[features]\n{feat}')
        with open(os.path.join(d, "src", "lib.rs"), "w") as f:
            f.write("// stand-in\n")
    return d


def workspace_members(tree):
    """Relative dirs of the root workspace's members (its members globs, minus excludes); None if no workspace."""
    from pathlib import PurePosixPath
    with open(os.path.join(tree, "Cargo.toml"), "rb") as f:
        ws = tomllib.load(f).get("workspace")
    if not ws:
        return None
    def hits(globs, d):
        return any(PurePosixPath(d).full_match(g) for g in globs)
    members, excludes = ws.get("members", []), ws.get("exclude", [])
    out = set()
    for root, dirs, files in os.walk(tree):
        dirs[:] = [d for d in dirs if d not in (".git", "target")]
        rel = os.path.relpath(root, tree)
        rel = "" if rel == "." else rel
        if rel and hits(members, rel) and not hits(excludes, rel):
            out.add(rel)
    return out


def drop_lemnos_lock_entries(lockpath):
    """Remove the Lemnos [[package]] blocks from a lock copy: cargo then resolves them from the [patch].
    Without this a pinned git version (2.0.0) keeps the git source and the tree's 2.0.1-dev is never used."""
    if not os.path.isfile(lockpath):
        return 0
    text = open(lockpath).read()
    blocks = re.split(r"(?m)^(?=\[\[package\]\]\n)", text)
    keep, dropped = [], 0
    for b in blocks:
        if b.startswith("[[package]]") and "Prometheus-Dynamics/Lemnos" in b:
            dropped += 1
            continue
        keep.append(b)
    if dropped:
        open(lockpath, "w").write("".join(keep))
    return dropped


def cmd_prepare(pristine, work, mdir, tree, locknames, stubs):
    sync(pristine, work)
    dropped = drop_lemnos_lock_entries(os.path.join(work, mdir, "Cargo.lock"))
    if dropped:
        print(f"prepare: {dropped} Lemnos lock entries dropped (cargo resolves them from the patch)")
    entries, dup = {}, []
    members = workspace_members(tree)
    for root, dirs, files in os.walk(tree):
        dirs[:] = [d for d in dirs if d not in (".git", "target")]
        if "Cargo.toml" not in files:
            continue
        rel = os.path.relpath(root, tree)
        if members is not None and rel not in members:
            continue
        try:
            pkg = tomllib.load(open(os.path.join(root, "Cargo.toml"), "rb")).get("package") or {}
        except tomllib.TOMLDecodeError:
            continue
        if "name" not in pkg:
            continue
        if pkg["name"] in entries:
            dup.append(pkg["name"])
        entries[pkg["name"]] = root
    if dup:
        sys.exit(f"prepare: duplicate package names in the tree: {sorted(set(dup))}")
    stubbed = []
    manifests = [os.path.join(root, "Cargo.toml") for root in entries.values()]
    manifests += [os.path.join(work, "Cargo.toml"), os.path.join(work, mdir, "Cargo.toml")]
    asked = requested_features(manifests)
    for line in open(locknames):
        name, version = line.rstrip("\n").split("\t")
        if name not in entries:
            entries[name] = stub(stubs, name, version, asked.get(name, set()))
            stubbed.append(name)
    mf = os.path.join(work, mdir, "Cargo.toml")
    text = open(mf).read()
    if f'[patch."{PATCH_URL}"]' in text:
        sys.exit("prepare: the consumer already has a [patch] for Lemnos; this step does not merge tables")
    block = [PATCH_BEGIN, f'[patch."{PATCH_URL}"]']
    block += [f"{n} = {{ path = {json.dumps(p)} }}" for n, p in sorted(entries.items())]
    block.append(PATCH_END)
    open(mf, "w").write(text.rstrip("\n") + "\n\n" + "\n".join(block) + "\n")
    print(f"prepare: {len(entries)} Lemnos packages patched ({len(entries) - len(stubbed)} from the tree, "
          f"{len(stubbed)} stand-ins: {', '.join(stubbed) or 'none'})")


def cmd_keys(log):
    out = collections.Counter()
    for line in open(log, errors="replace"):
        if not line.startswith("{"):
            continue
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        if m.get("reason") != "compiler-message":
            continue
        msg = m["message"]
        if msg.get("level") != "error":
            continue
        text = " ".join(msg["message"].split())
        if text.startswith(("aborting due to", "could not compile")):
            continue
        code = (msg.get("code") or {}).get("code") or "-"
        pm = re.search(r"#([A-Za-z0-9_\-]+)@", m.get("package_id", ""))
        pkg = pm.group(1) if pm else "?"
        spans = [s for s in msg.get("spans", []) if s.get("is_primary")] or msg.get("spans", [])
        file = spans[0]["file_name"] if spans else "-"
        out[f"{code}\t{pkg}\t{file}\t{text}"] += 1
    for k in sorted(out):
        for _ in range(out[k]):
            print(k)


def cmd_newerrs(base_f, cand_f):
    b = collections.Counter(l.rstrip("\n") for l in open(base_f) if l.strip())
    c = collections.Counter(l.rstrip("\n") for l in open(cand_f) if l.strip())
    for k in sorted((c - b).elements()):
        print(f"NEW\t{k}")
    for k in sorted((b - c).elements()):
        print(f"FIXED\t{k}")


def cmd_coverage(json_log, metadata_json):
    """Workspace members cargo checked, failed on, or never reached (skipped behind a failed dependency)."""
    meta = json.load(open(metadata_json))
    members = {}
    for pid in meta.get("workspace_members", []):
        pm = re.search(r"#([A-Za-z0-9_\-]+)@", pid)
        if pm:
            members[pm.group(1)] = True
    ok, bad = set(), set()
    for line in open(json_log, errors="replace"):
        if not line.startswith("{"):
            continue
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        pm = re.search(r"#([A-Za-z0-9_\-]+)@", m.get("package_id", ""))
        if not pm:
            continue
        if m.get("reason") == "compiler-artifact":
            ok.add(pm.group(1))
        elif m.get("reason") == "compiler-message" and m["message"].get("level") == "error":
            bad.add(pm.group(1))
    unreached = sorted(set(members) - ok - bad)
    print(f"COVERAGE checked {len(ok & set(members))}, failed {len(bad & set(members))}, "
          f"not reached {len(unreached)} of {len(members)} members")
    print(f"REACHED {len(members) - len(unreached)} {len(members)}")
    if unreached:
        print("NOT-REACHED " + " ".join(unreached))


def cmd_fix(work, fixfile):
    """Apply the scratch edits of one consumer rev. "insert" edits put text after the first "{" at or after
    line/col (applied last to first in each file); "replace" edits must match exactly once. Any miss fails."""
    edits = json.load(open(fixfile))
    miss, applied = [], 0
    inserts = collections.defaultdict(list)
    for e in edits:
        if "insert" in e:
            inserts[e["file"]].append(e)
    for f, group in inserts.items():
        p = os.path.join(work, f)
        text = open(p).read()
        lines = text.split("\n")
        offs = []
        for e in group:
            start = sum(len(l) + 1 for l in lines[:e["line"] - 1]) + e["col"] - 1
            j = text.find("{", start)
            if j < 0 or j - start > 200:
                miss.append(f"{f}:{e['line']}: no struct brace after the path ({e['why']})")
                continue
            offs.append((j + 1, e["insert"]))
        for off, ins in sorted(offs, reverse=True):
            text = text[:off] + ins + text[off:]
            applied += 1
        open(p, "w").write(text)
    skipped = 0
    for e in edits:
        if "insert" in e:
            continue
        p = os.path.join(work, e["file"])
        text = open(p).read()
        if "line" in e:
            ls = text.split("\n")
            n = ls[e["line"] - 1].count(e["old"])
            if n != 1:
                if e.get("optional"):
                    skipped += 1
                    continue
                miss.append(f"{e['file']}:{e['line']}: {n} matches on the line ({e['why']})")
                continue
            ls[e["line"] - 1] = ls[e["line"] - 1].replace(e["old"], e["new"])
            open(p, "w").write("\n".join(ls))
            applied += 1
            continue
        n = text.count(e["old"])
        if n != 1:
            if e.get("optional") and n == 0:
                skipped += 1
                continue
            miss.append(f"{e['file']}: {n} matches ({e['why']})")
            continue
        open(p, "w").write(text.replace(e["old"], e["new"]))
        applied += 1
    print(f"fix: {applied} of {len(edits)} scratch edits applied, {skipped} optional edit(s) not needed on this base")
    for m in miss:
        print("FIX-MISS " + m)
    return 1 if miss else 0


def cmd_addfeat(tree_root, errfile, stubs):
    """Resolution loop: a Lemnos package asked for a feature it lacks (cargo: "depends on `Y` with feature `F` but
    `Y` does not have that feature"). Adds F, empty, to Y's manifest in the tree copy or stand-in. Prints the count."""
    text = open(errfile, errors="replace").read()
    pairs = set(re.findall(r"depends on `([^`]+)` with feature `([^`]+)` but `\1` does not have that feature", text))
    added = []
    for name, feat in sorted(pairs):
        mf = find_package_manifest(tree_root, name)
        if mf is None:
            # a stand-in: stubs/<name>-<version>-<tag>/Cargo.toml
            cands = sorted(d for d in (os.listdir(stubs) if os.path.isdir(stubs) else []) if d.startswith(name + "-"))
            cands = [os.path.join(stubs, d, "Cargo.toml") for d in cands]
            cands = [c for c in cands if os.path.isfile(c) and tomllib.load(open(c, "rb"))["package"]["name"] == name]
            mf = cands[-1] if cands else None
        if mf is None:
            continue
        t = tomllib.load(open(mf, "rb"))
        if feat in (t.get("features") or {}):
            continue
        body = open(mf).read()
        if re.search(r"^\[features\]\s*$", body, re.M):
            body = re.sub(r"^(\[features\]\s*)$", lambda m: m.group(1) + "\n" + feat + " = []", body,
                          count=1, flags=re.M)
        else:
            body = body.rstrip("\n") + f"\n\n[features]\n{feat} = []\n"
        open(mf, "w").write(body)
        added.append(f"{name}:{feat}")
    print(f"addfeat: {', '.join(added) if added else 'nothing to add'}")
    return 0 if added else 1


def cmd_autofields(json_log, fields_tsv, out_edits):
    """E0063 "missing field" errors for fields with a scratch value: writes insert edits (applied by fix) and
    prints how many. A field without a scratch value is never filled (the error stays and is reported)."""
    vals = {}
    for line in open(fields_tsv):
        if line.strip() and not line.startswith("#"):
            f, v = line.rstrip("\n").split("\t")
            vals[f] = v
    edits, seen = [], set()
    for line in open(json_log, errors="replace"):
        if not line.startswith("{"):
            continue
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        msg = m.get("message") or {}
        if m.get("reason") != "compiler-message" or (msg.get("code") or {}).get("code") != "E0063":
            continue
        head = msg["message"].split(" in initializer")[0]
        fields = [f for f in re.findall(r"`(\w+)`", head) if f in vals]
        if not fields:
            continue
        sp = [s for s in msg.get("spans", []) if s.get("is_primary")]
        if not sp:
            continue
        s = sp[0]
        key = (s["file_name"], s["line_start"], s["column_start"])
        if key in seen:
            continue
        seen.add(key)
        ins = " " + " ".join(f"{f}: {vals[f]}," for f in fields)
        edits.append({"file": s["file_name"], "line": s["line_start"], "col": s["column_start"],
                      "insert": ins, "why": "E0063 scratch default: " + ", ".join(fields)})
    with open(out_edits, "w") as f:
        json.dump(edits, f, indent=1)
    print(len(edits))


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    cmd, a = argv[1], argv[2:]
    if cmd == "trigger" and len(a) >= 4:
        cmd_trigger(a[0], a[1], a[2], a[3:])
    elif cmd == "locknames" and len(a) == 1:
        cmd_locknames(a[0])
    elif cmd == "shim" and len(a) == 2:
        cmd_shim(a[0], a[1])
    elif cmd == "prepare" and len(a) == 6:
        cmd_prepare(*a)
    elif cmd == "keys" and len(a) == 1:
        cmd_keys(a[0])
    elif cmd == "newerrs" and len(a) == 2:
        cmd_newerrs(a[0], a[1])
    elif cmd == "coverage" and len(a) == 2:
        cmd_coverage(a[0], a[1])
    elif cmd == "addfeat" and len(a) == 3:
        sys.exit(cmd_addfeat(a[0], a[1], a[2]))
    elif cmd == "autofields" and len(a) == 3:
        cmd_autofields(a[0], a[1], a[2])
    elif cmd == "fix" and len(a) == 2:
        sys.exit(cmd_fix(a[0], a[1]))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
