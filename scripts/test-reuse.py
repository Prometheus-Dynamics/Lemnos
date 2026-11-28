#!/usr/bin/env python3
"""Workspace tests reused by key: a package whose tests passed in an earlier gate, with the same
key now, is not run again; it is named "reused-by-key" with that gate's commit, never as a pass.

A package's key holds: these scripts, the alloc keys (scripts/elf-alloc-key.py) of its test
binaries and of its own executables, its inputs (scripts/test-reach.py: its non-Rust files and its
declared reads, by content), the LEMNOS_* environment (but the per-run RAM directory), PATH (a per-run value in either, a temp or
runtime directory, a mktemp name or this PID, stops the plan), the
programs the workspace's tests start by name (by content), the kernel release, rustc and
cargo-nextest. A package scripts/test-reach.py finds reaching always runs. Records live in the git
common directory (lemnos-test-records/<key>), one file per passed key holding the commit.

    test-reuse.py plan LIST.json COMMIT OUT_DIR   writes OUT_DIR/filter (a nextest -E expression,
                                                  or nothing when every package runs),
                                                  OUT_DIR/run (the keys to record on a pass) and
                                                  prints each reused package
    test-reuse.py record OUT_DIR                  after a passing run: records OUT_DIR/run
    test-reuse.py --self-test

LIST.json is `cargo nextest list --workspace --message-format json`, taken from the build the run
uses. On CI (GITHUB_ACTIONS=true) or with LEMNOS_TEST_REUSE=0 nothing is reused: every package
runs, and nothing is recorded.
"""

import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
FORMAT = "test-reuse 1"
SCRIPTS = ("test-reuse.py", "test-reach.py", "elf-alloc-key.py")
PER_RUN_ENV = {"LEMNOS_TEST_RAM_DIR", "LEMNOS_TEST_REUSE"}
# mktemp's XXXXXX: six letters and digits of both cases. A run of lowercase letters alone is a
# name (".config"), not a mktemp suffix.
MKTEMP = re.compile(r"\.(?=[a-z]{0,5}[A-Z0-9])[A-Za-z0-9]{6}(?:/|$)")


class PerRunValue(Exception):
    pass


def refuse_per_run(name, value, env):
    """A value that changes from run to run (a temp or runtime directory, a mktemp name, this
    process's PID) never goes into a key: every run would miss, and a reuse that never happens
    hides as a slow pass. It stops the plan, naming the variable."""
    places = [env.get(k) for k in ("TMPDIR", "XDG_RUNTIME_DIR") if env.get(k)] + ["/tmp/", "/var/tmp/", "/dev/shm/"]
    hit = next((p for p in places if p in value), None)
    if hit is None and MKTEMP.search(value):
        hit = "a mktemp name"
    if hit is None and re.search(rf"(?<![0-9]){os.getpid()}(?![0-9])", value):
        hit = "this process's PID"
    if hit:
        raise PerRunValue(f"test reuse: {name}={value!r} holds a per-run value ({hit}); it may not "
                          "go into a test key (add it to PER_RUN_ENV if tests don't depend on it)")


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), os.path.join(HERE, name))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


reach = load("test-reach.py")
elfkey = load("elf-alloc-key.py")


def off():
    return os.environ.get("GITHUB_ACTIONS") == "true" or os.environ.get("LEMNOS_TEST_REUSE") == "0"


def file_hash(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def run_out(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError) as e:
        return f"unavailable: {e}"


def common(programs, env):
    """The part of every key the packages share."""
    h = hashlib.sha256((FORMAT + "\0").encode())
    for s in SCRIPTS:
        h.update(f"{s}\0{file_hash(os.path.join(HERE, s))}\0".encode())
    for k in sorted(env):
        if k.startswith("LEMNOS_") and k not in PER_RUN_ENV:
            refuse_per_run(k, env[k], env)
            h.update(f"env {k}={env[k]}\0".encode())
    for entry in env.get("PATH", "").split(":"):
        refuse_per_run("PATH", entry, env)
    h.update(f"PATH={env.get('PATH', '')}\0".encode())
    for p in sorted(programs):
        found = shutil.which(p, path=env.get("PATH"))
        h.update(f"program {p} {file_hash(found) if found else 'missing'}\0".encode())
    h.update(f"kernel {os.uname().release}\0".encode())
    h.update(run_out(["rustc", "-vV"]).encode() + b"\0")
    h.update(run_out(["cargo", "nextest", "--version"]).encode() + b"\0")
    return h.hexdigest()


def package_key(shared, binaries, inputs):
    """BINARIES: (id, path) of the package's test binaries and executables."""
    h = hashlib.sha256(f"{shared}\0{inputs}\0".encode())
    for ident, path in sorted(binaries):
        k = elfkey.key(path) if os.path.isfile(path) else "missing"
        h.update(f"{ident}\0{k}\0".encode())
    return h.hexdigest()


def own_executables(root):
    """package -> names of its bin targets (built at <target>/debug/<name>)."""
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
        cwd=root, capture_output=True, text=True, check=True).stdout)
    return {p["name"]: [t["name"] for t in p["targets"] if "bin" in t["kind"]] for p in meta["packages"]}


def records_dir(root):
    git = subprocess.run(["git", "rev-parse", "--git-common-dir"], cwd=root, capture_output=True,
                         text=True, check=True).stdout.strip()
    return os.path.join(root, git, "lemnos-test-records")


def plan(listing, commit, out, root=ROOT, classes=None, executables=None, env=None, records=None):
    """The packages to run and the ones reused; writes OUT/filter and OUT/run. Returns
    (reused: [(package, commit)], run: [package])."""
    env = dict(os.environ) if env is None else env
    classes = reach.classify(root) if classes is None else classes
    executables = own_executables(root) if executables is None else executables
    records = records_dir(root) if records is None else records
    target = listing["rust-build-meta"]["target-directory"]
    binaries = {}
    for suite in listing["rust-suites"].values():
        binaries.setdefault(suite["package-name"], []).append((suite["binary-id"], suite["binary-path"]))
    for pkg, names in executables.items():
        for n in names:
            binaries.setdefault(pkg, []).append((f"exe {n}", os.path.join(target, "debug", n)))
    programs = set().union(*(set(c.get("programs", [])) for c in classes.values())) if classes else set()
    shared = common(programs, env)
    reused, ran, keys = [], [], {}
    for pkg in sorted(set(binaries) | set(classes)):
        if not any(suite["package-name"] == pkg for suite in listing["rust-suites"].values()):
            continue  # no tests: nothing runs, nothing is reused
        c = classes.get(pkg)
        if c is None or c["reasons"] or off():
            ran.append(pkg)
            continue
        key = package_key(shared, binaries.get(pkg, []), c["inputs"])
        rec = os.path.join(records, key)
        if os.path.isfile(rec):
            with open(rec) as f:
                reused.append((pkg, f.read().strip()))
        else:
            ran.append(pkg)
            keys[pkg] = key
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, "filter"), "w") as f:
        if reused:
            f.write("not (" + " | ".join(f"package(={p})" for p, _ in reused) + ")\n")
    with open(os.path.join(out, "run"), "w") as f:
        if not off():
            f.write(f"{records}\n{commit}\n")
            for pkg, key in sorted(keys.items()):
                f.write(f"{pkg} {key}\n")
    return reused, ran


def record(out):
    with open(os.path.join(out, "run")) as f:
        lines = f.read().splitlines()
    if len(lines) < 2:
        return 0
    records, commit = lines[0], lines[1]
    os.makedirs(records, exist_ok=True)
    for line in lines[2:]:
        _, key = line.split()
        tmp = os.path.join(records, f".{key}.{os.getpid()}")
        with open(tmp, "w") as f:
            f.write(commit + "\n")
        os.replace(tmp, os.path.join(records, key))
    return len(lines) - 2


def self_test():
    """Plants on a planted repository and real executables: a fresh key runs; a recorded key is
    reused with its commit; a changed binary runs; a declared input's change reruns; a reaching
    package always runs; CI reuses nothing and records nothing."""
    failures = []
    exe = shutil.which("true") or "/usr/bin/true"
    with tempfile.TemporaryDirectory(prefix="reuse.", dir=os.environ.get("TMPDIR")) as tmp:
        repo = os.path.join(tmp, "repo")

        def put(path, text):
            full = os.path.join(repo, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as f:
                f.write(text)
            subprocess.run(["git", "add", "--", path], cwd=repo, check=True)

        os.makedirs(repo)
        subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
        put("pkgs/a/src/lib.rs", "fn f() {}\n")
        put("pkgs/b/tests/t.rs", 'fn t() { std::fs::read("../../shared/table.txt").unwrap(); }\n')
        put("pkgs/c/tests/t.rs", 'fn t() { std::fs::read("../../shared/other.txt").unwrap(); }\n')
        put("shared/table.txt", "x")
        put("shared/other.txt", "o")
        put(reach.INPUTS, '[packages.b]\nreads = ["shared/table.txt"]\n')
        pkgs = [("a", "pkgs/a"), ("b", "pkgs/b"), ("c", "pkgs/c")]
        target = os.path.join(tmp, "target")
        os.makedirs(os.path.join(target, "debug", "deps"))
        bins = {}
        for p in "abc":
            bins[p] = os.path.join(target, "debug", "deps", f"{p}-test")
            shutil.copyfile(exe, bins[p])
        listing = {"rust-build-meta": {"target-directory": target},
                   "rust-suites": {p: {"package-name": p, "binary-id": p, "binary-path": bins[p]} for p in "abc"}}
        records = os.path.join(tmp, "records")
        # A fixed PATH: the plants never depend on the PATH of whoever runs them.
        env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LEMNOS_TEST_RAM_DIR": "/somewhere"}
        out = os.path.join(tmp, "out")

        def go(commit, env=env):
            classes = reach.classify(repo, pkgs)
            return plan(listing, commit, out, repo, classes, {"a": []}, env, records)

        def check(title, got, reused, ran):
            r, n = got
            if sorted(p for p, _ in r) != sorted(reused) or sorted(n) != sorted(ran):
                failures.append(f"{title}: reused {r}, ran {n}; expected reused {reused}, ran {ran}")

        check("fresh keys", go("c1"), [], ["a", "b", "c"])
        record(out)
        got = go("c2")
        check("recorded keys", got, ["a", "b"], ["c"])
        if dict(got[0]).get("a") != "c1":
            failures.append(f"a reused package names the commit that passed it: {got[0]}")
        with open(os.path.join(out, "filter")) as f:
            if f.read().strip() != "not (package(=a) | package(=b))":
                failures.append("the filter does not leave out exactly the reused packages")
        check("a per-run RAM directory", go("c2", dict(env, LEMNOS_TEST_RAM_DIR="/elsewhere")), ["a", "b"], ["c"])
        check("a keyed variable", go("c2", dict(env, LEMNOS_SOMETHING="1")), [], ["a", "b", "c"])
        # A changed binary: one loaded byte of a's test binary.
        data = bytearray(open(bins["a"], "rb").read())
        _, _, secs = elfkey.sections(bytes(data))
        sec = next(s for s in secs if s[0] == ".text")
        data[bytes(data).find(sec[5])] ^= 1
        with open(bins["a"], "wb") as f:
            f.write(data)
        check("a changed binary", go("c3"), ["b"], ["a", "c"])
        record(out)
        put("shared/table.txt", "y")
        check("a declared input's change", go("c4"), ["a"], ["b", "c"])
        record(out)
        check("all recorded again", go("c5"), ["a", "b"], ["c"])
        for title, extra in [("a temp directory", {"LEMNOS_X": "/tmp/abc"}),
                             ("the runtime directory", {"XDG_RUNTIME_DIR": "/run/user/7", "LEMNOS_X": "/run/user/7/x"}),
                             ("a mktemp name", {"LEMNOS_X": "/srv/lemnos-gate-ram.Ab12Cd"}),
                             ("this PID", {"LEMNOS_X": f"run-{os.getpid()}"}),
                             ("a temp directory on PATH", {"PATH": "/usr/bin:/tmp/bin"})]:
            try:
                go("c5", dict(env, **extra))
                failures.append(f"a per-run value ({title}) went into a key")
            except PerRunValue:
                pass
        check("a fixed keyed value", go("c5", dict(env, LEMNOS_Y="/opt/fixed")), [], ["a", "b", "c"])
        # The shape of an entry on CI's runner PATH: a dot-directory, not a mktemp name (the red of 8309fd90).
        for fixed in ["/srv/runner/.config/composer/vendor/bin", "/srv/u/.cargo/bin", "/opt/x/.local/bin"]:
            try:
                go("c5", dict(env, PATH=f"/usr/bin:{fixed}"))
            except PerRunValue as e:
                failures.append(f"a fixed PATH entry refused: {e}")
        os.environ["GITHUB_ACTIONS"] = "true"
        try:
            check("on CI", go("c6"), [], ["a", "b", "c"])
            with open(os.path.join(out, "run")) as f:
                if f.read():
                    failures.append("on CI the run still lists keys to record")
        finally:
            del os.environ["GITHUB_ACTIONS"]
    for f in failures:
        print(f"test-reuse self-test: FAIL {f}", file=sys.stderr)
    print(f"test-reuse self-test: {'FAILED' if failures else 'passed'} ({len(failures)} failed)")
    return 1 if failures else 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if len(argv) == 4 and argv[0] == "plan":
        with open(argv[1]) as f:
            listing = json.load(f)
        try:
            reused, ran = plan(listing, argv[2], argv[3])
        except PerRunValue as e:
            print(e, file=sys.stderr)
            return 1
        for pkg, commit in reused:
            print(f"reused-by-key {pkg} (passed at {commit})")
        print(f"test reuse: {len(ran)} packages run, {len(reused)} reused-by-key"
              + (" (reuse is off)" if off() else ""))
        return 0
    if len(argv) == 2 and argv[0] == "record":
        print(f"test reuse: recorded {record(argv[1])} passed keys")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
