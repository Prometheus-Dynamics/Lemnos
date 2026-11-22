#!/usr/bin/env python3
"""The checker's own cases: scripts/check-signals.py is run by its absolute path on planted text.

Each fixture is a text file written to a temporary directory. Nothing here runs the script under
test, and no fixture is executed: the checker only reads them. Every refused case must fail by name
(the line's text is in the output), and every allowed case must pass. The last case runs the
checker over the repository itself, so the real scripts are a case too.
"""

import os
import subprocess
import sys
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CHECKER = os.path.join(ROOT, "scripts", "check-signals.py")

GUARD_DEF = 'is_group() {\n  case "$1" in \'\'|*[!0-9]*) return 1 ;; esac\n  [ "$1" -gt 1 ]\n}\n'
SH = "#!/usr/bin/env bash\n"


def run_checker(paths):
    return subprocess.run(
        [sys.executable, "-I", CHECKER, *paths],
        capture_output=True,
        text=True,
    )


class SignalCases(unittest.TestCase):
    def check(self, name, body, ok, needle=None):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, name)
            with open(path, "w", encoding="utf-8") as f:
                f.write(body)
            r = run_checker([path])
            if ok:
                self.assertEqual(r.returncode, 0, f"{name} should pass:\n{r.stderr}{r.stdout}")
            else:
                self.assertEqual(r.returncode, 1, f"{name} should fail:\n{r.stdout}{r.stderr}")
                self.assertIn(needle, r.stderr, f"{name}: the failure must name the line")

    def test_refused(self):
        cases = [
            ("kill -9 -1", SH + "kill -9 -1\n", "-1"),
            ("kill -- -1", SH + "kill -- -1\n", "-1"),
            ("kill with 0", SH + "kill -TERM 0\n", " 0"),
            ("kill of a group, unguarded", SH + 'pg=3\nkill -KILL -- "-$pg"\n', '-$pg'),
            ("kill -s of a group", SH + 'kill -s TERM -- -"$x"\n', "-$x"),
            ("guard on another line", SH + GUARD_DEF + 'is_group "$p"\nkill -STOP -- "-$p"\n', "-$p"),
            ("pkill", SH + "pkill -f sleep\n", "pkill"),
            ("killall", SH + "killall sleep\n", "killall"),
            ("fuser -k", SH + "fuser -k 8080/tcp\n", "fuser"),
            ("xargs kill", SH + "pgrep -f x | xargs kill\n", "xargs"),
            ("pgrep fed to kill", SH + "kill $(pgrep -f rehearse)\n", "pgrep"),
            ("guard called, not defined", SH + 'is_group "$p" && kill -KILL -- "-$p"\n', "defined nowhere"),
            ("python os.kill(-1)", "import os\nos.kill(-1, 9)\n", "-1"),
            ("python os.kill(0)", "import os\nos.kill(0, 9)\n", "os.kill"),
            ("python pkill argv", "import subprocess\nsubprocess.run(['pkill', '-f', 'x'])\n", "pkill"),
        ]
        for name, body, needle in cases:
            with self.subTest(name):
                fname = "t.py" if body.startswith("import") else "t.sh"
                self.check(fname, body, ok=False, needle=needle)

    def test_allowed(self):
        cases = [
            ("guarded group kill", SH + GUARD_DEF + 'p=3\nis_group "$p" && kill -KILL -- "-$p"\n'),
            ("grouped with braces", SH + GUARD_DEF + 'p=3\n{ is_group "$p" && kill -KILL -- "-$p"; } || kill -KILL "$p"\n'),
            ("probe of a recorded pid", SH + 'p=3\nkill -0 "$p" 2>/dev/null || true\n'),
            ("kill of a recorded pid", SH + 'kill -TERM "$p"\n'),
            ("comment and echo", SH + "# never pkill, never kill -1\necho 'kill -1 is refused'\n"),
            ("quoted word after kill", SH + 'echo "pkill and killall are refused" # kill -9 -1\n'),
            ("python killpg of a recorded group", "import os\nos.killpg(3, 9)\n"),
            ("python guarded negative", "import os\nis_group(p) and os.kill(-p, 9)\n"),
        ]
        for name, body in cases:
            with self.subTest(name):
                fname = "t.py" if body.startswith("import") else "t.sh"
                self.check(fname, body, ok=True)

    def test_scope(self):
        # A file that is not a script is not read, so a word in its text is not a signal.
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "notes.txt")
            with open(path, "w", encoding="utf-8") as f:
                f.write("pkill -f x\nkill -1\n")
            r = run_checker([path])
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_repository(self):
        # The real tree: every tracked script passes. The findings, if any, name the file and line.
        r = run_checker([])
        self.assertEqual(r.returncode, 0, f"{r.stdout}{r.stderr}")


if __name__ == "__main__":
    unittest.main()
