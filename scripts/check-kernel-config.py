#!/usr/bin/env python3
"""Keeps docs/kernel-config.md honest about lemnos.ko and its dependencies.

Plain gate (no kernel tree):
1. Every CONFIG_ symbol the module's sources, Kbuild and Kconfig refer to (`CONFIG_X`,
   `IS_ENABLED(CONFIG_X)`, `$(CONFIG_X)`) has a row in the "lemnos.ko" table.
2. Every row marked `required` is enabled (`=y`) in testing/qemu/kernel.config and in
   testing/qemu/kernel-modules-off.config; `must be unset` rows are not enabled in either. The
   modules-off configuration also says CONFIG_MODULES is not set and CONFIG_LEMNOS=y.
3. Every CONFIG_ symbol the doc names has a row in the "Dependencies" table, and each dependency
   listed for a `lemnos.ko` required symbol is enabled in both harness configurations.

Kernel tree (CI, after the harness kernel is built; the Kconfig files are in the tree):
    check-kernel-config.py --tree <linux source> --config <its .config> [--arch x86]
   For each `lemnos.ko` required symbol, follows its Kconfig dependencies through that tree and
   fails when a dependency the configuration leaves unset is not listed for it in the doc.

Regenerating the table (reference tree, read-only, from a defconfig file):
    check-kernel-config.py --print-table <linux source> <defconfig> [--arch arm64]

A dependency is one a configuration must state explicitly: a symbol set by a `default` or a
`select` is not counted, because a merged fragment does not get it. Symbols with no prompt
(set only by other symbols) are not reported, since no configuration sets them by hand.

Exits 1 and says what is wrong; 0 when all hold.
"""
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "kernel-config.md"
KERNEL = ROOT / "kernel"
HARNESS = ROOT / "testing" / "qemu" / "kernel.config"
# The configuration of the kernel with no modules (build-kernel.sh --modules-off).
MODULES_OFF = ROOT / "testing" / "qemu" / "kernel-modules-off.config"
SECTION = "## Symbols of lemnos.ko"
DEPS_SECTION = "## Dependencies"
SYMBOL = re.compile(r"(?<![A-Za-z0-9_])(CONFIG_[A-Z0-9_]+)")
ROW = re.compile(r"^\|\s*`(CONFIG_[A-Z0-9_]+)`\s*\|[^|]*\|\s*([^|]+?)\s*\|")
DEP_ROW = re.compile(r"^\|\s*`(CONFIG_[A-Z0-9_]+)`\s*\|[^|]*\|\s*([^|]*?)\s*\|\s*$")


# ---- Kconfig: symbols, their dependencies, and the closure ----------------------------------

KCONFIG_KW = re.compile(r"^\s*(config|menuconfig)\s+(\w+)")


def parse_file(path, syms):
    stack, cur, help_indent = [], None, None
    raw, buf = [], ""
    for line in open(path, errors="ignore").read().splitlines():
        if line.rstrip().endswith("\\"):
            buf += line.rstrip()[:-1] + " "
            continue
        raw.append(buf + line)
        buf = ""
    for line in raw:
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        if help_indent is not None:
            if stripped == "" or indent > help_indent:
                continue
            help_indent = None
        if not stripped or stripped.startswith("#"):
            continue
        if indent == 0 and not KCONFIG_KW.match(line):
            cur = None
        m = KCONFIG_KW.match(line)
        if m:
            cur = m.group(2)
            entry = syms.setdefault(cur, {"deps": [], "selects": [], "prompt": False})
            for _, cond in stack:
                if cond:
                    entry["deps"].append(cond)
            continue
        if re.match(r"^if\s", stripped):
            stack.append(("if", stripped[3:].strip()))
            cur = None
            continue
        if re.match(r"^menu\s", stripped):
            stack.append(("menu", None))
            cur = None
            continue
        if stripped.startswith("endif") or stripped.startswith("endmenu"):
            if stack:
                stack.pop()
            cur = None
            continue
        if stripped == "help" or stripped.startswith("---help---"):
            help_indent = indent
            continue
        if cur is None:
            continue
        if stripped.startswith("depends on "):
            syms[cur]["deps"].append(stripped[len("depends on "):].split("#")[0].strip())
        elif stripped.startswith("select "):
            syms[cur]["selects"].append(stripped.split()[1])
        elif re.match(r'^(bool|tristate|int|hex|string)\s+"', stripped):
            syms[cur]["prompt"] = True


def parse_tree(root, extra=(), arch="arm64"):
    syms, files = {}, list(extra)
    for d, dirs, fs in os.walk(root):
        rel = os.path.relpath(d, root)
        if rel == "arch":
            dirs[:] = [x for x in dirs if x == arch]
        dirs[:] = [x for x in dirs if x not in ("Documentation", "samples", "tools", "rust", ".git", "scripts")]
        files += [os.path.join(d, f) for f in fs if f == "Kconfig" or f.startswith("Kconfig.")]
    for path in files:
        parse_file(path, syms)
    return syms


TOKEN = re.compile(r"\s*(&&|\|\||!|\(|\)|[A-Za-z_][A-Za-z0-9_]*|=|!=)")


def tokens(expr):
    expr = re.sub(r"\$\([^)]*\)", " 0 ", expr)
    pos, out = 0, []
    while pos < len(expr):
        m = TOKEN.match(expr, pos)
        if not m:
            if expr[pos].isspace():
                pos += 1
                continue
            return None
        out.append(m.group(1))
        pos = m.end()
    return out


def parse_expr(toks):
    i = [0]

    def peek():
        return toks[i[0]] if i[0] < len(toks) else None

    def take():
        i[0] += 1
        return toks[i[0] - 1]

    def unary():
        t = peek()
        if t == "!":
            take()
            return ("not", unary())
        if t == "(":
            take()
            e = disj()
            take()
            return e
        if t is None or t in ("&&", "||", ")", "=", "!="):
            raise ValueError
        take()
        if peek() in ("=", "!="):
            raise ValueError  # a comparison (ARCH=..., X=y) is not a dependency on a symbol
        return ("sym", t)

    def conj():
        e = unary()
        while peek() == "&&":
            take()
            e = ("and", e, unary())
        return e

    def disj():
        e = conj()
        while peek() == "||":
            take()
            e = ("or", e, conj())
        return e

    return disj()


def evaluate(node, env):
    kind = node[0]
    if kind == "sym":
        return env.get(node[1], "n") in ("y", "m")
    if kind == "not":
        return not evaluate(node[1], env)
    if kind == "and":
        return evaluate(node[1], env) and evaluate(node[2], env)
    return evaluate(node[1], env) or evaluate(node[2], env)


def names(node, out):
    if node[0] == "sym":
        out.add(node[1])
    elif node[0] in ("and", "or"):
        names(node[1], out)
        names(node[2], out)
    elif node[0] == "not":
        pass
    return out


def conjuncts(node):
    return conjuncts(node[1]) + conjuncts(node[2]) if node[0] == "and" else [node]


def closure(syms, config, root):
    """The prompted symbols that `root` needs and that `config` leaves unset, followed through the
    dependency chain (a dependency that is set still has its own dependencies followed)."""
    missing, seen, todo = set(), set(), [root]
    while todo:
        s = todo.pop()
        if s in seen or s not in syms:
            continue
        seen.add(s)
        for expr in syms[s]["deps"]:
            toks = tokens(expr)
            if not toks:
                continue
            try:
                node = parse_expr(toks)
            except (ValueError, IndexError):
                continue
            for c in conjuncts(node):
                for t in names(c, set()):
                    if t in syms and t not in seen:
                        todo.append(t)
                # A single required symbol is reported. An alternative (||) is not: the
                # configuration may mean any of them, so its symbols are followed only.
                if c[0] == "sym" and not evaluate(c, config):
                    t = c[1]
                    if t in syms and syms[t]["prompt"] and config.get(t, "n") not in ("y", "m"):
                        missing.add(t)
    return missing


def read_config(path):
    config = {}
    for line in open(path, errors="ignore"):
        line = line.strip()
        m = re.match(r"^CONFIG_(\w+)=(.*)$", line)
        if m:
            config[m.group(1)] = m.group(2).strip('"')
        m = re.match(r"^# CONFIG_(\w+) is not set$", line)
        if m:
            config[m.group(1)] = "n"
    return config


# ---- the doc --------------------------------------------------------------------------------

def referenced():
    found = {}
    for path in sorted(KERNEL.rglob("*")):
        if not path.is_file() or path.suffix == ".md":
            continue
        if path.suffix not in (".c", ".h", "") and path.name not in ("Kbuild", "Makefile", "Kconfig"):
            continue
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            for name in SYMBOL.findall(line):
                found.setdefault(name, f"{path.relative_to(ROOT)}:{number}")
    return found


def section_text(heading):
    text = DOC.read_text()
    start = text.find(heading)
    if start < 0:
        sys.exit(f"{DOC.relative_to(ROOT)}: no section {heading!r}")
    body = text[start + len(heading):]
    end = body.find("\n## ")
    return body if end < 0 else body[:end]


def table():
    rows = {}
    for line in section_text(SECTION).splitlines():
        match = ROW.match(line)
        if match:
            rows[match.group(1)] = match.group(2).strip()
    return rows


def dependency_table():
    """symbol -> set of dependency symbols the doc lists for it."""
    rows = {}
    for line in section_text(DEPS_SECTION).splitlines():
        match = DEP_ROW.match(line)
        if match:
            rows[match.group(1)] = set(re.findall(r"CONFIG_([A-Z0-9_]+)", match.group(2)))
    return rows


def doc_symbols():
    """The symbols the doc names outside the Dependencies table, which are rows of it: the
    dependencies listed inside that table are the closure's targets, not rows of their own."""
    text = DOC.read_text()
    start = text.find(DEPS_SECTION)
    if start >= 0:
        end = text.find("\n## ", start + len(DEPS_SECTION))
        text = text[:start] + ("" if end < 0 else text[end:])
    found = set()
    for line in text.splitlines():
        found.update(SYMBOL.findall(line))
    return found


def fragment(path=HARNESS):
    values = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        m = re.match(r"^(CONFIG_[A-Z0-9_]+)=(\S+)$", line)
        if m:
            values[m.group(1)] = m.group(2)
        m = re.match(r"^# (CONFIG_[A-Z0-9_]+) is not set$", line)
        if m:
            values[m.group(1)] = "unset"
    return values


def plain_checks(problems):
    rows = table()
    for name, where in referenced().items():
        if name not in rows:
            problems.append(f"{where}: {name} is not in the lemnos.ko table of {DOC.relative_to(ROOT)}")
    deps = dependency_table()
    for name in sorted(doc_symbols()):
        if name not in deps:
            problems.append(f"{name} is named in {DOC.relative_to(ROOT)} but has no row in the Dependencies table")
    for path in (HARNESS, MODULES_OFF):
        enabled = fragment(path)
        for name, requirement in rows.items():
            if requirement == "required" and enabled.get(name) != "y":
                problems.append(f"{name} is required for lemnos.ko but {path.relative_to(ROOT)} does not set it to y")
            if requirement == "must be unset" and enabled.get(name) in ("y", "m"):
                problems.append(f"{name} must be unset for lemnos.ko but {path.relative_to(ROOT)} sets it")
            if requirement == "required" or name == "CONFIG_LEMNOS":
                for dep in sorted(deps.get(name, ())):
                    if enabled.get(f"CONFIG_{dep}") != "y":
                        problems.append(
                            f"CONFIG_{dep} is a dependency of {name} (the Dependencies table) "
                            f"but {path.relative_to(ROOT)} does not set it to y")
    if fragment(MODULES_OFF).get("CONFIG_MODULES") != "unset":
        problems.append(f"{MODULES_OFF.relative_to(ROOT)} must say # CONFIG_MODULES is not set")
    if fragment(MODULES_OFF).get("CONFIG_LEMNOS") != "y":
        problems.append(f"{MODULES_OFF.relative_to(ROOT)} must set CONFIG_LEMNOS=y")


def tree_checks(tree, config_path, arch, problems):
    """The required symbols of lemnos.ko, followed through the kernel tree's Kconfig files, against
    that tree's configuration: an unset dependency not listed for the symbol in the doc is an error."""
    extra = [ROOT / "kernel" / "Kconfig"]
    syms = parse_tree(tree, extra=extra, arch=arch)
    config = read_config(config_path)
    deps = dependency_table()
    for key, requirement in table().items():
        if requirement != "required":
            continue
        bare = key[len("CONFIG_"):]
        if bare not in syms:
            problems.append(f"{key} has no Kconfig symbol in {tree}")
            continue
        if config.get(bare, "n") not in ("y", "m"):
            if bare == "LEMNOS":
                continue  # the in-tree switch is only set for the built-in kernel
            problems.append(f"{key} is required but {config_path} leaves it unset")
            continue
        for dep in sorted(closure(syms, config, bare)):
            if dep not in deps.get(key, set()):
                problems.append(
                    f"CONFIG_{dep} is unset in {config_path} and is a dependency of {key}, "
                    f"but is not listed for it in the Dependencies table of {DOC.relative_to(ROOT)}")


def print_table(tree, defconfig, arch):
    syms = parse_tree(tree, extra=[ROOT / "kernel" / "Kconfig"], arch=arch)
    config = read_config(defconfig)
    for name in sorted(doc_symbols()):
        bare = name[len("CONFIG_"):]
        if bare not in syms:
            print(f"| `{name}` | | (not in this tree) |")
            continue
        missing = sorted(closure(syms, config, bare))
        cell = " ".join(f"`CONFIG_{m}`" for m in missing) if missing else "none"
        print(f"| `{name}` | | {cell} |")


def main(argv):
    if argv[:1] == ["--print-table"]:
        tree, defconfig = argv[1], argv[2]
        arch = argv[argv.index("--arch") + 1] if "--arch" in argv else "arm64"
        print_table(tree, defconfig, arch)
        return 0
    problems = []
    if "--tree" in argv:
        tree = argv[argv.index("--tree") + 1]
        config = argv[argv.index("--config") + 1]
        arch = argv[argv.index("--arch") + 1] if "--arch" in argv else "x86"
        tree_checks(tree, config, arch, problems)
    else:
        plain_checks(problems)
    for problem in problems:
        print(f"kernel-config: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("kernel-config: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
