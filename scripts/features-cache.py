#!/usr/bin/env python3
"""The cache of scripts/check-features.sh: one entry per crate and feature form.

    features-cache.py key <metadata.json> <crate> <dev|nodev> <entry> <text>...
        Prints the key of a result: a SHA-256 over
          * the texts given (the form, the feature sets, the toolchain, the scripts' own hashes),
          * the files that decide the build's configuration: the workspace Cargo.toml,
            Cargo.lock, .cargo/config.toml, rust-toolchain.toml, packaging/shipped.txt,
          * the environment that changes what rustc makes (RUSTFLAGS and the like, CARGO_PROFILE_*),
          * every file of every workspace package the crate can depend on (normal and build
            dependencies, and the crate's own dev-dependencies with `dev`), whatever the features:
            their manifests, sources, build scripts and the files their tests read,
          * every other file the last build read (its rustc dep-info), as recorded in <entry>.
        Registry packages are pinned by Cargo.lock. The set of packages is a superset of what any
        feature set builds, so a change that could reach the result changes the key.

    features-cache.py keys <metadata.json> <requests>
        The same for many results at once: one request per line, "<crate>\t<dev|nodev>\t<entry>"
        and the texts, tab-separated; one key per line out.

    features-cache.py record <messages.json> <metadata.json> <crate> <dev|nodev>
        Prints, one per line, the files outside the packages above that the build read: from the
        dep-info of each workspace unit in cargo's JSON messages.
"""

import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.getcwd()
CONFIG = ["Cargo.toml", "Cargo.lock", ".cargo/config.toml", "rust-toolchain.toml",
          "packaging/shipped.txt"]
ENV = re.compile(r"^(RUSTFLAGS|RUSTDOCFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_RUSTFLAGS|RUSTC|"
                 r"RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTC_BOOTSTRAP|CARGO_INCREMENTAL|"
                 r"CARGO_PROFILE_[A-Z0-9_]+|CARGO_TARGET_[A-Z0-9_]+_RUSTFLAGS)$")


def closure(meta, crate, dev):
    """The directories of the workspace packages `crate` can depend on, itself first."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    (start,) = [p["id"] for p in meta["packages"] if p["name"] == crate and p["source"] is None]
    seen, todo = [], [(start, True)]
    while todo:
        pid, root = todo.pop()
        if pid in seen:
            continue
        seen.append(pid)
        for dep in nodes[pid]["deps"]:
            kinds = {k["kind"] for k in dep["dep_kinds"]}
            if kinds & {None, "build"} or (root and dev and "dev" in kinds):
                todo.append((dep["pkg"], False))
    dirs = [os.path.dirname(packages[p]["manifest_path"]) for p in seen
            if packages[p]["source"] is None]
    return dirs


def files_of(dirs):
    rel = [os.path.relpath(d, ROOT) for d in dirs]
    out = subprocess.run(["git", "ls-files", "-z", "-co", "--exclude-standard", "--", *rel],
                         capture_output=True, check=True).stdout.decode()
    return sorted({f for f in out.split("\0") if f and os.path.isfile(f)})


def digest(h, path):
    h.update(path.encode() + b"\0")
    try:
        with open(path, "rb") as f:
            h.update(hashlib.sha256(f.read()).digest())
    except OSError:
        h.update(b"absent")


def key(meta_path, crate, dev, entry, texts, meta=None):
    meta = meta or json.load(open(meta_path))
    h = hashlib.sha256()
    for t in texts:
        h.update(t.encode() + b"\0")
    for f in CONFIG:
        digest(h, f)
    for k in sorted(os.environ):
        if ENV.match(k):
            h.update(f"{k}={os.environ[k]}\0".encode())
    for f in files_of(closure(meta, crate, dev == "dev")):
        digest(h, f)
    extra = []
    if os.path.exists(entry):
        extra = [l[6:].rstrip("\n") for l in open(entry) if l.startswith("extra ")]
    for f in sorted(extra):
        digest(h, f)
    return h.hexdigest()


def record(messages, meta_path, crate, dev):
    meta = json.load(open(meta_path))
    dirs = [d + os.sep for d in closure(meta, crate, dev == "dev")]
    workspace = {p["id"] for p in meta["packages"] if p["source"] is None}
    read = set()
    for line in open(messages):
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("reason") != "compiler-artifact" or m.get("package_id") not in workspace:
            continue
        for f in m.get("filenames", []):
            d, name = os.path.split(f)
            stem = re.sub(r"\.(rlib|rmeta|so|a|dylib)$", "", name)
            stem = stem[3:] if stem.startswith("lib") and stem != name else stem
            dep = os.path.join(d, stem + ".d")
            if not os.path.exists(dep):
                continue
            text = open(dep).read().replace("\\\n", " ")
            first = text.split("\n", 1)[0]
            for src in re.split(r"(?<!\\) ", first.split(": ", 1)[-1]):
                src = src.replace("\\ ", " ").strip()
                if not src:
                    continue
                src = os.path.normpath(os.path.join(ROOT, src))  # rustc writes them relative
                if not src.startswith(tuple(dirs)) and src.startswith(ROOT + os.sep):
                    read.add(os.path.relpath(src, ROOT))
    for f in sorted(read):
        print(f)


def main():
    if sys.argv[1] == "key":
        print(key(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6:]))
    elif sys.argv[1] == "keys":
        # keys <metadata.json> <requests>: each request line is "<crate> <dev|nodev> <entry>" and
        # its texts, tab-separated; prints one key per line, in order.
        meta = json.load(open(sys.argv[2]))
        for line in open(sys.argv[3]):
            crate, dev, entry, *texts = line.rstrip("\n").split("\t")
            print(key(None, crate, dev, entry, texts, meta))
    elif sys.argv[1] == "record":
        record(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5])
    else:
        sys.exit(f"features-cache.py: unknown command {sys.argv[1]}")


if __name__ == "__main__":
    main()
