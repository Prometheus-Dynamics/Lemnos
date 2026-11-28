#!/usr/bin/env python3
"""Which workspace packages' tests can be reused by their binaries' keys (scripts/elf-alloc-key.py):
those whose tests read nothing at run time that such a key would leave out.

A package's tests may read, and its key holds: what its test binaries load (their alloc keys), its
package's own executables (CARGO_BIN_EXE_*), every file of its own directory that is not Rust
source, the paths it declares in testing/test-inputs.toml (by content), the LEMNOS_* variables, and
the programs on PATH. A package is *reaching* (never reused: its tests always run) when any of its
Rust sources (library, tests, benches, examples, build script):

- names a path outside its own directory: `..`, or a top-level entry of the repository
  (`devices/...`, `testing/...`), unless that path lies under a declared input;
- walks up or finds the build (`.parent()`, `.ancestors()`, `current_exe` other than starting
  itself again, CARGO_TARGET_TMPDIR, CARGO_TARGET_DIR), or runs cargo;
- names a file by CARGO_MANIFEST_DIR at run time, other than a join into a declared input;
- starts a program by anything but a literal name or host path, its own binary, or itself;
- reads an environment variable by name that the key doesn't hold and that isn't one of the
  harmless ones below.

Broad on purpose: a package flagged wrongly only runs its tests every time. A path built at run
time from pieces no literal shows is not seen here; the gate's dep-info check and the plants say
where that line is.

    test-reach.py [--json]   every package: reusable, or why not (file:line)
    test-reach.py --self-test
"""

import bisect
import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
INPUTS = "testing/test-inputs.toml"

REACH = re.compile(
    r'\.parent\(\)|\.ancestors\(\)|current_exe|CARGO_TARGET_TMPDIR|CARGO_TARGET_DIR'
    r'|Command::new\(\s*"cargo"|env!\(\s*"CARGO"\s*\)')
LITERAL = re.compile(r'"((?:[^"\\]|\\.)*)"')
RAW_STRING = re.compile(r'(?:b|c)?r(#*)"')
# Starting its own binary again: the child is the same bytes the key holds.
RESPAWN = re.compile(
    r'Command::new\(\s*(?:std::)?env::current_exe\(\)\s*\.(?:expect\(\s*"(?:[^"\\]|\\.)*"\s*\)|unwrap\(\))\s*\)')
# A test that a path holds no `..`: names nothing.
DOTDOT_CHECK = re.compile(r'\.contains\(\s*"\.\."\s*\)')
# A cargo feature's name in a cfg: names no file.
FEATURE = re.compile(r'feature\s*=\s*"[^"]*"')
# Data compiled in: its bytes are in the binary, so in its key, wherever the file lies.
INCLUDE_DATA = re.compile(r'include_(?:bytes|str)!\(\s*"(?:[^"\\]|\\.)*"\s*\)')
PATH_MODULE = re.compile(r'#\s*\[\s*path\s*=\s*"((?:[^"\\]|\\.)*)"\s*\]')
RUNTIME_SOURCE = re.compile(r'CARGO_MANIFEST_DIR')
DECLARED_JOIN = re.compile(
    r'(?:Path::new\(\s*)?env!\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*\)?\s*\.join\(\s*"((?:[^"\\]|\\.)*)"\s*\)'
    r'|concat!\(\s*env!\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*"/?((?:[^"\\]|\\.)*)"\s*\)')
ENV_READ = re.compile(r'(?:env::var(?:_os)?|env!|option_env!)\(\s*(?:"([A-Za-z_0-9]+)"|([A-Z_][A-Z_0-9]*)\s*\))')
# A variable the package's own tests set for the children they start: theirs, not the runner's.
ENV_SET = re.compile(r'(?:\.env|set_var)\(\s*(?:"([A-Za-z_0-9]+)"|([A-Z_][A-Z_0-9]*)\s*,)')
ENV_CONST = re.compile(r'const ([A-Z_][A-Z_0-9]*): &str = "([^"]*)";')
# Held by the key: their values are in it.
KEYED_ENV = re.compile(r'^(LEMNOS_)')
# Set by cargo for the build (env!), or harmless where a test reads them: scratch places, the
# user, the runner's own (CI turns reuse off), and the build's compile-time facts.
HARMLESS_ENV = re.compile(
    r'^(CARGO_PKG_[A-Z_]+|CARGO_MANIFEST_DIR|CARGO_CRATE_NAME|CARGO_BIN_EXE_[A-Za-z0-9_-]+|CARGO_BIN_NAME'
    r'|CARGO_PRIMARY_PACKAGE|TARGET|HOST|PROFILE|OPT_LEVEL|DEBUG|NUM_JOBS'
    r'|TMPDIR|XDG_RUNTIME_DIR|HOME|USER|LOGNAME|RUST_BACKTRACE|RUST_LOG|NEXTEST_[A-Z_]+|CI|GITHUB_ACTIONS)$')


def line_of(text):
    """at -> 1-based line of offset `at` in TEXT."""
    starts = [m.end() for m in re.finditer("\n", text)]
    return lambda at: bisect.bisect_right(starts, at) + 1


TOKEN = re.compile(
    r'(?P<line>//[^\n]*)'
    r'|(?P<block>/\*)'
    r'|(?<![A-Za-z0-9_])(?P<raw>[bc]?r(?P<hashes>#*)")'
    r'|(?<![A-Za-z0-9_])(?P<str>[bc]?"(?:[^"\\]|\\.)*(?:"|\Z))'
    r"|(?P<char>'(?:\\[^']*|[^\\'\n])')", re.S)


def string_literals(text):
    """(line, string) of Rust source TEXT's strings as the compiler reads them: plain, byte and C
    strings, and raw strings; never a char literal or a lifetime. In a comment, every quoted run
    counts (a doctest's paths)."""
    out, i, n = [], 0, len(text)
    line = line_of(text)
    while True:
        m = TOKEN.search(text, i)
        if not m:
            return out
        at = m.start()
        if m.group("line") is not None:
            out += [(line(at), s) for s in LITERAL.findall(m.group(0))]
            i = m.end()
        elif m.group("block") is not None:
            depth, j = 1, at + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out += [(line(at), s) for s in LITERAL.findall(text[at:j])]
            i = j
        elif m.group("raw") is not None:
            close = '"' + m.group("hashes")
            stop = text.find(close, m.end())
            stop = n if stop < 0 else stop
            out.append((line(at), text[m.end():stop]))
            i = stop + len(close)
        elif m.group("str") is not None:
            body = m.group("str")
            body = body[body.index('"') + 1:]
            out.append((line(at), body[:-1] if body.endswith('"') else body))
            i = m.end()
        else:
            i = m.end()


def blank(match):
    """The match replaced by as many newlines as it held, so later line numbers hold."""
    return "\n" * match.group(0).count("\n")


def under(path, dirs):
    return any(path == d or path.startswith(d + "/") for d in dirs)


def in_home(literal, home):
    """Whether a relative LITERAL taken from HOME (where cargo runs tests and CARGO_MANIFEST_DIR
    points) stays inside HOME."""
    if os.path.isabs(literal):
        return False
    path = os.path.normpath(os.path.join(home, literal))
    return path == home or path.startswith(home + "/")


def declared(literal, home, said):
    for base in (home, ""):
        if under(os.path.normpath(os.path.join(base, literal)), said):
            return True
    return False


def reaches(literal, tops):
    if literal == ".." or "../" in literal or "/.." in literal:
        return True
    return any(literal == t or literal.startswith(t + "/") or ("/" + t + "/") in literal for t in tops)


def without_home_modules(name, text, home):
    """TEXT without each `#[path = "..."]` naming a file inside HOME: compiled in, so in the
    binary. A path leaving HOME stays and is judged as any other."""
    here = os.path.dirname(name)
    bases = (here, os.path.join(here, os.path.splitext(os.path.basename(name))[0]))

    def kept(m):
        lit = m.group(1)
        if not os.path.isabs(lit) and all(in_home(os.path.relpath(os.path.join(b, lit), home), home)
                                          or os.path.normpath(os.path.join(b, lit)).startswith(home + "/")
                                          for b in bases):
            return blank(m)
        return m.group(0)

    return PATH_MODULE.sub(kept, text)


COMMAND = re.compile(r'Command::new\(')
BIN_EXE = re.compile(r'env!\(\s*"CARGO_BIN_EXE_[A-Za-z0-9_-]+"\s*\)$')
IDENT = re.compile(r'&?([A-Z_][A-Z_0-9]*)$')


def commands(text):
    """(offset, argument) of each `Command::new(...)` in TEXT, the argument as written."""
    out = []
    for m in COMMAND.finditer(text):
        depth, j = 1, m.end()
        while j < len(text) and depth:
            depth += {"(": 1, ")": -1}.get(text[j], 0)
            j += 1
        out.append((m.start(), text[m.end():j - 1].strip()))
    return out


def program_of(arg, consts):
    """The program a `Command::new` argument names, as ("name", program) for a name looked up on
    PATH, ("own", None) for the package's own binary or a host path, or ("unknown", None)."""
    if arg.startswith('\\"'):
        return "own", None  # the call's text inside a string, not a call
    lit = LITERAL.fullmatch(arg)
    if lit is None and IDENT.match(arg) and IDENT.match(arg).group(1) in consts:
        lit_text = consts[IDENT.match(arg).group(1)]
    elif lit is not None:
        lit_text = lit.group(1)
    elif BIN_EXE.match(arg):
        return "own", None
    else:
        return "unknown", None
    if lit_text.startswith("/"):
        return "own", None
    if "/" in lit_text:
        return "unknown", None
    return "name", lit_text


def env_set(text):
    consts = dict(ENV_CONST.findall(text))
    return {m.group(1) or consts.get(m.group(2), m.group(2)) for m in ENV_SET.finditer(text)}


def programs_in(text):
    """The programs TEXT starts by name (looked up on PATH)."""
    consts = dict(ENV_CONST.findall(text))
    return {p for _, a in commands(RESPAWN.sub(blank, text)) for k, p in [program_of(a, consts)] if k == "name"}


def reasons_for(name, text, home, said, tops, own_env=frozenset()):
    """Why the Rust source NAME (repository path, content TEXT) of the package at HOME reaches
    outside what its key holds; [] when it doesn't. SAID: its declared inputs."""
    out = []
    text = without_home_modules(name, text, home)
    text = DOTDOT_CHECK.sub(blank, RESPAWN.sub(blank, text))

    lineno = line_of(text)

    for m in REACH.finditer(text):
        out.append(f"{name}:{lineno(m.start())}: {m.group(0)}")
    joined = DECLARED_JOIN.sub(
        lambda m: blank(m) if declared((m.group(1) or m.group(2)), home, said) else m.group(0), text)
    for m in RUNTIME_SOURCE.finditer(joined):
        out.append(f"{name}:{lineno(m.start())}: CARGO_MANIFEST_DIR names a file outside the declared inputs")
    for line, lit in string_literals(INCLUDE_DATA.sub(blank, FEATURE.sub(blank, text))):
        # An absolute path is the host's (/sys, /proc), never the repository's: the repository is
        # reached only by CARGO_MANIFEST_DIR, `..` or a top-level name, all judged here.
        if lit.startswith("/"):
            continue
        # A top-level name counts whether cargo's cwd or the repository root would resolve it; a
        # `..` that stays inside HOME (from cargo's cwd) does not reach.
        top_named = not lit.startswith("..") and reaches(lit.replace("..", ""), tops)
        if reaches(lit, tops) and not declared(lit, home, said) and (top_named or not in_home(lit, home)):
            out.append(f"{name}:{line}: names {lit[:60]!r}")
    consts = dict(ENV_CONST.findall(text))
    for at, arg in commands(text):
        kind, _ = program_of(arg, consts)
        if kind == "unknown":
            out.append(f"{name}:{lineno(at)}: starts a program its key can't name: Command::new({arg[:50]})")
    for m in ENV_READ.finditer(text):
        var = m.group(1) or consts.get(m.group(2), m.group(2))
        # OUT_DIR at build time (a build script) is the build's; read at run time, its files are not keyed.
        build_time = var == "OUT_DIR" and name.endswith("build.rs")
        if not (KEYED_ENV.match(var) or HARMLESS_ENV.match(var) or var in own_env or build_time):
            out.append(f"{name}:{lineno(m.start())}: reads the environment's {var}")
    return out


def inputs_digest(root, data_files, said, files):
    """The content key of what a package's tests may read besides their binaries: its own
    non-Rust files and every listed file under its declared inputs, by path and bytes."""
    h = hashlib.sha256(b"test-inputs 1\0")
    for f in sorted(set(data_files) | {f for f in files if under(f, said)}):
        full = os.path.join(root, f)
        h.update(f.encode() + b"\0")
        if os.path.islink(full):
            h.update(b"link\0" + os.readlink(full).encode())
        elif os.path.isfile(full):
            with open(full, "rb") as fh:
                h.update(hashlib.sha256(fh.read()).digest())
        else:
            h.update(b"missing")
    return h.hexdigest()


def load_inputs(root):
    path = os.path.join(root, INPUTS)
    if not os.path.exists(path):
        return {}
    with open(path, "rb") as f:
        data = tomllib.load(f)
    return {pkg: [os.path.normpath(p) for p in v.get("reads", [])]
            for pkg, v in data.get("packages", {}).items()}


def workspace(root):
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
        cwd=root, capture_output=True, text=True, check=True).stdout)
    return [(p["name"], os.path.relpath(os.path.dirname(p["manifest_path"]), root))
            for p in meta["packages"]]


def listed(root):
    return subprocess.run(["git", "ls-files"], cwd=root, capture_output=True, text=True,
                          check=True).stdout.split("\n")


def classify(root, pkgs=None):
    files = [f for f in listed(root) if f]
    tops = sorted({f.split("/")[0] for f in files if "/" in f})
    inputs = load_inputs(root)
    pkgs = workspace(root) if pkgs is None else pkgs
    homes = sorted((h for _, h in pkgs), key=len, reverse=True)
    result = {}
    for name, home in pkgs:
        said = inputs.get(name, [])
        # The package's own files: under HOME and not under a nested package.
        nested = [h for h in homes if h != home and h.startswith(home + "/")]
        why = []
        own = [f for f in files if f.startswith(home + "/") and not under(f, nested)]
        texts = {}
        for f in own:
            if f.endswith(".rs"):
                with open(os.path.join(root, f), errors="replace") as fh:
                    texts[f] = fh.read()
        own_env = set().union(*(env_set(t) for t in texts.values())) if texts else set()
        for f, t in texts.items():
            why += reasons_for(f, t, home, said, tops, own_env)
        for d in said:
            if not os.path.exists(os.path.join(root, d)):
                why.append(f"{INPUTS}: declares {d}, which does not exist")
        result[name] = {"home": home, "reasons": why,
                        "programs": sorted(set().union(*(programs_in(t) for t in texts.values()))) if texts else [],
                        "inputs": inputs_digest(root, [f for f in own if not f.endswith(".rs")], said, files)}
    return result


def self_test():
    tops = ["devices", "drivers", "testing"]
    home = "drivers/x"
    said = ["devices/boards"]
    cases = [
        ("plain test", 'fn t() { assert_eq!(1, 1); }', False),
        ("own fixture", 'let d = std::fs::read("tests/data/a.bin");', False),
        ("up the tree", 'let d = std::fs::read("../y/a.bin");', True),
        ("a top-level path", 'let d = std::fs::read("devices/other/a.toml");', True),
        ("declared top-level path", 'let d = std::fs::read("devices/boards/a.toml");', False),
        ("declared, from home", 'let d = std::fs::read("../../devices/boards/a.toml");', False),
        ("current_exe beside", 'let p = std::env::current_exe().unwrap().with_file_name("z");', True),
        ("respawn", 'let c = Command::new(std::env::current_exe().unwrap());', False),
        ("parent", 'let p = here.parent().unwrap();', True),
        ("cargo", 'Command::new("cargo").arg("build");', True),
        ("a program by name", 'let o = Command::new("dtc").arg("-v").output();', False),
        ("a program by a const name", 'const TOOL: &str = "fdtget"; let o = Command::new(TOOL).output();', False),
        ("a host program by path", 'let o = Command::new("/bin/sh").output();', False),
        ("its own binary", 'let o = Command::new(env!("CARGO_BIN_EXE_tool")).output();', False),
        ("a program from a variable", 'let o = Command::new(&program).output();', True),
        ("a program beside the test", 'let o = Command::new(me.with_file_name("other")).output();', True),
        ("a relative program path", 'let o = Command::new("tools/run").output();', True),
        ("target tmpdir", 'let t = env!("CARGO_TARGET_TMPDIR");', True),
        ("manifest dir at run time", 'let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("x.toml");', True),
        ("manifest dir join, declared", 'let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devices/boards");', False),
        ("raw string with a quote, then a path", 'let a = r#"say "hi""#; let p = "../z";', True),
        ("a path in a comment", '// see "devices/other/b.toml"\nfn t() {}', True),
        ("char quote", "let q = '\"'; let s = \"fine\";", False),
        ("dotdot check", 'if p.contains("..") { return; }', False),
        ("keyed env", 'let v = std::env::var("LEMNOS_DTMERGE");', False),
        ("harmless env", 'let v = std::env::var("TMPDIR");', False),
        ("unkeyed env", 'let v = std::env::var("DISPLAY");', True),
        ("env the package sets for its child", 'cmd.env("SHM_X_SOCKET", p); let v = std::env::var("SHM_X_SOCKET");', False),
        ("OUT_DIR read at run time", 'let p = env!("OUT_DIR");', True),
        ("env through a const", 'const V: &str = "WAYLAND_DISPLAY"; let v = std::env::var(V);', True),
        ("include of a home file", 'const D: &[u8] = include_bytes!("data/a.bin");', False),
        ("data included from outside home", 'const D: &[u8] = include_bytes!("../../testing/a.bin");', False),
        ("source included from outside home", 'include!("../../testing/z.rs");', True),
        ("path module inside home", '#[path = "more.rs"] mod more;', False),
        ("path module outside home", '#[path = "../../testing/z.rs"] mod z;', True),
        ("absolute system path", 'let s = std::fs::read("/proc/self/stat");', False),
        ("absolute path with a top-level name", 'let s = std::fs::read("/sys/kernel/debug/x");', False),
        ("a feature named like a top-level dir", '#[cfg(any(test, feature = "testing"))] fn f() {}', False),
        ("a bare top-level name", 'let p = Path::new("testing").join("a");', True),
    ]
    failures = []
    for title, text, reach in cases:
        got = reasons_for(f"{home}/tests/t.rs", text, home, said, tops, env_set(text))
        if bool(got) != reach:
            failures.append(f"{title}: {'reaching' if got else 'not reaching'} ({got}), expected "
                            f"{'reaching' if reach else 'not reaching'}")
    if programs_in('Command::new("dtc"); const T: &str = "cc"; Command::new(T); Command::new(&x);') != {"dtc", "cc"}:
        failures.append("programs_in: not exactly the named programs")
    # Line numbers hold after blanked spans.
    got = reasons_for(f"{home}/tests/t.rs", 'let c = Command::new(std::env::current_exe()\n.unwrap());\nlet p = "../z";',
                      home, said, tops)
    if not got or not got[0].endswith("names '../z'") or ":3:" not in got[0]:
        failures.append(f"line numbers after a blanked span: {got}")
    failures += repo_plants()
    for f in failures:
        print(f"test-reach self-test: FAIL {f}", file=sys.stderr)
    print(f"test-reach self-test: {'FAILED' if failures else 'passed'} ({len(cases) + 1} cases, {len(failures)} failed)")
    return 1 if failures else 0


def repo_plants():
    """A two-package repository: a package keeps its reuse until it reads outside without
    declaring; a declared input's change, and a package's own data change, move its inputs key."""
    import tempfile
    failures = []
    with tempfile.TemporaryDirectory(prefix="reach.", dir=os.environ.get("TMPDIR")) as tmp:
        def put(path, text):
            full = os.path.join(tmp, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as f:
                f.write(text)
            subprocess.run(["git", "add", "--", path], cwd=tmp, check=True)

        def run():
            return classify(tmp, [("a", "pkgs/a"), ("b", "pkgs/b")])

        subprocess.run(["git", "init", "-q"], cwd=tmp, check=True)
        put("pkgs/a/src/lib.rs", "fn f() {}\n")
        put("pkgs/a/tests/data.bin", "one")
        put("pkgs/b/tests/t.rs", 'fn t() { assert_eq!(1, 1); }\n')
        put("shared/table.txt", "x")
        r0 = run()
        if r0["a"]["reasons"] or r0["b"]["reasons"]:
            failures.append(f"plain packages reach: {r0}")
        put("pkgs/b/tests/t.rs", 'fn t() { std::fs::read("../../shared/table.txt").unwrap(); }\n')
        r1 = run()
        if not r1["b"]["reasons"] or r1["a"]["reasons"]:
            failures.append(f"an undeclared outside read: b {r1['b']['reasons']}, a {r1['a']['reasons']}")
        put(INPUTS, '[packages.b]\nreads = ["shared"]\n')
        r2 = run()
        if r2["b"]["reasons"]:
            failures.append(f"a declared outside read still reaches: {r2['b']['reasons']}")
        put("shared/table.txt", "y")
        r3 = run()
        if r3["b"]["inputs"] == r2["b"]["inputs"] or r3["a"]["inputs"] != r2["a"]["inputs"]:
            failures.append("a declared input's change: b's key held or a's moved")
        put("pkgs/a/tests/data.bin", "two")
        r4 = run()
        if r4["a"]["inputs"] == r3["a"]["inputs"] or r4["b"]["inputs"] != r3["b"]["inputs"]:
            failures.append("a package's own data change: a's key held or b's moved")
        put(INPUTS, '[packages.b]\nreads = ["gone"]\n')
        if not any("does not exist" in w for w in run()["b"]["reasons"]):
            failures.append("a declared path that does not exist is not refused")
    return failures


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    result = classify(ROOT)
    if argv == ["--json"]:
        json.dump(result, sys.stdout, indent=1, sort_keys=True)
        print()
        return 0
    reusable = sorted(n for n, r in result.items() if not r["reasons"])
    reaching = sorted(n for n, r in result.items() if r["reasons"])
    for n in reaching:
        print(f"reaching: {n} (its tests always run)")
        for why in result[n]["reasons"][:5]:
            print(f"    {why}")
        if len(result[n]["reasons"]) > 5:
            print(f"    ... {len(result[n]['reasons']) - 5} more")
    print(f"test-reach: {len(reusable)} of {len(result)} packages reusable by their binaries, "
          f"{len(reaching)} always run")
    wrong = [w for r in result.values() for w in r["reasons"] if w.startswith(INPUTS)]
    for w in wrong:
        print(f"test-reach: {w}", file=sys.stderr)
    return 1 if wrong else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
