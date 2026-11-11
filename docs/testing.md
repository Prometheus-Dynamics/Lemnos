# Testing

There are four levels of test. Each one reaches something the one before cannot.

| Level | Runs on | Reaches |
|---|---|---|
| The gate | Any Linux PC, and CI | All the Rust code, against simulated and mock hardware |
| The QEMU harness | A PC with QEMU, and CI | The kernel module, in a real kernel |
| The bench | A PC (dry run) or the reference board | The whole daemon, measured |
| A board run | A board | Real hardware. Nothing else does |

## The gate

A change lands when `scripts/gate.sh` passes. CI runs exactly that script.

```sh needs:the-gate-itself
scripts/gate.sh
```

What it runs, in order:

1. `cargo fmt --all --check`.
2. `cargo clippy --workspace --all-targets -- -D warnings`.
3. `cargo test --workspace`.
4. `cargo check` of the `no_std` crates for three targets: a Cortex-M, a RISC-V microcontroller and
   32-bit ARM Linux.
5. A few feature combinations: the measurement counters, the client on tokio, the C API as a shared
   library.
6. `scripts/check-boundaries.sh`: the layering rules.
7. `scripts/check-docs.py`: the commands and links in these documents.

The last two also run on their own:

```sh
scripts/check-boundaries.sh
scripts/check-docs.py --list
```

### How the Rust tests reach hardware they do not have

- **The simulated board** (`backends/sim`): buses whose chips are register files a test fills in,
  a clock that moves only when the test moves it, and scripted faults. Chip drivers are tested on
  it.
- **Mocks** (`lemnos-hal`, feature `mock`): one in-memory double for each hardware trait.
- **Played-back kernel nodes**: a block driver talks to its node through a small trait, and its
  tests put a model of the kernel behind it. Tests never open a real device node on the machine
  they run on. The few that can are off unless `LEMNOS_TEST_DEVICES=1` is set.
- **The counting allocator** (`lemnos-measure`): a test wraps a steady-state loop and fails if it
  allocated. Calls, values, frames and jobs must allocate nothing.
- **Test vectors**: the frames in [the protocol specification](wire.md#test-vectors) are checked
  against the code in both directions.
- **Planted violations** (`testing/boundary/`): one small tree per layering rule, each breaking it.
  The boundary check must fail on every one.

Two tests run on a real dma-heap when told where one is, and are skipped otherwise:

```sh needs:a-dma-heap-this-account-may-open
LEMNOS_TEST_DMA_HEAP=/dev/dma_heap/system cargo test -p lemnosd --test stream_dma --test stream_fill
```

### The boundary rules

`scripts/check-boundaries.sh` fails when:

- a crate in `engine/` depends on `contracts/`, `drivers/`, `backends/` or `service/`;
- a type, module or file in `engine/` or `service/` is named after a kind of hardware;
- `unsafe` appears outside `engine/abi`, `engine/capi`, `engine/measure`, `backends/linux-sys` and
  `drivers/*-sys`, or a crate drops the workspace lints that forbid it;
- a product name, a home directory or a mount path appears anywhere outside `testing/board-runs/`.

### The document check

`scripts/check-docs.py` runs every command block in these documents against the simulated board,
checks that an example shown in a document is the file it names, and checks every relative link.
A block marked `slow` runs only with `--slow`. A block marked `needs:` is never run, and the check
lists it with what it needs.

```sh needs:the-document-check-itself
scripts/check-docs.py
scripts/check-docs.py --slow
```

## The QEMU harness

The kernel module is tested in a virtual machine. `testing/qemu/run.sh` builds the module, a test
block with nothing behind it and a stand-in for the RP1 PIO driver, boots a kernel with them as the
whole image, and runs the checks in `kernel/test/` and `lemnos-module-check`, which drives the real
Linux backend through the module and then through the stock interfaces. It fails unless every
check passed and the kernel reported nothing wrong while they ran.

```sh slow
CC="zig cc -target x86_64-linux-musl" testing/qemu/run.sh
```

It uses this machine's kernel and headers unless `KDIR` and `KERNEL` name another built tree and
its image. `CC` names a compiler that can link static binaries. Left out, it is `gcc`, which needs
the static C library installed; `zig cc` with a musl target needs nothing more. The harness also
needs QEMU, `cpio`, and the Rust target `x86_64-unknown-linux-musl`.

CI runs it against a kernel built with lock and memory checking on. That kernel is built by:

```sh needs:a-download-and-a-full-kernel-build
testing/qemu/build-kernel.sh "$TMPDIR/kernel"
```

`testing/bench/module.md` has the module route's software cost against the stock route, measured
in this harness. Those are numbers from a virtual machine with simulated adapters. They say nothing
about a real bus.

## The bench

`testing/bench/bench.sh` measures the whole daemon against a reference set
(`testing/bench/reference.tsv`): sample delivery at 200 Hz, CPU, memory, threads, binary sizes and
more. The design's ["Proof on hardware"](design.md#proof-on-hardware) says what the set is.

A dry run needs no board. It builds the shipped binaries, starts `lemnosd --sim` on the bench board
file, records what a simulation can give, and prints the report:

```sh slow
testing/bench/bench.sh --out "$TMPDIR/bench"
```

A real run goes to the board over ssh, stops the services that hold the hardware, runs the set and
starts them again:

```sh needs:the-reference-board
testing/bench/bench.sh --real --board USER@HOST
```

The earlier figures the report prints beside its own come from the `dev` branch's records
(`git show ed43ff3:TODO.md`). Some rows of the reference set still say a measurement has no step in
the harness. The real run has not been done: see [Hardware status](hardware-status.md).

## Board runs

Code written without the hardware is marked **untested on board**, in its crate's documentation
and in a file under `testing/board-runs/`. The file is the check: what the PC tests could not
show, the board-file entry to use, numbered steps with commands, and for each step what counts as
a pass.

A file is named after the change that wrote it. The name is only a label. Find the file for a
crate by searching:

```sh
grep -l 'drivers/bmi088' testing/board-runs/*.md
```

### Doing one

1. **Take the board lock.** One session at a time uses a board. Every command that touches it runs
   under `flock /run/lock/board-gpu.lock`.
2. **Build on a solid-state disk**, for the board:

   ```sh needs:the-board-target-installed
   cargo build -p lemnosd -p lemnos-ctl --profile service --target aarch64-unknown-linux-musl
   ```

   Use a glibc target when the run needs driver packages.
3. **Stop whatever else holds the hardware** on the board: an older daemon, a stock driver bound
   to the block. Note what you stopped, and start it again afterwards.
4. **Write the board file** the run's file gives, and check it:

   ```sh needs:a-board
   lemnosd --check --board board.toml
   lemnosd --board board.toml --socket /run/lemnos/lemnosd.sock
   ```

5. **Do the steps in order.** Some are destructive on purpose (arming a watchdog, wedging a
   block) and say so. Those go last.
6. **Record the result in the file**, under its steps: the command output, the timings, and
   anything that did not match. Change its `Status:` line from `untested on board` to what was
   run and when. A step that failed is recorded as failed, with its output.
7. **Report what you changed on the board**: modules loaded, devices unbound, slots changed,
   reboots. All are allowed. None may go unsaid.
8. **Fix the crate's documentation.** When a driver's check passes, its `untested on board` line
   changes to say what ran. Do the same for its row in [Hardware status](hardware-status.md).

Timings are quoted only from the reference board, and always with the board named in the run's
file.

A board run never holds a change back. Code lands marked untested, and the run follows.
