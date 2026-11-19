#!/usr/bin/env python3
"""Which of a crate's tests reach code that differs between two builds of it.

    feature-tests.py <crate> <shipped.ll> <tested.ll> <tests.txt>

The two .ll files are the LLVM IR of one crate's library test binary in two feature sets (the set
it ships with, and the set the workspace tests build), each from one compile. tests.txt is the
shipped build's `--list --format terse`. Prints, one per line, every test whose code may differ:

  * a function or global is compared by its demangled name, with the crate disambiguators
    (`crate[hash]`) taken out, and its body with every symbol renamed the same way and the
    debug and metadata numbers dropped. One that is not the same in both builds, or that is in
    one build only, differs.
  * a test reaches everything its own function and closures name, and everything those name in
    turn, globals (vtables, statics, constants) included. A function called through a vtable or a
    pointer is reached through the global or function that names it.
  * a test whose code is in only one build, or that cannot be found in both, is printed: when in
    doubt, a test runs.

Code outside the crate's IR (its dependencies' non-generic code) is not compared: the caller
checks that every dependency resolves to the same features in both builds.
"""

import hashlib
import re
import subprocess
import sys

SYMBOL = re.compile(r'@("(?:[^"\\]|\\.)*"|[-a-zA-Z$._][-a-zA-Z$._0-9]*)')
METADATA = re.compile(r'(, )?!(?:[a-zA-Z_.]+ )?!\d+|![a-zA-Z_.]+ !\{[^}]*\}|#\d+|metadata !\d+')


def parse(path):
    """{symbol: [(raw text, [referenced symbols])]} for every define and global."""
    nodes = {}
    lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("define "):
            body = [line]
            while not lines[i].startswith("}"):
                i += 1
                body.append(lines[i])
            text = "\n".join(body)
            name = SYMBOL.search(line[line.index("@"):]).group(1)
            nodes.setdefault(name, []).append(text)
        elif line.startswith("@"):
            name = SYMBOL.match(line).group(1)
            nodes.setdefault(name, []).append(line)
        i += 1
    return nodes


def demangle(names):
    out = subprocess.run(["c++filt"], input="\n".join(n.strip('"') for n in names),
                         capture_output=True, text=True, check=True).stdout.split("\n")
    return {n: re.sub(r"\[[0-9a-f]+\]", "", d) for n, d in zip(names, out)}


LOCAL = re.compile(r'%("(?:[^"\\]|\\.)*"|[-a-zA-Z$._0-9]+)')


def local_names(body):
    """The body with its comments dropped and its local values and blocks renamed in the order
    they first appear: their names (an inlined callee's mangled name, say) carry the crate's and
    the impls' numbering, and a local name means nothing outside its function."""
    lines = []
    for line in body.split("\n"):
        line = line.split(";", 1)[0].rstrip() if not line.lstrip().startswith("define ") else line
        label = re.match(r'^("(?:[^"\\]|\\.)*"|[-a-zA-Z$._0-9]+):', line)
        if label:
            line = "%" + line
        if line:
            lines.append(line)
    seen = {}
    return LOCAL.sub(lambda m: "%" + seen.setdefault(m.group(1), f"v{len(seen)}"), "\n".join(lines))


def content_names(nodes, names):
    """Names for the module's unnamed constants (`alloc_<hash>`, `anon.<hash>.N`, `vtable.N`):
    their hashes and numbers depend on the crate's own hash, so each is named by what it holds,
    with the constants it points at named the same way."""
    local = {n for n, bodies in nodes.items()
             if not n.strip('"').startswith("_R") and not bodies[0].startswith("define ")}
    done, open_ = {}, []

    def name(n):
        if n in done:
            return done[n]
        if n in open_:
            # A constant that points into itself or round a cycle (a relative lookup table):
            # named by its place in the cycle, which is the same in both builds when the
            # constants are.
            return f"const.cycle{len(open_) - open_.index(n)}"
        open_.append(n)
        text = nodes[n][0].split("=", 1)[1]
        text = SYMBOL.sub(lambda m: "@" + (name(m.group(1)) if m.group(1) in local
                                            else names.get(m.group(1), m.group(1))), text)
        open_.pop()
        digest = hashlib.sha256(METADATA.sub("", text).encode()).hexdigest()[:24]
        done[n] = f"const.{digest}"
        return done[n]

    return {n: name(n) for n in local}


def normalise(nodes, names):
    """{normalised name: (sorted normalised bodies, set of normalised names referenced)}"""
    names = {**names, **content_names(nodes, names)}
    out = {}
    for name, bodies in nodes.items():
        texts, refs = [], set()
        for body in bodies:
            refs.update(names.get(s, s) for s in SYMBOL.findall(body))
            body = SYMBOL.sub(lambda m: "@" + names.get(m.group(1), m.group(1)), body)
            texts.append(local_names(METADATA.sub("", body)))
        key = names.get(name, name)
        old_texts, old_refs = out.get(key, ([], set()))
        out[key] = (sorted(old_texts + texts), old_refs | refs)
    return out


def main():
    crate, shipped_ll, tested_ll, tests_txt = sys.argv[1:5]
    shipped, tested = parse(shipped_ll), parse(tested_ll)
    every = set()
    for nodes in (shipped, tested):
        for name, bodies in nodes.items():
            every.add(name)
            for body in bodies:
                every.update(SYMBOL.findall(body))
    names = demangle(sorted(every))
    a, b = normalise(shipped, names), normalise(tested, names)
    differs = {k for k in a.keys() | b.keys() if a.get(k, (None,))[0] != b.get(k, (None,))[0]}

    root = crate.replace("-", "_")
    selected = []
    tests = [l.rsplit(": ", 1)[0] for l in open(tests_txt) if l.endswith(": test\n")]
    # The harness finds a test through its descriptor: a constant holding the test's name and the
    # function it calls. That function may be another test's when the two compiled the same and
    # were merged, so a test starts from its descriptor, not from a name like its own.
    strings = {}
    for k, (texts, _) in a.items():
        m = re.search(r'constant \[\d+ x i8\] c"([^"\\]*)"', texts[0]) if k.startswith("const.") else None
        if m:
            strings.setdefault(m.group(1), set()).add(k)
    users = {}
    for k, (_, refs) in a.items():
        for r in refs:
            users.setdefault(r, set()).add(k)
    for test in tests:
        path = f"{root}::{test}"
        # The test's own function and closures, and its descriptors.
        own = re.compile(r"(?:^|[<&( ])" + re.escape(path) + r"(?:$|::|>| )")
        starts = [k for k in a.keys() | b.keys() if own.search(k)]
        starts += [d for n in strings.get(test, ()) for d in users.get(n, ())]
        if not starts or not all(k in a and k in b for k in starts):
            selected.append(test)
            continue
        seen, todo = set(), list(starts)
        while todo:
            k = todo.pop()
            if k in seen:
                continue
            seen.add(k)
            if k in differs:
                selected.append(test)
                break
            todo.extend(a[k][1] if k in a else ())
    print(f"{len(differs)} of {len(a.keys() | b.keys())} symbols differ; "
          f"{len(selected)} of {len(tests)} tests reach one", file=sys.stderr)
    for test in selected:
        print(test)


if __name__ == "__main__":
    main()
