# Working on the Lemnos `next` branch

`next` is a clean-slate rebuild of Lemnos. It started empty on purpose. `dev` is the frozen
reference, and nothing here changes it. The work is a pre-planned list of PRs into `next`.

## If you are picking this up

1. Read [`docs/design.md`](docs/design.md). It is the design every PR builds.
2. Run `python3 scripts/pr-status.py` to see what's merged, open, ready and blocked.
3. Take a PR marked ready and read its brief at `docs/prs/<id>.md`. The brief is the whole
   assignment: what it needs, what it creates, which files on `dev` to read, what to build and what
   proves it.

[`TODO.md`](TODO.md) is the full list.

## Working a PR

- Base branch `next`, working branch `pr/<id>` (for example `pr/A2`), one PR into `next`.
- Read `dev` as a reference: `git fetch origin dev`, then `git show origin/dev:<path>`. Bring in only
  what the brief lists.
- Plain paths under "Read on dev" are files on `dev`. `TODO.md at <sha>` means
  `git show <sha>:TODO.md`. Anything else names its source: Styx is
  `Prometheus-Dynamics/Styx`, Orion is `Prometheus-Dynamics/Orion`, `rpi-7.2.y` is the Raspberry Pi
  kernel, and `radxa/allwinner-bsp` and `k230_linux_sdk` are the vendor sources. Those are references
  to read. Nothing from them is added as a dependency unless the brief says so.
- Stay inside the paths your PR creates. Don't edit the root `Cargo.toml`, `TODO.md`, or a changelog.
  `Cargo.lock` is ignored. Keeping to this is what lets many sessions run at once without conflicts.
- Add a crate by creating its directory under the right folder. Workspace members are globs. Inherit
  shared dependencies with `workspace = true`, and declare any others in your own crate.
- If your PR needs a shared interface changed, stop and say so in the PR description. Shared
  interfaces (`engine/abi`, `engine/hal`, `engine/core`, `engine/macros`) change only through
  A-series PRs.
- A PR merges when `scripts/gate.sh` passes. Paste the proof named in the brief into the PR
  description.

## Rules

- Lemnos is only the engine. Nothing in `engine/` or `service/` may know what a kind of hardware is.
- Block drivers follow the block rule in the design: no vendor library in Lemnos, and the driver
  carries the caller's work without knowing what it means.
- Failure rule in every driver: never give up, backoff capped at 8 s, no auto-off.
- `unsafe` only in `engine/abi`, `engine/capi`, `engine/measure`, `backends/linux-sys` and crates
  named `drivers/*-sys`, with one operation per block and a `SAFETY` comment.
- Steady-state paths allocate nothing.
- Hardware you can't reach is marked `untested on board`, with the check written to
  `testing/board-runs/<id>.md`. A PR is not held back for a board run.
- Never claim something works without having run it. Report failures with their output.
- Breaking other repositories is fine. They aren't built here.
- The OV9782 datasheet never goes in the repository.
- Never push tags. Never force-push `next` or `dev`. Don't change `dev`.
- Don't estimate how long anything will take. Describe size by scope and risk.
- Product names appear only in the planning files (this file, `TODO.md`, `docs/design.md`,
  `docs/prs/`). PR F1 removes them.
- A driver crate names its type under `[package.metadata.lemnos]`, and `scripts/generate-builtins.py` adds it to `lemnosd`. A maintainer runs that after merging, so a driver PR edits no service file.

## Board runs

These need a real board, so they run in a local session on the user's machine after a PR merges.
That session takes `/run/lock/board-gpu.lock`, uses the CM5 (Raze #2), and reports CM5 timings
only. Builds there go on NVMe or SSD, never on the Github HDD.
