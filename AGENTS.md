# Working on Lemnos

Working instructions for anyone, person or agent, changing this repository. Read
[`README.md`](README.md) for what Lemnos is and [`docs/design.md`](docs/design.md) for why it is
built this way. [`TODO.md`](TODO.md) lists what is open.

## Build and test

Rust comes from `rust-toolchain.toml`. Build a crate, lint it, test it:

```sh
cargo build -q -p lemnos-wire
cargo clippy -q -p lemnos-wire --all-targets -- -D warnings
cargo test -q -p lemnos-wire
```

Try a change against the simulated board:

```sh
cargo build -q -p lemnosd -p lemnos-ctl --features lemnosd/fake-devices
export PATH="$PWD/target/debug:$PATH"
export LEMNOSD_SOCKET="${TMPDIR:-/tmp}/lemnosd.sock"
lemnosd --sim --board docs/examples/sim-board.toml &
```

```sh
lemnos-ctl list
kill %1
wait
```

Before a change lands, the whole gate must pass. It is what CI runs.

```sh needs:the-gate-itself
scripts/gate.sh
```

[`docs/testing.md`](docs/testing.md) describes the gate, the QEMU harness for the kernel module,
and board runs.

## Landing a change

- Work on a branch and open a pull request into `next`. `dev` is the earlier version, kept as a
  reference: read it with `git show origin/dev:<path>`, and never change it.
- A change lands when `scripts/gate.sh` passes. Check its exit status before committing. Paste the
  output that proves the change into the pull request.
- Never push tags. Never force-push `next` or `dev`.
- Workspace members are globs: add a crate by creating its directory under the right folder.
  Inherit shared dependencies with `workspace = true`. `Cargo.lock` is ignored.
- After adding a driver, run `python3 scripts/generate-builtins.py` so the daemon lists it.
- Nothing is frozen. A shared interface (`engine/abi`, `engine/hal`, `engine/core`,
  `engine/macros`, `engine/wire`, the kernel interface, the board format) may change when the work
  needs it, in the same change, as long as every implementation, test and document follows.
- Keep the documents true. A change that alters behaviour alters the page that describes it, and
  `scripts/check-docs.py` runs every command the documents show.

## Rules

- **Lemnos is only the engine.** Nothing in `engine/` or `service/` may know what a kind of
  hardware is. `engine/` never imports `contracts/`, `drivers/`, `backends/` or `service/`.
- **No vendor libraries.** Nothing closed is linked, loaded or copied. The dynamic loader is used
  only for Lemnos's own driver packages. Per the 2026-11-13 ruling, register facts may be transcribed
  from open-source vendor user-space code with provenance (source file and version); the libraries
  themselves are neither used nor linked.
- **No software stand-in for hardware.** A driver carries the caller's work to its block without
  knowing what it means. A job the block cannot do fails. It is never done on the CPU instead.
- **The failure rule, in every driver.** Never give up on hardware: reconnect, with a wait capped
  at 8 s. No automatic off. A caller's request is never redone and never replaced.
- **`unsafe` only in** `engine/abi`, `engine/capi`, `engine/measure`, `backends/linux-sys` and
  crates named `drivers/*-sys`, with one operation per block and a `SAFETY` comment.
- **A caller's bytes are never trusted.** Every address, offset, size, descriptor or handle that
  comes from a caller is checked against memory the caller really holds, with arithmetic that
  cannot overflow, before hardware sees it. If a kernel interface makes that impossible, the
  driver refuses the work and says why.
- **Steady-state paths allocate nothing.** Prove it with the counting allocator in
  `lemnos-measure`.
- **Never claim something works without having run it.** Report failures with their output. A test
  must be shown to fail without the code it tests.
- **Hardware you can't reach is marked `untested on board`** in the crate's documentation, with
  the check written to a file under `testing/board-runs/`. A change is not held back for a board
  run.
- **Tests never open real device nodes** on the machine they run on.
- **No product or board names** outside `testing/board-runs/`. Say "the reference board". No home
  directories or mount paths either. `scripts/check-boundaries.sh` enforces this.
- **The OV9782 datasheet never goes in the repository**, nor any other document we may not
  redistribute. Facts taken from one are written in our own words, with the source named.
- **Don't say how long anything will take.** Describe size by scope and risk.
- **Plain words, short sentences** in code comments and documents. No marketing.

## Board runs

A board run needs a real board, so it happens in a session on a machine that has one, after the
code has landed. That session takes `/run/lock/board-gpu.lock` with `flock` for everything that
touches the board, builds on a solid-state disk, and reports every module load, unbind and reboot
it caused. Timings are quoted only from the reference board.
[`docs/testing.md`](docs/testing.md#board-runs) has the steps.
