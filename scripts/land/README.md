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

Some consumers of the Lemnos crates are compiled by the lander. Their data is kept out of the repository in the
directory named by `LAND_CC_DATA`:

- `consumers.tsv`: one line per consumer, with its name, repository path, pinned rev, workspace directory,
  lockfile path and an optional status. A status of `excluded: pre-port pin` means the consumer is not checked.
- `shims.tsv`: Lemnos packages and features added to the tree copy when the tree lacks them.
- `fields.tsv`: scratch values for E0063 "missing field" errors that the compile may fill.
- `fixes/<consumer>-<rev8>.json`: scratch edits for one consumer at one pinned rev.

The code finds each consumer's files by name from that directory. Nothing in the scripts names a consumer.

## Lanes

A train runs four lanes at once, each with its own CPU set and its own bound:

- `gate`: `scripts/gate.sh` in the stack worktree, with `CARGO_BUILD_JOBS` from `LAND_JOBS_GATE`, on
  `LAND_CPUS_GATE`. Bound 3600 s.
- `qemu`: the modules QEMU harness, on `LAND_CPUS_QEMU`. Its kernel comes from `LAND_QK_DIR`, built when missing.
  Bound 1000 s.
- `off`: the modules-off kernel and the built-in and three watchdog runs, side by side on `LAND_CPUS_OFF`.
  Bound 600 s each.
- `ccomp`: consumer compile, niced, on `LAND_CPUS_CC`.

Each lane writes its exit status to `<n>/<lane>.rc`. A part that already passed for the same tree is not run
again. When every lane is 0 the stack is pushed; otherwise the train prints the first failure lines of each
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
`baseline/<consumer>-<rev8>-<base8>-<tool8>.keys` plus `.cov` (the base's errors and coverage, once per base).
The `<tool8>` hash covers the driver, `cc.py`, `shims.tsv` and `consumers.tsv`, so a change to any of them
recomputes the baseline.

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
