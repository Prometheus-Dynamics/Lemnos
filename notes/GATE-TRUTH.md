# Gate truth: every lane that could pass on a failure

An audit of the gate and the lander found lanes whose failure could read as a pass. Each is fixed, and each has a
planted failure in `testing/gate-lanes/check.sh`. Run against the tree before the fixes (0837c60f), all 13 cases
are NOT CAUGHT. Against this one, all 13 are caught (50 s wall, 23 CPU-s, nice 10, load 149).

| # | Where | What could pass | Now |
|---|---|---|---|
| 1 | `scripts/land/consumer-compile.sh`, `cc.py` trigger | git failing in the trigger check (or any trigger failure) read as "skipped", exit 0. With no consumers at all, cc.py exited on its usage line and that was "skipped" too | the trigger's status and its RESULT line are required; git failures in cc.py exit 2 |
| 2 | consumer-compile keys, coverage, newerrs, cache | their statuses were dropped; an empty coverage result was cached and later read as a pass; `covfail` was never set | each status checked (exit 2, "did not run"); a cached result must be whole; `covfail` removed |
| 3 | `scripts/check-shipped.sh` licences | a failing `cargo tree` inside `< <(...)` gave an empty list, and the step held | the list is taken into a variable and must not be empty |
| 4 | `scripts/check-libraries.sh` | `cargo tree ... \|\| true` hid a failure; `DEPS_ONLY=1` (used by nothing) stopped before lint and tests with exit 0 | cargo tree on its own line, its own line required; `DEPS_ONLY` removed |
| 5 | `scripts/check-packaging.sh` | `systemd-analyze verify` saying "not found" skipped the rest with "packaging: ok"; a missing udevadm, sysusers or systemd-analyze, or systemd older than 254, was a skip; busybox and ShellCheck were never there, so the init scripts were never checked by either | every complaint and every missing tool is a red; the init scripts are parsed by the board's static BusyBox ash (`testing/qemu/build-busybox-init.sh`) and the pinned ShellCheck v0.11.0 (`scripts/fetch-shellcheck.sh`, GitHub's published sha256). ShellCheck finds nothing in them |
| 6 | `testing/mdev/inner.sh`, `check.sh` | mdev's status was dropped: a failed mdev leaves the placeholders as made, which can match an expectation | inner.sh exits with mdev's status; check_one fails on it |
| 7 | `scripts/check-boundaries.sh` self-test | a fixture "caught" by any failure, a crash or another rule included | each fixture's expected message is named, and the exit must be 1 |
| 8 | the lander's gate key | `testing/board-runs/` was a note, outside the gate key, but documents' commands read it (`docs/hardware-status.md`, `docs/testing.md`); a runnable block in a note would never run | board runs are in the gate key; `check-docs.py` refuses a runnable block in `notes/`, `TODO.md`, `PARKED.md` |
| 9 | `testing/devtree/build-dtc.sh` | the guard's message was inside a comment; errexit still stopped the script, so this was a red with no message, not a pass | the guard prints its message |

## The rerun on a stall, removed

`scripts/land/gate-attempts.sh` ran the gate again when it exited 3 (a step that failed while starved), so a
starved red could land as green on the second run. The gate now runs once. In `scripts/gate-bounds.sh`:

- A failure is exit 1 whatever the load; starvation is evidence in its log ("was starved while it ran", from the
  run-queue wait), never another status.
- A stall is counted only on time blocked: a look is progress when the tree used CPU, or its run-queue wait in
  `/proc/<pid>/schedstat` grew (time runnable, waiting for a CPU), or a process is in state R now. The old look saw
  state R only at the instant of the look.
- A stalled step's log has each process's state, run-queue wait, wchan, system call and where that call's first
  file descriptor leads. For example, cargo waiting on a build lock:
  `pid N (cargo) state S, runnable wait 0.8 s, wchan locks_lock_inode_wait, syscall 73 on fd 8 -> .../target/debug/.cargo-lock`.
- `testing/gate-bounds/check.sh`: 9 of 9 cases, starved-step now wants exit 1 with its evidence. It had four
  competitors on its core; now two, the most allowed.

## The suspects

- `scripts/check-kernel-config.py`: real. Expressions the parser could not read were skipped, so their
  dependencies were never followed. On the cached 7.3.5 trees that is `BLK_DEV_DM`'s `DAX || DAX=n` and `RUST`'s
  `!KASAN_SW_TAGS || RUSTC_VERSION >= 109600`. The token pattern also lost `!=` (it took `!` first). Comparisons
  (`=`, `!=`, `<`, `<=`, `>`, `>=`) are now read and evaluated, and an expression that still cannot be read is a
  problem of the run. With them read, both trees pass. Self-check cases added.
- `scripts/check-features.sh`: real, the same `< <(...)` pattern. A failing `sets` gave the loops no input, and the
  step reported 0 crates and held. The sets are taken into variables, and an empty shipped list fails. The test
  binary's `--list` status is checked too. Not planted in check.sh, because the step builds every crate.
- `scripts/check-signals.py`: not real. An unreadable file returns 2. A binary file or a file that is not a script
  is not a script to check.

## The rest of the class, swept

- `scripts/check-boundaries.sh`: every list a rule walks is taken into a variable (`grep_any` tells no match from a
  read error), an empty file list fails, and an unreadable file is a failure.
- `scripts/gate.sh`: the test shard's package list comes from a variable, not empty.
- Checked and left as they are:
  - `scripts/check-orion-copies.sh`: an empty list is already refused.
  - `testing/mdev/check.sh`'s negative case: its output is checked line by line.
  - `scripts/features-cache.py`: skips only lines that are not JSON (cargo's own messages).
  - consumer-compile's "no public change" skip: the designed outcome of a trigger that ran.

## The earlier lander's trains, by positive markers

The earlier lander's run logs for 2026-11-19 to 11-23 are no longer on disk, so those trains cannot be read now.
What remains is trains 9 to 16 (2026-11-17), in the old scratch directory:

- Trains 11, 12, 13, 14 and 16 landed with gate, off and QEMU status 0.
- Their gate logs end "gate passed" with every step's time line.
- Trains 12, 13, 14 and 16 ran QEMU, and their logs end "lemnos-qemu: passed (0 failed)".
- Train 11 reused the QEMU and modules-off green of its tree (`e6e3bd45`), which train 10 earned with
  "lemnos-qemu: passed (0 failed)".
- Train 10's gate was red on that same tree, and train 11's gate was green. Train 10's red was the boundary
  check finding a forbidden name in `target-off/`, a build directory the lander had put inside the worktree: dep
  files that carry the build path. With the directory out of the worktree, train 11 passed. A named cause,
  outside the tree under test, fixed before the next run: not a hidden red.
- No unit-check skip and no consumer compile (it was not a lane yet).
- Every one of them skipped the init scripts' BusyBox and ShellCheck parses: 4 "not installed" lines each, hole 5.
  ShellCheck and the board's ash now find nothing in the scripts as they are, so nothing landed broken through
  that gap.

No hidden red in what can be read.
