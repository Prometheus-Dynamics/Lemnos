# Lander

The lander is the local merge step for `next`. It stacks each PR branch onto the base in order, runs the
full gate and both QEMU harnesses on this machine at the same time, and on green pushes the stack to `next`
as a fast-forward. Nothing lands on red. GitHub CI does not gate a merge on `next`; the lander does.

## Files

- `land.sh <suffix> <id>...` starts `l-train.sh` as a systemd user unit named `<LAND_UNIT_PREFIX><suffix>`.
  The unit's log is `<LAND_STATE_DIR>/run-<suffix>.log`.
- `l-train.sh <id>...` is the train. Each `<id>` is a branch `pr/<id>` on the remote.
- `consumer-compile.sh <base> <cand> <logdir>` is the consumer-compile lane (below).
- `cc.py` is its helper. It has no data of its own.
- `env-load.sh` reads the machine's env file and checks it. The three scripts above source it.
- `env.example` is the template for the env file.

## Machine settings

Nothing machine-specific is in these scripts. Paths, CPU sets, job counts, the remote, the PR repository, the
unit prefix and the consumer data directory all come from the env file named by `LEMNOS_LAND_ENV`. Copy
`env.example` outside the repository, fill in every variable, and export `LEMNOS_LAND_ENV` to point at the
copy. A missing or unset `LEMNOS_LAND_ENV`, or an unset variable, stops the lander before it changes anything,
and the message names the variable.

The variables are described one by one in `env.example`. The remote can be a remote name or a URL or path to a
bare repository. With a path, a rehearsal pushes only to that repository. `LAND_PR_REPO` empty means no PR is
closed on landing.

## Consumer data

Some consumers of the Lemnos crates are compiled by the lander. Their data is in git under `consumers/`, by
label (no consumer is named in the repository), and `setup` writes the machine's copy to the directory named
by `LAND_CC_DATA`:

- `consumers.tsv`: one line per consumer, with its label, repository path (found by setup), pinned rev,
  workspace directory, lockfile path and an optional status. A status of `excluded: pre-port pin` means the
  consumer is not checked.
- `shims.tsv`: Lemnos packages and features added to the tree copy when the tree lacks them.
- `fields.tsv`: scratch values for E0063 "missing field" errors that the compile may fill.
- `fixes/<consumer>-<rev8>.json`: scratch edits for one consumer at one pinned rev.

The code finds each consumer's files by label from that directory. Nothing in the scripts names a consumer.

## Setup

Everything the lander needs outside git is made from git by one command, `scripts/land/setup [--base DIR]
[--repo PATH] [--pin REV]` (DIR defaults to `/mnt/nvme-wd/lemnos-land`). It writes the env file from
`machine.env`, the guard wrappers from `guard/`, `tools/slot-run.sh`, and the consumer data from
`consumers/` (each consumer's checkout found by its pinned commit), and with `--pin` copies the lander
scripts at REV to `bin/land-<rev8>`, which trains run from. Run again, it rewrites only what differs and
names it. The caches (`cache/qk`, `cache/tarballs`, `cache/busybox`, and the consumer compile's) start
empty and fill as trains run: a missing cache costs time, never a different result.

The consumer fixes (`consumers/fixes/`) are gate inputs and live in git: one file per consumer pin, each
entry with a one-line reason that names what it bridges at that pin (`consumers/fixes/README.md`).

## Lanes

A train runs five lanes at once, each with its own CPU set and its own bound:

- `notes`: `scripts/check-boundaries.sh` and `scripts/check-docs.py --list`, every train (seconds).

- `gate`: `scripts/gate.sh` in the stack worktree, with `CARGO_BUILD_JOBS` from `LAND_JOBS_GATE`, on
  `LAND_CPUS_GATE`. Bound: 109164 s, 4 x the sum of the gate's step budgets (a backstop; each step is bounded by its CPU budget and its progress watch).
- `qemu`: the modules QEMU harness, on `LAND_CPUS_QEMU`. Its kernel comes from `LAND_QK_DIR`, built when missing.
  Its modules are kept under `$LAND_SCRATCH/qemu-modules` by the hash of all they are built from
  (`LEMNOS_QEMU_MODULES`), so a train that does not change them builds none. Every stanza runs each train:
  most trains deal them over three guests side by side (`run.sh --split orion-link,ncm-after-controller
  gadget,route-a`), and every `LAND_WHOLE_EVERY`-th train (default 10) runs them all in one boot, so the
  one-boot order is still checked. Bound 1000 s.
- `off`: the modules-off kernel and the built-in and the watchdog runs, side by side on `LAND_CPUS_OFF`.
  The kernel is kept like the modules kernel, its key also holding the tree hash of `kernel/`, which it
  builds in; the newest three are kept. Bound 600 s each.
- `ccomp`: consumer compile, niced, on `LAND_CPUS_CC`.

Each lane writes its exit status to `<n>/<lane>.rc`. No ceremony runs: a part is keyed by the paths it
reads, and one already green for its key is not run again. The notes (`TODO.md`, `PARKED.md`, `notes/`,
`testing/board-runs/`) are read only by the `notes` lane, so the gate's key is the tree without them. `docs/`
is read by the gate's tests and checks and by nothing the other lanes run, so the `qemu`, `off` and `ccomp`
key is the tree without the notes and `docs/` (`ccomp`'s also holds the base). A notes-only commit runs only
the `notes` lane; a docs-only one runs it and the gate. When every lane is 0 the stack is pushed; otherwise the train prints the first failure lines of each
red lane and pushes nothing.

## Consumer compile

The `ccomp` lane compiles the consumers a train can break, against the base and against the candidate, and
fails only on errors the candidate adds.

Trigger. The changed files between base and head are checked for a public change in a crate that some consumer
locks. A changed Rust file counts when an added or removed line is a declaration, attribute, signature, field,
variant or macro line, and it is not a test path. A changed `Cargo.toml` of a locked crate counts, and the
workspace manifest counts for every consumer. Unsure lines count. When nothing counts the log says
`consumer-compile: skipped (no public change in consumer crates)`, which takes about half a second. Docs- and
test-only trains take that path.

Compile. Per affected consumer and side, the consumer's pinned rev is archived, the tree at the sha is
archived with its shims applied, and the consumer is copied to a work directory. Its manifest gets a patch for
each Lemnos package. A package the consumer locks that the tree lacks gets an empty stand-in with the features
the manifests ask for, up to 8 resolution passes. Then the scratch fixes for the pinned rev are applied, and
`cargo check --workspace --all-targets --all-features --keep-going --message-format=json` runs with
`CARGO_BUILD_JOBS` from `LAND_JOBS_CC`. Missing fields with a value in `fields.tsv` are filled and the
compile runs again, up to 4 passes.

Keys. An error is keyed by code, package, file and message, with line numbers stripped. The candidate's keys
minus the base's are NEW, and the reverse are FIXED.

Verdict.
- Fail when there are NEW errors and no commit in base..head carries the trailer
  `Consumer-break-announced: <text>`.
- Fail when the candidate reaches fewer workspace members than the base, unless the same trailer covers it.
- A base shortfall does not fail. If the base itself does not reach every member, the log says so loudly and
  names the members. A break already on `next` would otherwise fail every later train.
- Pass when there are no new errors and the candidate hides nothing.
- Did not run (exit 2): a compile failed with no compiler error, a scratch edit or default field no longer
  applies, a fixes file is missing for the pinned rev, a Lemnos package still resolves to git, or the defaults
  do not converge in 4 passes. A consumer with no fixes file for its pin is refused rather than guessed.

Exclusions. A consumer whose status is `excluded: pre-port pin` is not checked. The log says
`<consumer>: not checked (pre-port pin <rev>; ...)`. When a pin moves: clear the status, add
`fixes/<consumer>-<newrev8>.json` (without it the step refuses to run), and check the coverage. Cache entries
are keyed by rev, so stale ones are ignored.

Cache. Under `LAND_CC_CACHE`: `locks/<consumer>-<rev8>.names` (the Lemnos packages per pinned rev) and
`baseline/<consumer>-<rev8>-<sha8>-<tool8>.keys` plus `.cov`: the errors and coverage of one commit, written
for both sides. A train's candidate is the next train's base, so in a serial lander the base is always
found there. A rerun of the same candidate finds its own result there too. The `<tool8>` hash covers the
driver, `cc.py`, `shims.tsv`, `consumers.tsv`, `fields.tsv`, the `fixes/` files and the toolchain
(`rustc -vV`, `cargo -V`), so a change to any of them recomputes every result.

Warm builds. Under `LAND_CC_WORK` the Lemnos tree is always at `trees/live` and each consumer's copy at
`work/<consumer>-<rev8>`. Each compile syncs a fresh archive into them and rewrites only the files that
differ, so cargo rebuilds only the crates a commit changed.
The `flock` on `$LAND_CC_WORK/lock` lets one compile run at a time.

Kernel cache. `LAND_QK_DIR` holds QEMU kernel trees keyed by their inputs. A tree is per path: its build
files carry absolute paths, so a copied tree silently builds against the original. Never copy a kernel cache
dir; a new path builds its own tree from the tarball.

Limits. A pre-existing break in a crate hides its dependents from cargo. The base shortfall is logged and
named, and a candidate that hides members from the compile fails unless it is announced.

## Running a rehearsal

A rehearsal lands a throwaway commit through the same scripts without touching the real remote.

1. Make a bare clone of the remote's `next` under a scratch directory, and push a `pr/<id>` branch to that bare
   repository with the commit to land.
2. Write an env file whose `LAND_REMOTE` is the bare repository, whose worktree, scratch, state, temp, lock and
   cache paths are all in the rehearsal's own scratch directory, whose `LAND_PR_REPO` is empty, and whose
   `LAND_UNIT_PREFIX` is a prefix other than the real one.
3. Run `land.sh <suffix> <id>` with that env file. The train ends `LANDED` when it is green and `RED` with
   nothing pushed otherwise.

A rehearsal runs every lane, so it takes as long as a real train. Keep the rehearsal's caches apart from the
real ones.
