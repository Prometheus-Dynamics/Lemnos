# Writing a driver

A driver is the code that talks to one part and says what it provides. Lemnos does not know what a
part is: it matches tags, carries calls and values, and runs drivers under its failure rules. There
are three ways to write a driver. Pick the first that fits.

| Form | What you write | Use it when |
|---|---|---|
| **Description** | A text file that describes the chip's registers. No code. | A register-based sensor with a contract that already exists, and a generic driver (`drivers/generic-sensor`) covers it. |
| **Built in** | A crate under `drivers/` with a `#[driver]` type. | The part needs code: a state machine, a block, a protocol. |
| **Package** | A compiled file that lemnosd loads through `lemnos-abi`. | A part that must be added to a device after the image is built. Not built yet for most drivers. |

The test for where a driver lives: it goes in Lemnos when a second, unrelated product would use it
unchanged. Which bus, address, pin or interrupt a part is wired to belongs in the board file, not in
the driver.

## The description form

The `magnetometer/1` contract is served by `drivers/generic-sensor`. Its contract and value names
are fixed in code. The file gives only the chip's facts.

A whole file, from `drivers/generic-sensor/tests/chips/sim-mag.chip`:

```text
# A made-up three-axis magnetometer for the simulated bus. It is not any real
# part, and its registers exist only as the simulated bus's bytes.

identity 0x00 0x48                      # the chip ID register reads 0x48

init 0x0a 0x80                          # soft reset
wait 2                                  # ms before the chip takes its next write
init 0x09 0x1d                          # continuous measurement

axis x 0x01 16 little signed 0.000001   # tesla per count
axis y 0x03 16 big signed 0.000001
axis z 0x05 24 little signed 0.000001
```

The lines are:

- `identity <register> <value>`: optional. Checked before every start. A wrong value is reported as
  `NOT_SUPPORTED` and health is `Failed("identity mismatch")`, and the next read checks again.
- `init <register> <value>`: a register write, run in file order when the chip starts.
- `wait <milliseconds>`: holds the start-up here. The driver answers `AGAIN` until the host's clock
  passes the time; nothing sleeps.
- `axis <x|y|z> <register> <bits> <little|big> <signed|unsigned> <scale>`: one axis of the vector.
  Each of x, y and z is needed once. `scale` is contract units per count (tesla here).

Numbers are decimal or `0x` hex, `#` starts a comment, and there are at most 32 `init` and `wait`
lines. Registers are 8-bit. An axis is 8, 16, 24 or 32 bits.

The chip's I2C address is wiring, so it goes in the board file. The description goes in the same
entry as one `description` setting. Without `std` it is the text itself; with `std` it is a path to
the file, read once at bind:

```toml
[[devices]]
id = "mag1"
driver = "generic-sensor"
bus = "<the bus the board names>"
config = { address = 0x2c, description = "/etc/lemnos/chips/sim-mag.chip" }
```

Binding parses the file once into fixed arrays. A file with a bad line is refused at bind with
`INVALID_ARGUMENT`; the parser's error names the line, and the tests in `src/tests.rs` check each
refusal.

To prove a new file, put it under `tests/chips/` and bind it on the simulated bus the way
`src/tests.rs` does. Then break one register in the file and check that the test fails before you
restore it.

## The built-in form

A built-in driver is a crate in `drivers/<name>/`. Its `Cargo.toml` names its type under
`[package.metadata.lemnos]`, and `scripts/generate-builtins.py` lists it for lemnosd when a
maintainer runs it after the driver merges. Chip drivers are `no_std` over `embedded-hal`. The
closest examples are `drivers/bmm150` (a magnetometer on I2C, with its power-up sequence in Rust) and
`drivers/ina2xx` (a power monitor with its chip layer in a separate module).

A driver implements `Lifecycle` (its `start`, `describe` and `health`) and its contract trait.
`#[driver("name", provides = [...])]` generates the bind function. Every driver follows the failure
rule: it never gives up, it reports the fault, and the next read tries again. Pacing between
attempts, capped at 8 s, is the host's job, because the host decides how often it reads. A driver
never switches the part off on its own.

One failure a driver can't get past by trying again: what it was bound with is dead, such as the
handle of a USB device that was unplugged. Its `health` then returns `Health::Gone(reason)`. The
host looks after every method and every tick, drops the driver and binds it again under the same
rule, which opens whatever is there now by its board name. Use `Health::Failed` for everything the
driver can recover from itself. A driver that holds something that can be unplugged should set
`TICKS` and look at it in `tick`: the host ticks it when the backend hints that its hardware may
have changed, so it finds out without waiting for a caller.

Tests run against the simulated bus (`lemnos-sim`), where the chip's registers are plain bytes that
a test sets and reads back. Steady-state reads must allocate nothing; prove it with the counting
allocator in `lemnos-measure`, as `drivers/generic-sensor` does.

## The package form

Not built yet. A compiled driver loaded through `lemnos-abi` will be documented with the PR that adds
the loader. Until then, a driver for a part that cannot be built in is a built-in driver in the
product's own repository, linked into that product's lemnosd.

## Checklist

- The driver's contract exists in `contracts/`. A driver provides only contracts it can serve.
- The driver is `no_std` unless the form needs the host (the `std` feature gates it).
- Settings are checked at bind and a bad one is `INVALID_ARGUMENT`.
- Each failure is reported through health, and a read after it tries again.
- Tests run on the simulated bus, and a test shows that a wrong register fails.
- Hardware you could not reach is marked `untested on board`, with the check written to
  `testing/board-runs/<id>.md`.
