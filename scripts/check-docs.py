#!/usr/bin/env python3
"""Checks the documents: every command in them runs, every link leads somewhere.

The documents are every Markdown file in the repository except the board-run
records under testing/board-runs/, whose commands need a board by definition.

Commands. A fenced block is a command block when its first word is `sh`:

    ```sh                 run here, against the simulated board
    ```sh slow            run only with --slow (a release build, a kernel build, QEMU)
    ```sh needs:REASON    never run here: it needs a board, root or a kernel tree

All the blocks of one document run in order in one shell, from the repository
root, so a block can use what an earlier one exported or started. The shell
stops at the first command that fails. For each document the shell gets:

    PATH             with the debug build's binaries (lemnosd, lemnos-ctl) in front
    TMPDIR           an empty directory, removed afterwards
    LEMNOSD_SOCKET   a socket path in it, which lemnosd and lemnos-ctl both read

After each block, if the block left something running in the background, the
check waits until the service's socket is there, so the next block can talk
to a lemnosd the block started. Whatever a
document left running is stopped when the document ends.

Files. A block whose info string has `file=PATH` must be that file, byte for
byte, so an example shown in a document is the example that is checked.

Links. Every relative link must name a file or directory that exists, and a
`#fragment` on a Markdown file must be one of its headings.

Drivers. docs/drivers.md must say what the code is: its driver tables name
exactly the `#[driver]` types lemnosd builds in, each with the contracts it
provides; a crate in its library table has no driver lemnosd builds in; and no
crate whose driver is built in says in its crate documentation that it is not.

    scripts/check-docs.py            the fast commands, files and links
    scripts/check-docs.py --slow     the slow commands too
    scripts/check-docs.py --list     list every command block and what it is, run nothing
    scripts/check-docs.py [--slow] FILE...   run only these documents' commands
"""
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
SKIP_DIRS = {"target", ".git", "target-docker", ".cargo-docker", "node_modules"}
NO_COMMANDS = ("testing/board-runs/",)
# Planted violations for the boundary check; not documents.
NOT_DOCUMENTS = ("testing/boundary/",)

FENCE = re.compile(r"^(\s*)(`{3,}|~{3,})(.*)$")
LINK = re.compile(r"(?<!\!)\[[^\]\n]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")


def documents():
    found = []
    for directory, names, files in os.walk(ROOT):
        names[:] = sorted(n for n in names if n not in SKIP_DIRS)
        for name in sorted(files):
            if name.endswith(".md"):
                path = pathlib.Path(directory, name).relative_to(ROOT).as_posix()
                if not path.startswith(NOT_DOCUMENTS):
                    found.append(path)
    return found


def blocks(text):
    """(first line number, info string, body) for every fenced block, and the text outside them."""
    out, prose, inside = [], [], None
    for number, line in enumerate(text.splitlines(), 1):
        match = FENCE.match(line)
        if inside is None:
            if match:
                inside = (match.group(2), number, match.group(3).strip(), [], len(match.group(1)))
            else:
                prose.append((number, line))
        elif match and match.group(2).startswith(inside[0]) and not match.group(3).strip():
            out.append((inside[1], inside[2], "\n".join(inside[3]) + "\n"))
            inside = None
        else:
            inside[3].append(line[inside[4]:] if line[: inside[4]].isspace() else line)
    return out, prose


def slug(heading):
    heading = re.sub(r"`([^`]*)`", r"\1", heading)
    heading = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    heading = heading.strip().lower()
    heading = re.sub(r"[^\w\- ]", "", heading)
    return heading.replace(" ", "-")


def anchors(path):
    _, prose = blocks((ROOT / path).read_text())
    found = set()
    for _, line in prose:
        match = HEADING.match(line)
        if match:
            found.add(slug(match.group(2)))
    return found


def check_links(path, prose, problems):
    here = (ROOT / path).parent
    for number, line in prose:
        line = re.sub(r"`[^`]*`", "", line)
        for target in LINK.findall(line):
            if re.match(r"^[a-z][a-z0-9+.-]*:", target):
                continue
            file, _, fragment = target.partition("#")
            where = (here / file).resolve() if file else (ROOT / path)
            if not where.exists():
                problems.append(f"{path}:{number}: link to {target}: no such file")
                continue
            if fragment and where.suffix == ".md":
                if fragment not in anchors(where.relative_to(ROOT).as_posix()):
                    problems.append(f"{path}:{number}: link to {target}: no such heading")


def classify(info):
    """None for a block that is no command; else ('run'|'slow'|'needs', reason)."""
    words = info.split()
    if not words or words[0] != "sh":
        return None
    for word in words[1:]:
        if word == "slow":
            return ("slow", "")
        if word.startswith("needs:"):
            return ("needs", word[len("needs:"):].replace("-", " "))
    return ("run", "")


SETTLE = r"""
__settle() {
  # A service a block started must be listening before the next block talks to it.
  local tries=0
  while [ ! -S "$LEMNOSD_SOCKET" ] && [ -n "$(jobs -pr)" ] && [ $tries -lt 300 ]; do
    command sleep 0.1; tries=$((tries + 1))
  done
}
__finish() {
  local pids; pids=$(jobs -p)
  [ -n "$pids" ] && kill -TERM $pids 2>/dev/null
  wait 2>/dev/null
  return 0
}
trap __finish EXIT
"""


def run_document(path, commands, target_bin):
    """Runs a document's blocks in one shell. Returns (ok, output)."""
    scratch = tempfile.mkdtemp(prefix="lemnos-docs-")
    try:
        script = ["set -euo pipefail", SETTLE]
        for number, body in commands:
            script.append(f"echo '--- {path}:{number}'")
            lines = [l for l in body.splitlines() if l.strip() and not l.lstrip().startswith("#")]
            if all(whole_command(line) for line in lines):
                # Each line is a command by itself: show it, then run it.
                for line in lines:
                    script.append("echo " + shell_quote("$ " + line))
                    script.append(line)
            else:
                for line in lines:
                    script.append("echo " + shell_quote("$ " + line))
                script.append(body)
            script.append("__settle")
        env = dict(os.environ)
        env["PATH"] = f"{target_bin}{os.pathsep}{env.get('PATH', '')}"
        env["TMPDIR"] = scratch
        env["LEMNOSD_SOCKET"] = os.path.join(scratch, "lemnosd.sock")
        result = subprocess.run(
            ["bash", "-c", "\n".join(script)],
            cwd=ROOT,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
        return result.returncode == 0, result.stdout
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def whole_command(line):
    """Whether one line of a block parses as a command without the lines around it."""
    if line.rstrip().endswith("\\"):
        return False
    return subprocess.run(["bash", "-n", "-c", line], stderr=subprocess.DEVNULL).returncode == 0


def shell_quote(text):
    return "'" + text.replace("'", "'\\''") + "'"


def first_line(body):
    for line in body.splitlines():
        if line.strip() and not line.lstrip().startswith("#"):
            return line.strip()
    return ""


DRIVER = re.compile(r'#\[driver\("([^"]+)", provides = \[([^\]]*)\]\)\]\s*(?:#\[[^\]]*\]\s*)*pub struct (\w+)')
CONTRACT = re.compile(r'#\[contract\("([^"]+)", version = (\d+)\)\]\s*(?:#\[[^\]]*\]\s*)*pub trait (\w+)')
NOT_BUILT_IN = re.compile(r"not registered in `lemnosd`|not built into (the daemon|`lemnosd`)|nothing is registered in `lemnosd`")


def sources(crate_dir):
    return [p for p in sorted((crate_dir / "src").rglob("*.rs")) if "tests" not in p.parts and p.name != "tests.rs"]


def check_drivers(problems):
    """docs/drivers.md against the driver crates and lemnosd's built-in list."""
    import tomllib

    tags = {}
    for path in sorted(ROOT.glob("contracts/*/src/**/*.rs")) + sorted(ROOT.glob("drivers/*/src/**/*.rs")):
        for m in CONTRACT.finditer(path.read_text()):
            tags[m.group(3)] = f"{m.group(1)}/{m.group(2)}"
    builtin = (ROOT / "service/lemnosd/src/builtin.rs").read_text()
    built_in = {}  # driver name -> set of contract tags
    built_in_dirs = set()
    crates = {}
    for manifest in sorted(ROOT.glob("drivers/*/Cargo.toml")):
        package = tomllib.loads(manifest.read_text())["package"]
        crates[manifest.parent.name] = package
        crate = package["name"].replace("-", "_")
        types, renamed = {}, {}
        for path in sources(manifest.parent):
            code = path.read_text()
            renamed.update((m.group(2), m.group(1)) for m in re.finditer(r"\b(\w+) as (\w+)\b", code))
            for m in DRIVER.finditer(code):
                types[m.group(3)] = (m.group(1), m.group(2))
        for kind, (name, provides) in types.items():
            if f"entry::<{crate}::{kind}>()" in builtin:
                traits = [t.strip() for t in provides.split(",") if t.strip()]
                built_in[name] = {tags.get(renamed.get(t, t), t) for t in traits}
                built_in_dirs.add(manifest.parent.name)
    text = (ROOT / "docs/drivers.md").read_text()
    library = text.split("## Library crates", 1)
    if len(library) != 2:
        problems.append("docs/drivers.md: no `## Library crates` section")
        return
    rows = {}
    for line in library[0].splitlines():
        m = re.match(r"\| `([^`]+)`(?: \(crate `[^`]+`\))? \| ([^|]*) \|", line)
        if m:
            rows[m.group(1)] = set(re.findall(r"`([a-z0-9.-]+/\d+)`", m.group(2)))
    for name in sorted(set(rows) - set(built_in)):
        problems.append(f"docs/drivers.md: `{name}` is in a driver table but lemnosd builds in no such driver")
    for name in sorted(set(built_in) - set(rows)):
        problems.append(f"docs/drivers.md: lemnosd builds in `{name}`, and no driver table lists it")
    for name in sorted(set(rows) & set(built_in)):
        if rows[name] != built_in[name]:
            problems.append(f"docs/drivers.md: `{name}` is listed as providing {sorted(rows[name])}, and provides {sorted(built_in[name])}")
    table = library[1].split("\n## ", 1)[0]
    for m in re.finditer(r"^\| `([^`]+)` \|", table, re.M):
        name = m.group(1)
        if name not in crates:
            problems.append(f"docs/drivers.md: library crate `{name}` is not under drivers/")
        elif name in built_in_dirs:
            problems.append(f"docs/drivers.md: library crate `{name}` has a driver lemnosd builds in")
    for name in sorted(built_in_dirs):
        lib = ROOT / "drivers" / name / "src/lib.rs"
        found = NOT_BUILT_IN.search(lib.read_text()) if lib.is_file() else None
        if found:
            problems.append(f"drivers/{name}/src/lib.rs: says \"{found.group(0)}\", and lemnosd builds it in")


def check_board_runs(problems):
    """docs/hardware-status.md lists every written board run, and counts them right."""
    path = "docs/hardware-status.md"
    text = (ROOT / path).read_text()
    runs = sorted(p.name for p in (ROOT / "testing/board-runs").glob("*.md"))
    for name in runs:
        if f"board-runs/{name})" not in text:
            problems.append(f"{path}: testing/board-runs/{name} is not listed")
    stated = re.search(r"(?:None|Of) those (\d+) files", text)
    if not stated:
        problems.append(f"{path}: no \"Of those N files\" count of the board runs")
    elif int(stated.group(1)) != len(runs):
        problems.append(f"{path}: says {stated.group(1)} board runs, and there are {len(runs)}")


def main():
    slow = "--slow" in sys.argv
    list_only = "--list" in sys.argv
    only = [a for a in sys.argv[1:] if not a.startswith("--")]
    unknown = [a for a in only if a not in documents()]
    if unknown:
        sys.exit(f"docs: not a document: {' '.join(unknown)}")
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target.is_absolute():
        target = ROOT / target
    target_bin = target / "debug"

    problems, not_run, ran, skipped_slow = [], [], 0, []
    check_drivers(problems)
    check_board_runs(problems)
    signal.signal(signal.SIGINT, signal.default_int_handler)
    if not list_only:
        # The programs the documents run, built as the README builds them.
        build = ["cargo", "build", "-q", "-p", "lemnosd", "-p", "lemnos-ctl",
                 "--features", "lemnosd/fake-devices"]
        print("$ " + " ".join(build), flush=True)
        if subprocess.run(build, cwd=ROOT).returncode != 0:
            sys.exit("docs: the programs the documents run did not build")
    for path in documents():
        text = (ROOT / path).read_text()
        fenced, prose = blocks(text)
        check_links(path, prose, problems)
        commands = []
        for number, info, body in fenced:
            for word in info.split():
                if word.startswith("file="):
                    wanted = ROOT / word[len("file="):]
                    if not wanted.is_file():
                        problems.append(f"{path}:{number}: {word}: no such file")
                    elif wanted.read_text() != body:
                        problems.append(f"{path}:{number}: the block is not {word[5:]} as it stands")
            kind = classify(info)
            if kind is None or path.startswith(NO_COMMANDS) or (only and path not in only):
                continue
            if kind[0] == "needs":
                not_run.append((path, number, kind[1], first_line(body)))
            elif kind[0] == "slow" and not slow:
                skipped_slow.append((path, number, first_line(body)))
            else:
                commands.append((number, body))
        if list_only:
            for number, body in commands:
                print(f"run    {path}:{number}: {first_line(body)}")
            continue
        if not commands:
            continue
        ok, output = run_document(path, commands, target_bin)
        ran += len(commands)
        sys.stdout.write(output)
        sys.stdout.flush()
        if not ok:
            problems.append(f"{path}: a command failed (the output above ends at it)")

    print()
    for path, number, line in skipped_slow:
        print(f"slow, run with --slow: {path}:{number}: {line}")
    for path, number, reason, line in not_run:
        print(f"not run, needs {reason}: {path}:{number}: {line}")
    if problems:
        print()
        for problem in problems:
            print(f"docs: {problem}", file=sys.stderr)
        sys.exit(1)
    if not list_only:
        print(f"docs hold: {ran} command blocks ran, {len(skipped_slow)} slow ones left out, "
              f"{len(not_run)} need hardware, root or a kernel tree; files and links checked")


main()
