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

1. `cargo metadata --locked`: `Cargo.lock` matches the manifests (docs/building.md, "The lock file").
2. `cargo fmt --all --check`.
3. `cargo clippy --workspace --all-targets -- -D warnings`.
4. The workspace tests. With `cargo-nextest` installed they run as `cargo nextest run --workspace` (every test in
   its own process, on two thirds of the cores, since the gate's other lanes build beside them) and then
   `cargo test --workspace --doc`; without it, or with `GATE_TESTS=cargo`, as `cargo test --workspace`. The tests
   are the same either way. `GATE_TEST_JOBS` sets how many run at once. Tests may therefore run as separate
   processes side by side: a test that builds or writes a shared file takes a file lock, not a lock in its own
   process.
5. `cargo check` of the `no_std` crates for three targets: a Cortex-M, a RISC-V microcontroller and
   32-bit ARM Linux.
6. A few feature combinations: the measurement counters, the client on tokio, the C API as a shared
   library.
7. `scripts/check-boundaries.sh`: the layering rules.
8. `scripts/check-docs.py`: the commands and links in these documents, the driver list and the
   board-run list.

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

The camera chain tests (`lemnos-camera-proof`) take the stream buffers the host gives: dma-bufs from
its `system` dma-heap when it has a usable one, sealed memfds when it doesn't. The two hosts run
different paths, and each run says which in its output. With dma-bufs the front end's frames are
lent to the back end. With memfds the kernel refuses to lend them, so the proof copies each frame
through the back end's `input` stream instead, and checks the refusal of one lent job on a memfd
(`bad-config`). `LEMNOS_TEST_NO_DMA_HEAP=1` makes the host look as if it has no usable heap, so a
dma-heap machine runs the memfd path, as a CI runner without a heap does. Nothing else changes.

### A consumer's tests: `lemnos-standin`

A program that uses Lemnos tests against a real service with `lemnos-standin`
(`testing/crates/standin`), taken as a dev-dependency at a locked revision. `StandIn::start(devices, sim)`
starts `lemnosd::Service` in-process, with every built-in driver the daemon ships, over a simulated board
the test builds (`SimBoard`: lines, PWM channels, buses, streams). `socket()` is where the test's client
connects. `wait_running` waits for named devices. `restart()` is a lemnosd restart: the same board,
socket and simulated parts, and nothing the service held. `FakeSys` lays out an hwmon fan and thermal
zones for the drivers that read `/sys`. `start_in(dir, ...)` puts the socket in a directory the test made,
next to that tree. What the hardware was told is read back from the simulated parts (`MockLine::level`,
`MockPwm::current`, `FanFiles::pwm1`). A light's colours are read through the service, with `current`.

It is the real service, so it refuses what a board refuses: a claim another client holds is `BUSY`, a line
a board device owns is `bind failed: busy` for any other device the board gives it, and an address the board
file doesn't allow is `NOT_PERMITTED`. `testing/crates/standin/tests/consumer_shapes.rs`
shows each use. The Orion precheck is left out (no Orion in a consumer's test).

### The boundary rules

`scripts/check-boundaries.sh` fails when:

- a crate in `engine/` depends on `contracts/`, `drivers/`, `backends/` or `service/` (except the C
  library on `backends/linux-stream`, the operating system's half of a frame stream, which a client
  program brings);
- a type, module or file in `engine/` or `service/` is named after a kind of hardware;
- `unsafe` appears outside `engine/abi`, `engine/capi`, `engine/measure`, `backends/linux-sys` and
  `drivers/*-sys`, or a crate drops the workspace lints that forbid it;
- a product name, a home directory or a mount path appears anywhere outside `testing/board-runs/`.

### The document check

`scripts/check-docs.py` runs every command block in these documents against the simulated board,
checks that an example shown in a document is the file it names, and checks every relative link.
It also holds [Drivers and contracts](drivers.md) to the code: its driver tables name exactly the
drivers `lemnosd` builds in, each with the contracts it provides; no crate in its library table
has a built-in driver; and no built-in driver's crate documentation says it is not built in. And
it holds [Hardware status](hardware-status.md) to `testing/board-runs/`: every written check is
listed there, and the count it gives is the count of files.
A block marked `slow` runs only with `--slow`. A block marked `needs:` is never run, and the check
lists it with what it needs.

```sh needs:the-document-check-itself
scripts/check-docs.py
scripts/check-docs.py --slow
```

## Fresh worktrees: the seed

A fresh worktree's first gate compiles every dependency. `scripts/seed-target.sh` fills a new
target directory with the third-party build artifacts from a seed instead.

```sh needs:a-seed-directory
LEMNOS_CARGO_SEED_DIR=<seed-dir> scripts/seed-target.sh save <target-dir>
LEMNOS_CARGO_SEED_DIR=<seed-dir> scripts/seed-target.sh load <target-dir>
scripts/seed-target.sh key <target-dir>
```

- `save` runs the workspace's clippy and test builds on `<target-dir>` (it builds only what is
  missing), then writes the seed. `load` fills an empty `<target-dir>/debug` from it. `key` prints
  the key the checkout would use.
- The seed directory has no default: the script writes nowhere unless `LEMNOS_CARGO_SEED_DIR` is
  set. Each seed is one directory named by its key: the hash of `Cargo.lock`, `rustc -vV`,
  `cargo -V`, the profile (`debug`, for dev and test builds) and the host triple. `load` refuses a
  seed whose key does not match, and a target that is not empty.
- A loaded target records its key in `<target-dir>/.lemnos-seed`.
- `save` and `load` hold `flock` on `<seed-dir>/.lock`. Loads share the lock, so worktrees can load
  at once. A save copies into a private directory and holds the lock only for the swap, so a load
  never reads a seed being replaced.
- Copies use `cp --reflink=auto`: a reflink on btrfs and xfs, a plain copy elsewhere.

What a seed holds, and why:

- **Registry and git dependency artifacts only.** A workspace member's artifacts carry the
  checkout's absolute paths (`CARGO_MANIFEST_DIR`, `file!()`, the build script's `OUT_DIR`), so
  cargo would reuse them as fresh while they read another checkout's files. `save` takes an
  artifact only from a unit whose package is registry or git, by the hash in its file names. It
  refuses to write a seed in which a hash also belongs to a path package. `MANIFEST` lists the
  packages seeded, one per line, and none of them is a workspace member.
- **Build-script outputs are re-pointed on load.** cargo replays a build script's `output` into
  rustc, and it names the `OUT_DIR` of the target it was saved from. `load` rewrites that path to
  the new target, keeping each file's mtime.
- **Git dependencies that depend on workspace crates are rebuilt.** A git package that depends on a
  workspace crate (through a `[patch]`) is dirty whenever that crate is rebuilt, and in a fresh
  worktree it always is. The seed still holds its artifacts, and cargo recompiles them. Check which
  crates are rebuilt with `cargo clippy -vv`, which names the dependency that made each unit dirty.

The seed is only useful where the filesystem has reflinks: btrfs and xfs here. On tmpfs
(`/dev/shm`) the copy is a plain one, about 1.5 s for the seed of this lock. Targets need about 15 to
25 GB each; the seed is about 0.7 GB.

## The QEMU harness

The kernel module is tested in a virtual machine. `testing/qemu/run.sh` builds the module, a test
block with nothing behind it, fake blocks for the module's register jobs (a register window of
ordinary memory, simulated interrupts and a counted reset line, one an encoder that finishes each
job and one an ISP that runs continuously) and a stand-in for the RP1 PIO
driver, boots a kernel with them as the
whole image, and runs the guest checks (`testing/crates/*-check`) and `lemnos-module-check`, which drives the real
Linux backend through the module and then through the stock interfaces. It fails unless every
check passed and the kernel reported nothing wrong while they ran.

```sh slow
CC="zig cc -target x86_64-linux-musl" testing/qemu/run.sh
```

It uses this machine's kernel and headers unless `KDIR` and `KERNEL` name another built tree and
its image. `CC` names a compiler that can link static binaries. Left out, it is `gcc`, which needs
the static C library installed; `zig cc` with a musl target needs nothing more. The harness also
needs QEMU, `cpio`, and the Rust target `x86_64-unknown-linux-musl`.

The guest's first process (`testing/crates/init`) runs the checks as a list of stanzas, each one
load of the module with the checks that need it loaded that way, and prints how long each took.
With no argument `run.sh` boots one guest that runs them all in order. That is the whole run, the
one that says a branch is good. Two arguments are for the work before it:

- `testing/qemu/run.sh --only pisp-fe,nand` boots one guest that runs the named stanzas, and the
  ones they start from. It is for a failing check looked at alone, and for showing that a check
  fails without the change it is about. Its last line says it is not the whole run.
- `testing/qemu/run.sh --split orion-link stream gadget,nand` deals the whole list out over guests
  that boot side by side: one per argument, and one more with every stanza no argument names. Every
  stanza runs. What it does not show is the stanzas one after another in one boot.

`LEMNOS_QEMU_MODULES` names a directory that keeps the modules `run.sh` builds, under the hash of
everything they are built from (the module's sources, the harness's, the kernel tree, the
compilers). A run whose hash is there takes them and builds none; a changed source is built. It is
for repeated runs of one tree. Unset, every run builds its modules, and a landing leaves it unset.

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
