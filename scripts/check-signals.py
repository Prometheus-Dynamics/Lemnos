#!/usr/bin/env python3
"""Refuses a script that signals a process it did not record.

A signal to pid -1 reaches every process of the user, and one to pid 0 or to a negative pid
reaches a whole process group, which may be the caller's own or one it did not make. Killing by
name (pkill, killall, `fuser -k`, a pgrep fed to kill) reaches whatever matches. The rule is that a
script signals only the pids it recorded (its own `$!`, or a pid file it wrote), so this check fails
on any script in the repository that:

  - runs pkill or killall;
  - runs fuser with -k, or xargs with kill in it;
  - runs pgrep on a line that also runs kill;
  - runs kill with a pid operand that is 0, or starts with `-` (a group or -1), unless the same
    physical line calls one of GUARDS, which tests the group first (is_group: a group above 1
    that is not this shell's own; group_is_check: the group the rehearsal recorded as the check's);
  - calls os.kill or os.killpg with a first argument of 0 or a negative value (Python), unless the
    line calls is_group.

A shell script that calls a guard must define it (`name() {`) or source a file that does.
Comments, quoted text and `kill -0 PID` (a probe of a recorded pid) are not signals and pass.

Usage: check-signals.py [PATH...]. With no path it checks every tracked file of the repository
(`git ls-files`). A directory path is walked. Only shell and Python scripts are read: a file is a
script when its name ends in .sh, .bash or .py, or its first line is a shell or Python shebang.
This checker and its test (testing/signals/) are exempt: they hold the words they look for.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GUARDS = ("is_group", "group_is_check")
GUARD_RE = re.compile(r"\b(?:" + "|".join(GUARDS) + r")\b")
DEFINES_RE = re.compile(r"^\s*(?:function\s+)?(?:%s)\s*(?:\(\))?\s*\{" % "|".join(GUARDS), re.M)
SOURCES_RE = re.compile(r"^\s*(?:\.|source)\s", re.M)
SHEBANG_SH = re.compile(r"^#!.*\b(?:bash|sh|dash|zsh)\b")
SHEBANG_PY = re.compile(r"^#!.*\bpython3?\b")
ASSIGN_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
PY_KILL_RE = re.compile(r"\bos\.(?:killpg|kill)\(\s*(?:-|0\b)")
PY_BYNAME_RE = re.compile(r"""["'](?:pkill|killall)["']""")
EXEMPT = ("scripts/check-signals.py", "testing/signals/test_check_signals.py")

# Words that run the command after them, so the command word is the next one.
WRAPPERS = {
    "!", "then", "do", "else", "elif", "if", "while", "until", "time", "exec", "command",
    "builtin", "sudo", "nice", "nohup", "setsid", "env", "stdbuf", "unbuffer", "ionice",
    "chrt", "flock", "timeout", "{", "}",
}
KILLERS = {"kill", "pkill", "killall"}
SEPARATORS = set(";|&()`\n")


def segments(line):
    """The simple commands of one shell line: lists of words, quotes removed, quoted text kept.

    A comment (a `#` that starts a word) ends the line. Operators split commands. This reads
    enough shell to find command words and their operands; it does not expand anything.
    """
    segs, words, cur, have = [], [], [], False
    quote, i, n = None, 0, len(line)

    def flush():
        nonlocal have
        if have:
            words.append("".join(cur))
        cur.clear()
        have = False

    def end():
        flush()
        if words:
            segs.append(list(words))
            words.clear()

    while i < n:
        c = line[i]
        if quote == "'":
            if c == "'":
                quote = None
            else:
                cur.append(c)
            i += 1
            continue
        if quote == '"':
            if c == '"':
                quote = None
            elif c == "\\" and i + 1 < n and line[i + 1] in '"\\$`':
                cur.append(line[i + 1])
                i += 2
                continue
            else:
                cur.append(c)
            i += 1
            continue
        if c == "\\" and i + 1 < n:
            cur.append(line[i + 1])
            have = True
            i += 2
            continue
        if c in "\"'":
            quote, have = c, True
            i += 1
            continue
        if c == "#" and not have:
            break
        if c in " \t":
            flush()
            i += 1
            continue
        if c == "$" and i + 1 < n and line[i + 1] == "(":
            end()
            i += 2
            continue
        if c == "$" and i + 1 < n and line[i + 1] == "{":
            j = line.find("}", i + 2)
            j = n - 1 if j < 0 else j
            cur.append(line[i : j + 1])
            have = True
            i = j + 1
            continue
        if c in SEPARATORS or (c in "{}" and not have):
            end()
            i += 1
            continue
        cur.append(c)
        have = True
        i += 1
    end()
    return segs


def kill_operands(args):
    """The pid operands of one kill's arguments: a signal option, then pids (`--` ends options)."""
    pids, after_dd, sig, i = [], False, False, 0
    while i < len(args):
        a = args[i]
        if not after_dd:
            if a == "--":
                after_dd = True
                i += 1
                continue
            if a in ("-l", "-L"):
                return pids
            if a in ("-s", "-n") and not sig:
                sig = True
                i += 2
                continue
            if not sig and a.startswith("-") and len(a) > 1:
                sig = True
                i += 1
                continue
        pids.append(a)
        i += 1
    return pids


def shell_findings(text):
    """(line number, message) for each refused signal in a shell script."""
    found = []
    lines = text.splitlines()
    for lineno, line in enumerate(lines, 1):
        guarded = bool(GUARD_RE.search(line))
        commands = []
        for seg in segments(line):
            words = list(seg)
            while words and (words[0] in WRAPPERS or ASSIGN_RE.match(words[0])):
                words.pop(0)
            if not words:
                continue
            commands.append(os.path.basename(words[0]))
        if "pgrep" in commands and ("kill" in commands or "xargs" in commands):
            found.append((lineno, "pgrep feeds a kill: the pids are found by name, not recorded"))
        for seg in segments(line):
            words = list(seg)
            while words and (words[0] in WRAPPERS or ASSIGN_RE.match(words[0])):
                words.pop(0)
            if not words:
                continue
            base = os.path.basename(words[0])
            if base in ("pkill", "killall"):
                found.append((lineno, f"{base} reaches processes by name"))
            elif base == "fuser" and any(
                w.startswith("-") and not w.startswith("--") and "k" in w[1:] for w in words[1:]
            ):
                found.append((lineno, "fuser -k kills whatever uses the file"))
            elif base == "xargs" and any(os.path.basename(w) in KILLERS for w in words[1:]):
                found.append((lineno, "xargs feeds kill: the pids are not recorded"))
            elif base == "kill":
                for pid in kill_operands(words[1:]):
                    if pid in ("0", "-0") or pid.startswith("-"):
                        if not guarded:
                            found.append(
                                (
                                    lineno,
                                    f"kill of pid {pid}: a group or every process; the line "
                                    f"needs {GUARDS[0]} or {GUARDS[1]} in front of it",
                                )
                            )
    if GUARD_RE.search(text) and not (DEFINES_RE.search(text) or SOURCES_RE.search(text)):
        for lineno, line in enumerate(lines, 1):
            if GUARD_RE.search(line):
                found.append((lineno, "the guard is called here but defined nowhere in this file"))
                break
    return found


def python_findings(text):
    found = []
    for lineno, line in enumerate(text.splitlines(), 1):
        if PY_KILL_RE.search(line) and "is_group" not in line:
            found.append((lineno, "os.kill or os.killpg of 0 or a negative pid (is_group first)"))
        if PY_BYNAME_RE.search(line):
            found.append((lineno, "pkill or killall reaches processes by name"))
    return found


def script_kind(rel, text):
    if rel.endswith((".sh", ".bash")):
        return "sh"
    if rel.endswith(".py"):
        return "py"
    first = text.split("\n", 1)[0]
    if SHEBANG_SH.match(first):
        return "sh"
    if SHEBANG_PY.match(first):
        return "py"
    return None


def check_text(rel, text):
    """The findings for one file's text, as (path, line, message, source line) tuples."""
    kind = script_kind(rel, text)
    if kind is None or rel in EXEMPT:
        return []
    found = shell_findings(text) if kind == "sh" else python_findings(text)
    src = text.splitlines()
    return [(rel, n, msg, src[n - 1].strip()) for n, msg in found]


def repo_files():
    out = subprocess.run(
        ["git", "-C", ROOT, "ls-files", "-z"], check=True, capture_output=True
    ).stdout
    return [os.path.join(ROOT, p) for p in out.decode().split("\0") if p]


def expand(paths):
    files = []
    for p in paths:
        if os.path.isdir(p):
            for d, dirs, names in os.walk(p):
                dirs[:] = [x for x in dirs if x != ".git"]
                files += [os.path.join(d, x) for x in names]
        else:
            files.append(p)
    return files


def rel_of(path):
    full = os.path.abspath(path)
    return os.path.relpath(full, ROOT) if full.startswith(ROOT + os.sep) else full


def main(argv):
    paths = argv[1:]
    if paths and paths[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    files = expand(paths) if paths else repo_files()
    scanned, findings = 0, []
    for path in files:
        try:
            with open(path, "rb") as f:
                raw = f.read()
        except OSError as e:
            print(f"check-signals: cannot read {path}: {e}", file=sys.stderr)
            return 2
        if b"\0" in raw[:4096]:
            continue
        text = raw.decode("utf-8", errors="replace")
        rel = rel_of(path)
        if script_kind(rel, text) is None:
            continue
        scanned += 1
        findings += check_text(rel, text)
    for rel, n, msg, src in findings:
        print(f"check-signals: {rel}:{n}: {msg}: {src}", file=sys.stderr)
    if findings:
        print(f"check-signals: FAIL, {len(findings)} signal(s) to an unrecorded target", file=sys.stderr)
        return 1
    print(f"check-signals: ok, {scanned} script(s) read, no signal to an unrecorded target")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
