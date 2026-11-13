# Writing a driver

A driver is the code that talks to one part and says what it provides. Lemnos does not know what a
part is: it matches tags, carries calls and values, and runs drivers under its failure rules
([Concepts](concepts.md)). There are three ways to write a driver. Pick the first that fits.

| Form | What you write | Use it when |
|---|---|---|
| **Description** | A text file that describes the chip's registers. No code. | A register-based magnetometer, accelerometer, gyroscope, temperature sensor or power monitor (`drivers/generic-sensor`). |
| **Built in** | A crate under `drivers/` with a `#[driver]` type. | The part needs code: a state machine, a block, a protocol. |
| **Package** | A compiled file that lemnosd loads through `lemnos-abi`. | A part that must be added to a device after the image is built. The driver is written exactly as a built-in one. |

The test for where a driver lives: it goes in Lemnos when a second, unrelated product would use it
unchanged. Which bus, address, pin or interrupt a part is wired to belongs in the board file, not in
the driver.

## The description form

Five contracts can be served from a description: `magnetometer/1`, `accelerometer/1`,
`gyroscope/1`, `temperature/1` and `power-monitor/1`, by the drivers `generic-magnetometer`,
`generic-accelerometer`, `generic-gyroscope`, `generic-temperature` and `generic-power-monitor`
in `drivers/generic-sensor`, for a chip on I2C, and their `modbus-*` twins for a Modbus RTU device
on a UART. Each contract's value names are fixed in code. The file gives only
the chip's facts.

A whole file, from `drivers/generic-sensor/tests/chips/sim-mag.chip`:

```text
# A made-up three-axis magnetometer for the simulated bus. It is not any real
# part, and its registers exist only as the simulated bus's bytes.

identity 0x00 0x48                      # the chip ID register reads 0x48

init 0x0a 0x80                          # soft reset
wait 2                                  # ms before the chip takes its next write
init 0x09 0x1d                          # continuous measurement

axis x 0x01 16 little signed 0.000001   # tesla per count
axis y 0x03 16 little signed 0.000001
axis z 0x05 24 little signed 0.000001
```

And a power monitor's, from `sim-power.chip` beside it, with 16-bit start-up writes:

```text
identity 0xfe 0x22                      # die ID
init 0x00 0x4127 16 big                 # configuration: averaging and continuous
init 0x0a 0x0200 16 big                 # calibration

value shunt_v 0x02 16 big signed 0.0000025      # V per count
value bus_v 0x04 16 big unsigned 0.00125        # V per count
value power_w 0x06 16 big unsigned 0.025        # W per count
value current_a 0x08 16 big signed 0.001        # A per count
```

The lines are:

- `identity <register> <value>`: optional. Checked before every start. A wrong value is reported as
  `NOT_SUPPORTED` and health is `Failed("identity mismatch")`, and the next read checks again.
- `init <register> <value> [<bits> <little|big>]`: a register write, run in file order when the
  chip starts. One byte, unless `bits` (8, 16, 24 or 32) and a byte order say wider.
- `wait <milliseconds>`: holds the start-up here. The driver answers `AGAIN` until the host's clock
  passes the time; nothing sleeps.
- `axis <x|y|z> <register> <bits> <little|big> <signed|unsigned> <scale> [shift <n>] [offset <o>]`:
  one axis of the vector, for the magnetometer, accelerometer and gyroscope. Each of x, y and z is
  needed once.
- `value <name> ...`, with the same words after the name: one value, for the others. Temperature
  takes `temperature` (°C); a power monitor takes `bus_v` and `shunt_v` (V), `current_a` (A) and
  `power_w` (W), each needed once.

A channel's number is `bits` wide (1 to 32) and sits `shift` bits above the bottom of the bytes
read at its register: a 12-bit count left-justified in two bytes is `12 ... shift 4`. As many bytes
are read as `bits` and `shift` fill, at most four. A signed number is two's complement in its
`bits`. Its value is `count * scale + offset`, in the contract's unit (tesla, m/s², rad/s, °C, V,
A, W).

Numbers are decimal or `0x` hex, `#` starts a comment, and there are at most 32 `init` and `wait`
lines. Registers are 8-bit addresses on I2C (Modbus below). An accelerometer's or gyroscope's batches need the chip's
FIFO, which a description does not cover, so `read_batch` answers `NOT_SUPPORTED`.

The chip's I2C address is wiring, so it goes in the board file. The description goes in the same
entry as one `description` setting: the text itself, in every build. A build with a filesystem (`std`)
also takes `description_file`, a path to the file, read once at bind; the two keys are never both
given, and a build without `std` refuses `description_file` by name. The key's meaning never depends on
the build: `description` is text everywhere and `description_file` is a path everywhere it works:

```toml
[[devices]]
id = "mag1"
driver = "generic-magnetometer"
bus = "<the bus the board names>"
config = { address = 0x2c, description_file = "/etc/lemnos/chips/sim-mag.chip" }
```

### A chip on Modbus

A sensor on a serial line that speaks Modbus RTU, as many RS-485 probes and meters do, is described
the same way. Its file starts with `modbus holding` or `modbus input`, the register table its values
are in. Registers are then the device's 16-bit registers: a channel of up to 16 bits reads one, a
wider one two, with `big` when the lower register holds the high half and `little` when it holds
the low half. An `init` writes one register; `identity` reads one. From
`drivers/generic-sensor/tests/chips/sim-modbus-temp.chip`:

```text
modbus input
identity 0x0000 0x5a01                  # device code
init 0x0200 0x0001                      # start measuring

value temperature 0x0001 16 big signed 0.1
```

Each `generic-*` driver has a twin for this, `modbus-magnetometer` to `modbus-power-monitor`, whose
one need is the UART, and `address` is the device's unit id (1 to 247). The
port's frame is wiring too, so it is set in the board file: `baud` (default 9600), `parity`
(`"none"`, `"even"` or `"odd"`, default `"even"`, Modbus's own default), `stop_bits` (1 or 2) and
`response_ms` (how long a reply may take, 1 to 500, default 100):

```toml
[[devices]]
id = "probe"
driver = "modbus-temperature"
bus = "<the port the board names>"
config = { address = 7, baud = 19200, parity = "none", description_file = "/etc/lemnos/chips/probe.chip" }
```

Each read is a request and its reply, checked for the unit, the function, the length and the CRC.
No reply is `TIMED_OUT`, a garbled one `IO`, and a Modbus exception (a register the device does
not have) `NOT_SUPPORTED`; the next read sets the frame and runs the start-up again. Reading a
Modbus device means sending requests, so one cannot be host-owned.

### Binding

Binding parses the file once into fixed arrays. A file with a bad line, or one written for another
contract, is refused at bind with `INVALID_ARGUMENT`; the parser's error names the line, and the
tests in `src/tests.rs` and `src/kinds.rs` check each refusal.

To prove a new file, put it under `tests/chips/` and bind it on the simulated bus the way
`src/kinds.rs` does, or on the simulated port with an answering device as `src/modbus_tests.rs`
does. Then break one register in the file and check that the test fails before you
restore it.

```sh slow
cargo test -q -p lemnos-driver-generic-sensor
```

A chip for another contract, or one that needs more than register reads (a FIFO, a conversion
command and a poll, values computed from calibration data), needs a built-in driver.

## The built-in form

A built-in driver is a crate in `drivers/<name>/`. Its `Cargo.toml` names its type under
`[package.metadata.lemnos]` (`driver = "Type"`, or a list for a crate with more than one, as
`drivers/pisp` has). `scripts/generate-builtins.py` then writes it into lemnosd's list of drivers
(`service/lemnosd/src/builtin.rs` and the service's `Cargo.toml`); with `--check` it only says
whether that list is current. Chip drivers are `no_std` over `embedded-hal`. The
closest examples are `drivers/bmm150` (a magnetometer on I2C, with its power-up sequence in Rust) and
`drivers/ina2xx` (a power monitor with its chip layer in a separate module).

A driver implements `Lifecycle` (its `start`, `describe` and `health`) and its contract trait.
`#[driver("name", provides = [...])]` generates the bind function. Every driver follows the failure
rule: it never gives up, it reports the fault, and the next read tries again. Pacing between
attempts, capped at 8 s, is the host's job, because the host decides how often it reads. A driver
never switches the part off on its own.

Health is what `health` returns when the host asks, and the host asks for every device, so a driver
does not report a change itself. The daemon looks at each device's health about four times a second
and tells clients of any change, whether or not the device's state changed too. A driver whose
health flaps is sent at most one event per look, so it can change health as often as its hardware
does. `health` must be cheap and must not block: it is called from outside the driver's own calls,
and while a call is running on the driver the host reports it as not yet looked at.

One failure a driver can't get past by trying again: what it was bound with is dead, such as the
handle of a USB device that was unplugged. Its `health` then returns `Health::Gone(reason)`. The
host looks after every method and every tick, drops the driver and binds it again under the same
rule, which opens whatever is there now by its board name. Use `Health::Failed` for everything the
driver can recover from itself. A driver that holds something that can be unplugged should set
`TICKS` and look at it in `tick`: the host ticks it when the backend hints that its hardware may
have changed, so it finds out without waiting for a caller.

A driver whose hardware raises a doorbell when there is work (a descriptor that becomes readable) returns it
from `wait_fd`, and the host waits on that descriptor instead of ticking on a deadline. When it becomes readable
the host ticks the driver at once. The host reads the descriptor to clear each wake before the tick runs, so the
tick finds nothing left to clear; a wake that comes while the tick runs is not lost and ticks the driver again.
The driver keeps the descriptor for as long as it runs. It still returns a deadline from `tick`: that is the
fallback for a wake that never came, and the rp1 receiver uses one frame period for it (`FALLBACK_NS`), counting
the frames a deadline tick finds (`lost_wakes`). `tick` must still not block: the host waits on the descriptor,
not the driver.

### Watching the board

A driver that needs to know the rest of the board sets `WATCHES_BOARD` and implements `board(view)`.
Before each tick the host hands it a `BoardView`: every device the board names, each as a
`BoardDevice` with its `id`, its `origin`, whether it is `bound` (its driver is running), and its
`health`. A dead device's `Health::Gone` is kept apart from `Health::Failed`. The origin is
`Origin::BuiltIn` for a device the board file names on a bus that stays put, and `Origin::Hotplug`
for one that comes and goes: one an `[[each]]` entry made, one on a USB port (the backend says which
buses come and go, `Provider::hotplug`), or one built on such a device (`uses`). A device from a
driver package has the origin its board entry gives it. Reading the view
allocates nothing, and the driver keeps what it needs from it in its own fields: the view is gone
after the call. A driver built into the host gets this; a driver package gets an empty view, and
a board-file list of strings (`health = ["device:x"]`) reaches a built-in driver through
`cfg.list("health")` (other arrays in a device's `config` are refused). A driver that watches the
board can't be built into a `lemnos-host-static` table: the table's slot refuses it when it is
built, because that host gives no view.

### Settings: `check_config`

A driver's settings are the `config` table its device gives in the board file. The driver declares
the keys it reads, with the kind each takes, and checks them in `check_config`. The check needs no
hardware and no bind:

```rust
const KEYS: &[Key] = &[
    Key::required("address", Kind::Int),
    Key::optional("device", Kind::Str),
];

fn check_config<'a>(cfg: &Config<'a>) -> Result<(), ConfigError<'a>> {
    cfg.check_keys(KEYS)?;
    // a range, a set of names, a combination: refused with `ConfigError::new(key, reason)`
    Ok(())
}
```

`check_keys` refuses a key the driver does not read (by name, with the nearest key it does read when
one is close), a key of the wrong kind, and a missing required key. The driver adds what it knows
only from its own settings: ranges and combinations. A `#[driver]` driver's generated bind runs
`check_config` before `start`, so a setting that is wrong never reaches `start`; a hand-written
`Driver` calls it itself at the top of `bind`.

`lemnos-ctl validate` runs the same check on the board file, through `lemnos_host::settings_refusals`,
and refuses a board with a setting a driver refuses, naming the device and the key. The daemon does
not refuse the board for it: the refused device is not made, shows as failed with the reason in its
state, and is retried under the failure rule, while the other devices run. A check that needs the
hardware, a file the board holds, or the state of the system is not in `check_config`: it stays in
`start`, and the driver says so in its documentation. A driver built outside this tree that does not
override `check_config` accepts every setting, as it did before.

### Long jobs

A method that runs longer than a call is marked `#[method(N, job)]` in its contract and returns
`Result<JobId>`. The driver starts the work, returns a number of its choosing, and goes on with the
work in `tick` (set `TICKS`). It sets `JOBS` and answers two more methods. `job(id, result)` says
how a job stands: `JobState::Running { done, total }` (in the contract's unit; `total` 0 when not
known yet), or `JobState::Finished(status)` with the result encoded into `result` on success. The
host asks after every call and tick of the driver, sends the starter what changed, and never asks
about a finished job again. `job_cancel(id)` stops a job; it then finishes as `Status::CANCELLED`.
The host cancels a caller's jobs when the caller goes. A job's result is at most 4 KiB, and a host
keeps at most 64 jobs open. A driver keeps no job across a restart: the host reports each job that
was open as interrupted, and a client starts it again if it wants it. `service/lemnosd/tests/jobs.rs`
has a whole driver that runs jobs.

A board file gives a device to the host with `owner = "host"` when the operating system's own
driver owns the hardware: Lemnos may read it and must not change it. The host refuses every method
such a device's description doesn't name as a value, and it stands between the driver and its
needs too: each need is handed over read-only (a write on a bus, a line set, a register write, a
job from a stream, a change asked of another device are all refused before they reach anything),
so what the driver does from `start` and `tick` changes nothing either. That only holds for
hardware reached through needs, so a driver says which kind it is with `Lifecycle::HOST_OWNED`:
`ThroughNeeds` when everything it does to hardware goes through its needs, `ReadsOnly` when it
opens files or nodes itself and, seeing `cfg.host_owned()`, opens them for reading only
(`drivers/hwmon-fan`). A driver that says nothing is never bound to a host-owned device; the
device shows as waiting, with that as its reason.

Reading most I2C chips means writing the register's address first (`write_read`). On a host-owned
device the I2C bus passes that, and nothing else that writes, when the driver declares how its
chips name a register with `Lifecycle::I2C_REGISTER_ADDRESS` (`Some(AddressWidth::Bits8)` or
`Bits16`): a transaction passes when every write in it is exactly one address of that width,
followed at once by a non-empty read. A register's value never fits (it travels in the same write
as the address), and a write with no read after it is refused. Declare it only for chips on which
an address write does nothing but move the register pointer; a chip that takes one-byte commands is
left at `None`, where only plain reads pass. The pointer is still the chip's state: an operating
system driver that sets it in one transfer and reads it in a later one can read the register
Lemnos named in between, while Lemnos's own address-and-read is one transfer the kernel never
splits. A driver that reads such a chip also looks at `cfg.host_owned()` and skips what it would
configure, reading what the owner set up instead (`drivers/bmm150`, `drivers/ina2xx`,
`drivers/generic-sensor`).

A driver with a stream need (`StreamHandle`) can ask what the stream's buffers are when it binds:
`buffer_kind()` answers `Plain` (shared memory, reached with `write` and `read`) or `Handles` (one
dma-buf per buffer, which hardware can be given), from the board file's `memory` key, before any
consumer attaches. A driver whose hardware takes buffers only by handle refuses a `Plain` stream
in `start` with `NOT_SUPPORTED`, as `drivers/g2d` does. The device then shows as failing to bind,
which points at the board file, where waiting for a consumer and going unhealthy would not.

A stream of dma-bufs may give each buffer in planes (the board file's `planes`), one dma-buf per
plane: `planes()` says how many, `plane_offset(plane)` where each starts in the buffer's bytes as
`write` and `read` address them, and `plane_handle(buffer, plane)` lends one. `buffer_handle` is
plane 0. A stream on standby (`standby = true`) has memory with no consumer attached, so the
handles are there to fill then too, and the newest frame published is the next consumer's first.

A driver that takes jobs on a stream holds the buffers its consumer names, and a stream's memory
is made anew for each consumer. `buffer_current(buffer)` says whether a held buffer was taken
under the consumer attached now. Ask it of one held buffer at the start of each tick, before
taking more: when it answers `false`, everything held and waiting was a consumer's that has gone,
so publish each buffer to give it up and start clean, as `drivers/usb-gadget` and `drivers/rga`
do. A consumer that posts bytes that are no job loses its stream with `fault_consumer()`.

Tests run against the simulated bus (`lemnos-sim`), where the chip's registers are plain bytes that
a test sets and reads back. Steady-state reads must allocate nothing; prove it with the counting
allocator in `lemnos-measure`, as `drivers/generic-sensor` does.

A driver for a hardware block follows the block rule in [the design](design.md#the-block-rule): no
vendor library (register facts may be transcribed from open-source vendor user-space code with provenance (source file and version); the libraries themselves are neither used nor linked.), and no doing the block's work in software. Its kernel interface (structures,
request numbers, the `unsafe` that issues them) goes in a crate of its own, `drivers/<name>-sys`,
and the driver crate has no `unsafe`. Every path by which a caller's bytes become an address, an
offset, a size or a descriptor the block will use is checked against a buffer the caller really
holds, with arithmetic that cannot overflow. If the kernel's interface gives no way to keep a job
to the caller's memory, the driver refuses the job and says why, as `drivers/vip9000` does.

```sh
python3 scripts/generate-builtins.py --check
```

## The package form

A package is a built-in driver compiled on its own into a shared object, which lemnosd loads when
it starts. The driver code is the same: a `#[driver]` type, `Lifecycle`, a contract. One line makes
the crate a package. The whole example is `engine/package/tests/fixture`, which the loader's tests
build and load.

`Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]          # a package is a shared object

[dependencies]
lemnos-core = { path = "...", features = ["std"] }

[profile.release]
panic = "unwind"                 # the default. Never "abort": see below.
```

`src/lib.rs`, cut down from the fixture:

```rust
use lemnos_core::{Config, Dev, I2c, Lifecycle, Result, contract, driver};

#[contract("example.package", version = 1)]
pub trait Example {
    #[method(1)]
    fn count(&mut self) -> Result<u32>;
    #[method(2)]
    fn add(&mut self, a: u32, b: u32) -> Result<u32>;
}

#[driver("package-fixture", provides = [Example])]
pub struct Fixture {
    #[needs(optional)]
    peer: Option<Dev<dyn Example>>,
    #[needs(optional)]
    bus: Option<I2c>,
    count: u32,
}

impl Lifecycle for Fixture {
    type Needs = FixtureNeeds;

    fn start(needs: FixtureNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            peer: needs.peer,
            bus: needs.bus,
            count: cfg.int("start").unwrap_or(0) as u32,
        })
    }
}

impl Example for Fixture {
    fn count(&mut self) -> Result<u32> {
        self.count += 1;
        Ok(self.count)
    }

    fn add(&mut self, a: u32, b: u32) -> Result<u32> {
        Ok(a + b)
    }
}

// The whole of what makes this crate a package.
lemnos_core::export_package!(Fixture);
```

`cargo build --release` makes `liblemnos_package_fixture.so`. The board file names the driver as it
names any other (`driver = "package-fixture"`), and tools see its contract and methods as they see a
built-in driver's.

### What the one line does

`export_package!` exports one data symbol, `lemnos_package`: a descriptor holding the package magic,
the version of the package boundary the crate was built against (with a hash of every layout that
crosses it) and a `register` function. The service reads the magic and version first. Only when
they match its own does it call `register`, which hands over the driver's manifest and a table of
functions: bind, describe, health, dispatch, tick, caller, caller_gone, drop, job and job_cancel. Each of them stops a
panic and returns it as a status. What the driver asks for when it binds comes back from the host as
handles, so the package links none of the host's types. The types are in `lemnos_abi::package`.

A package's driver can be given everything a built-in driver can: other devices
(`Dev<dyn Contract>`), I2C buses, SPI devices (`Spi`, and `RawSpiBus` with per-transfer settings),
GPIO lines with their edges, PWM channels, UARTs, register windows, interrupts, DMA memory, clocks,
resets, USB devices, USB functions and frame streams. The `#[needs]` fields are the same types and
the driver uses them through the same traits, so a driver crate that is built into the service
becomes a package with nothing changed but the export line. `engine/package/tests/ws2812-package`
is that: its whole source is

A field marked `#[link]` takes the device the board's `link` names, as `Option<Dev<dyn Contract>>`:
`None` when the board names no link. It is not a need: the manifest doesn't list it and no `uses`
entry names it. A linked device that isn't running fails the bind, which is retried, as a needed
device does; one with no such contract fails it too. The check a driver makes with it is the
driver's: the camera sensors ask their receiver's `source` (see [Drivers and contracts](drivers.md#contracts)).

```rust
lemnos_core::export_package!(lemnos_driver_ws2812::Ws2812);
```

and a test runs it beside the same driver built in and compares what each put on the bus.

Behind each resource is a handle the host holds, and each call on it crosses the boundary through
a small table of functions for its kind. Four things are worth knowing:

- Nothing in a steady-state call allocates, on either side.
- A resource's error arrives as the kind it was (`Timeout`, `Busy`, `Unavailable`, `Unsupported`
  and the rest), so a driver that tells them apart still can.
- One I2C or SPI transaction carries at most 16 steps, and one isochronous USB transfer at most
  64 packets. More is `InvalidInput` before anything reaches the bus.
- A stream buffer's handle (`StreamEndpoint::buffer_handle`, and `plane_handle`) crosses as the number of a file
  descriptor in the service's process, which is the package's process too. It is borrowed until
  the stream's next call, as it is for a built-in driver: duplicate it, or hand it to the kernel,
  to hold the memory longer.

A package is built against one version of the Lemnos crates and runs only in a service built with
the same package boundary: the same major version and the same layout hash. Anything else is
refused with the reason, before any of its code runs.

### Loading

Packages are off unless lemnosd is given a directory:

```text
lemnosd --packages /usr/lib/lemnos/packages --package-keys /etc/lemnos/package-keys
lemnosd --packages /usr/lib/lemnos/packages --package-mode open
```

`LEMNOSD_PACKAGES`, `LEMNOSD_PACKAGE_KEYS` and `LEMNOSD_PACKAGE_MODE` say the same from the
environment. With no directory, nothing is scanned and nothing is loaded. With one, lemnosd loads
every file in it whose name ends in `.so`, in order of name, and says on its error output what it
loaded and what it refused and why. A file that is refused never stops another, or the service.

`lemnos-ctl packages` asks a running service the same thing:

```text
$ lemnos-ctl packages
packages signed: only packages signed by a trusted key run
/usr/lib/lemnos/packages/a-driver.so  loaded   some-driver
/usr/lib/lemnos/packages/b-other.so   refused  bad signature: not signed by a trusted key, or changed since
/usr/lib/lemnos/packages/c-notes.so   refused  unsigned: signed mode, and it has no signature file
```

The first line is how packages are run (`off`, `signed` or `open`). If the directory could not be
used at all, the next line says why.

A statically linked lemnosd can't load packages. A program linked statically against its C
library, which is what a musl target builds unless told otherwise, has no dynamic loader in it.
Such a service looks at no file, says once that it is statically linked and that packages need a
dynamically linked build, and runs its built-in drivers as ever. `lemnos-ctl packages` shows the
same line. Build lemnosd for a glibc target, or for musl with `-C target-feature=-crt-static`, to
use packages.

A driver's name is taken once. A built-in driver always keeps its name: a package with the same
name is refused. Between two packages the first by file name wins.

`--package-mode` is the one switch:

- **signed** (the default): a package runs only if `<file>.sig` beside it is an Ed25519 signature
  of the whole package file, 64 raw bytes, by a key in the key file. The key file has one public key
  per line as 64 hex digits; blank lines and lines starting with `#` are skipped. No key file, no
  signature or a wrong one: refused.
- **open**: any package in the directory runs.

The file is read once, into memory that can't be changed afterwards. The signature is checked over
those bytes and the same bytes are what gets loaded, so a file swapped after the check changes
nothing.

Lemnos loads no vendor library. The loader is for Lemnos driver packages only: a file that is not a
whole shared object for this machine exporting `lemnos_package` with the right magic and version is
refused without any of it being run or mapped. A package must not load a vendor library either: the
block rule in `docs/design.md` holds for a package's driver as for any other.

### Building, signing and loading one

The whole path, with the fixture package and OpenSSL 3. Build the package, on its own, and put it
in a directory:

```sh slow
cargo build --manifest-path engine/package/tests/fixture/Cargo.toml --target-dir "$TMPDIR/fixture"
mkdir "$TMPDIR/packages"
cp "$TMPDIR/fixture/debug/liblemnos_package_fixture.so" "$TMPDIR/packages/"
```

Make a key, add its public half to the key file, and sign the package:

```sh slow
openssl genpkey -algorithm ed25519 -out "$TMPDIR/key.pem"
openssl pkey -in "$TMPDIR/key.pem" -pubout -outform DER | tail -c 32 | xxd -p -c 32 >> "$TMPDIR/package-keys"
openssl pkeyutl -sign -inkey "$TMPDIR/key.pem" -rawin -in "$TMPDIR/packages/liblemnos_package_fixture.so" -out "$TMPDIR/packages/liblemnos_package_fixture.so.sig"
```

Start the daemon with the directory and the keys, on a board file that names the package's
driver (`docs/examples/package-board.toml`):

```sh slow
export LEMNOSD_SOCKET="${TMPDIR:-/tmp}/lemnosd.sock"
lemnosd --sim --board docs/examples/package-board.toml --packages "$TMPDIR/packages" --package-keys "$TMPDIR/package-keys" &
```

The package is loaded, its device runs, and its contract is called like any other:

```sh slow
lemnos-ctl packages
lemnos-ctl list
lemnos-ctl describe thing
lemnos-ctl call thing add 2 3
kill %1
wait
```

### A package in C

A package need not be Rust. `include/lemnos/package.h` is the whole boundary as a C header: the
descriptor, the manifest, the table of functions the service calls on the driver and the tables
the driver calls on the service, one for each kind of resource. It is checked against the Rust
types by a test that compiles it and compares every size, offset and value, and
`engine/package/tests/c-package/package.c` is a small driver written against it that the loader's
tests build with the C compiler, load and run. Its rules are at the top of the header: nothing
unwinds across the boundary, a pointer and its length are what they say, and what `register_driver`
hands over stays valid while the package is loaded.

### What a package can break

A package runs inside lemnosd's process.

- **Panics and hangs are contained**, by the same code that contains a built-in driver's. A panic in
  bind or in a method comes back as a failure, the driver is dropped, and the host binds it again
  with the usual backoff, capped at 8 s. A call that does not return is given up on at the call
  deadline, and the device is retried once the stuck call lets go. Other devices carry on.
- **Memory corruption is not contained, and can't be.** A package that writes where it should not
  can break anything in the service. Signed mode says who built a package, not that it is correct.
- **`panic = "abort"` takes the service down.** A package must be built to unwind.
- **A package is never unloaded.** Replacing one means restarting lemnosd.

### Tests

Test the driver as a built-in driver is tested, on the simulated bus: it is the same type. To test
it as a package, do what `engine/package/tests` does: build the `cdylib`, load it with
`lemnos_package::Loader` and run it under `lemnos_host::Host`. `tests/resources.rs` there uses
every kind of resource from a package against the simulated board, and `tests/merged_driver.rs`
runs one driver both ways.

## What a driver describes

A device's description is what a tool with no code for it shows. Lemnos fills in the name, the
kind, the bus and the state itself, from the board file and the host; the driver fills in what only
it knows, one `Item` at a time (`engine/core/src/description.rs`):

| Field | Set by | Unknown when |
|---|---|---|
| Label | the board file's `label`, else the device id | never (the id is always there) |
| Kind | the contract crate, `kind = "..."` on `#[contract]` | the contract says `other` |
| Bus | the board file's `bus` | the device has none |
| Address | `out.set_address(addr)`: the 7-bit I2C address | the driver serves several addresses, or none |
| State and reason | the host | never |
| Unit | `Item::…().unit(Unit, exponent)` | `One` (dimensionless) is the default |
| Range | `.range(min, max)`, raw, in the item's exponent | left out: no range known |
| Step | `.step(n)`: one count, in raw units | left out |
| Limit | `.warn_below(raw)`, `.warn_above(raw)` | left out: no threshold |
| Reading | `.reading(Reading::…)`, else the method's, else the shape's | never: the shape always suggests one |
| Group | `.group("name")` on each part of a vector, with the axis in `.field(...)` | not a vector |

The readings are `number`, `range` (a settable number within its bounds), `switch`, `text`,
`colour array` and `three axes`. A contract declares a method's reading with
`#[method(N, reading = switch)]`, and the kind with `#[contract("tag", version = 1, kind = "sensor")]`
(`sensor`, `actuator`, `camera`, `power`, `system`, `light`, `bus`, `compute` or `other`). A control's
`method` is the one that sets it; a value's is the one that reads it. Neither the kind nor the
reading is part of a contract's layout hash: both are for description only.

A driver fills in a range only when it knows one: a sensor's full scale from its configured range,
a fan's duty from its per-mille limits. Where it does not know, it leaves the item without one. An
unknown figure is never a guess. A float value's range, which no integer scale states exactly, is
given with `real_range(min, max)` and `real_step(step)` in the item's unit.

Every item declares its unit state. `Item::unit(unit, exponent)` with a unit is known, and `One` (or
`.dimensionless()`) is dimensionless; a count, a switch, a text and an action are dimensionless. An
item that declares none is unknown, so a driver that does not set it is visible in the panel.

The wire carries these as an appended trailer on the description ([Wire](wire.md#description)).
A driver package carries them too. Its items' reading, step, warning limits and group, its contracts'
kinds and its methods' readings cross in the package's `PackageExtra` table, which is a separate table
so that the item table keeps its size. Each of them is given by the same declarations as a built-in
driver's, so a packaged driver describes what the same driver built in describes. An item's unit
state is in the table's flags (`EXTRA_HAS_UNIT_STATE`, with `EXTRA_UNIT_DIMENSIONLESS` and
`EXTRA_UNIT_UNKNOWN` for the two other states; neither set is known). A package built before the
flag decides by its unit string, as a host before the state did. A package carries no real-valued
range: its float items keep the integer one only.

A package built before that table ([the package form](#the-package-form)) loads with the
extras absent: its contracts are `other`, and its items have no reading, step, limit or group, but
keep their own range and address. A package built against a boundary this host does not know is
refused with the layout mismatch, before any of its code runs.

## Compatibility: what must match, and what need not

> **Status:** T1d has landed. The core and the wire parts are checked at connect (T1c). Each
> contract's base shape, its additions and `#[breaking]` are checked when a typed client binds it
> (T1d, below). The ABI tables of a driver package and the package guarantee follow in T1e and T1f.
> Until T1f lands, a driver package built against another contract or ABI revision is refused.

Nothing unreleased is versioned. Compatibility is structural: each unit of the interface has its own
hash, and a client, a driver package or the daemon need agree only on the units they actually use.
A minor Lemnos change does not rebuild every driver and client.

The units, and what each hash covers:

- **The core**: the handshake, framing, the call and reply envelopes, the event envelope, stream and
  job framing, and the description's framing. One hash, kept small. A connection refused on it says
  `core protocol differs`, with both hashes.
- **Each contract**: its tag, and for each base method its number, name, parameter and result
  shapes, and whether it fills a buffer or starts a job. Checked once, when a typed client binds that
  contract. A change to one contract never affects a client of another.
- **Each ABI table** of a driver package: its layout, checked when the package is loaded.
- **Each optional part** of the wire (stream handles, packages, jobs), negotiated by the feature
  bits, with its own hash where its layout can change.

### Base shape and additions

Every contract has a **base shape**: its tag and its base methods. A method or a struct field that
was appended after the first form of its contract is an **addition**, and it is marked:

- `#[added]` on an appended method of a `#[contract]` trait. It must come after every base method.
- `#[added]` on an appended field of a `#[derive(Encode, Decode)]` struct. It must come after every
  base field. A field of type `Tail<T>` is an addition by its type.
- `#[breaking]` with `#[added]` makes the item base again: a change that moves the old meaning of an
  existing field or method is breaking even when it is appended. The base then moves, and both sides
  are refused at bind.
- An item with no mark is base. Forgetting a mark moves the base, which is the safe error.

A contract's **base hash** covers its tag and its base methods only. Each method has its own
**method hash**: its number, name, base parameters and base result, and its flags. An added field
has its own hash, from its name and shape under the parameter or result it sits in. An addition does
not move the base hash, and a method whose argument struct gained a field keeps its method hash.

A typed client binds a contract by one rule that gives the same guarantee as an additions level on
the wire, with a precise message: the device's base hash must equal the client's base hash, and for
every method the client has, base or added, the device must have a method with the same number, name
and method hash, and must have every added field the client's method uses. Extra methods and fields
on the device are ignored. The additions level (how many methods are added) is kept for diagnostics
and the C API (`lemnos_contract_hash`), not compared at bind. [Client](client.md#the-typed-bind)
gives the refusal texts.

Structs carry their length. A reader skips the trailing fields it does not know, and a reader that
expects more fields than it received fills in the declared defaults. A method the device's driver
does not provide answers `NOT_SUPPORTED`, and its refusal names the method. The daemon's texts are
`contract holder/1 has no method 99` for a number the contract does not have, and
``the device's driver does not support method 12 `identity` of camera.sensor/1`` for a method the
contract has and the driver answers `NOT_SUPPORTED` to.

### Additive and breaking changes

A change is **additive** when an old reader still reads everything it needs correctly, given the
rules below. It is **breaking** when it changes what an existing field or method means. An author
marks a breaking change with `#[breaking]` on the field or method. A breaking change moves the base
shape, so the old and new sides are refused at bind, and the refusal names the contract.

Additive changes, by construction:

- a method appended at the end of a contract (a new number, after every existing one);
- a field appended at the end of a struct, when the field's absence leaves the meaning of the other
  fields unchanged (an absent `Option` is `None`, an absent flag is `false`);
- a new frame kind, a new feature bit, a function pointer appended to an ABI table.

Structural compatibility is not semantic compatibility. Appending a field can still change what the
existing fields mean. For example, a `gain law` added beside gain codes makes an old reader misread
the codes. Mark such an addition `#[breaking]`.

### Classifying a change

When you add a method or field, classify it in the PR:

- **Additive**: a method at the end, or a field whose absence means the same as before.
- **Breaking** (`#[breaking]`): anything whose old meaning changes without the new field. The gain law
  above is one.

### Examples from this branch

| Change | Class | Reason |
|---|---|---|
| `camera.sensor` `GainRange.law` | breaking (and marked `#[added] #[breaking]`) | without it, gain codes have one meaning (linear); an old reader would misread a reciprocal or decibel sensor's codes |
| `camera.sensor` `SensorInfo.embedded_data` | additive | absent means `None`, which is what a sensor from before it did |
| `camera.receiver` methods 9 to 11 (`embedded_stream`, `source`, `layout`) | additive | appended methods; an old driver answers `NOT_SUPPORTED` |
| `camera.sensor` method 12 (`identity`) | additive | appended method |
| `camera.sensor` methods 13 and 14 (`post_values_at`, `applied_landing`), `camera.receiver` method 12 (`frame_starts`) | additive | appended methods and their types; an old driver answers `NOT_SUPPORTED` and an old caller never calls them |
| `camera.receiver` methods 13 and 14 (`inputs`, `input`) and the `Input` record | additive | appended methods and a record layout a caller reads with `read_input`; an old driver answers `NOT_SUPPORTED`, and a receiver without a V4L2 node always does |
| `camera.sensor` methods 15 to 18 (`in_force`, `set_exposure`, `set_analogue_gain`, `set_frame_length`) and `InForce` | additive | appended methods and their type; the three setters are posts, so a driver without a landing answers `NOT_SUPPORTED` as `post_values_at` does |
| `Unit::Line` (`"line"`) | additive | a new unit symbol on the description's unit string; an item with it reads as unit-less to a client that does not know it |
| `camera.isp` methods 13 and 14 (`input_layout`, `output_layout`) | additive | appended methods, marked `#[added]` in T1d (U1b added them without the mark) |
| `lens/1` methods 4 and 5 (`direction`, `settle_us`) | additive | appended methods |
| `raw-bus` methods 6 and 7 (`uart_write`, `uart_read`) | additive | appended methods |
| `camera.sensor` `SensorInfo` delay fields (`Mode.exposure_delay`, `gain_delay`, `frame_length_delay`) | not a change | no delay is redefined: `SensorInfo` has none, and the three delays on `Mode` have the same type, place and timing rule as at T1b |
| `camera.receiver` `layout` (method 11) against the stream's buffer length | additive | the method is appended, and its meaning did not change: `FrameLayout.size` is the bytes per frame at most, as at T1b, and the stream's buffer length is the stream's own, unchanged |
| `FrameStream::bytes()` on a multi-plane stream | not a change | it returns plane 0, as at T1b (same code, same doc); the other planes are read with `plane` |
| The stateless-decode stamp, a whole number of microseconds (`drivers/m2m-decode`) | not a change | the rule and its check are unchanged since T1b; the T1c change to `codec-block` is to byte sizes only |
| `MethodInfo.job` on a new method | additive | a method that was not a job before is not called differently by a client that doesn't know it is one |
| `MethodInfo.job` set on an existing method | breaking | a client that calls it as a call would now get `NOT_SUPPORTED` |
| `system.identity/1` (new contract) | additive | a new contract is only bound by a client that asks for it |

### Hashes and versions

Each unit's hash is computed at compile time from its declarations, and nothing writes one by hand.
The `#[contract]` macro computes each contract's base hash and each method's hash, and the derives
compute each struct's shape. The device's description carries each contract's base hash, each
method's hash and whether it is an addition, and the hash of each added field; see
[the wire's description](wire.md#description).

Nothing is versioned while unreleased: a contract's `version` stays 1. An additive change leaves the
base hash unchanged; a breaking change moves it.

Doc comments, method bodies and the driver's own state don't move any hash.

## Checklist

- The driver's contract exists in `contracts/`. A driver provides only contracts it can serve.
- The driver is `no_std` unless the form needs the host (the `std` feature gates it).
- Settings are checked by `check_config` (see [Settings](#settings-check_config)): the host runs it when it
  validates the board file and before it makes the device, and the generated bind runs it again.
- Each failure is reported through health, and a read after it tries again.
- Tests run on the simulated bus, and a test shows that a wrong register fails.
- Hardware you could not reach is marked `untested on board` in the crate's documentation, with
  the check written to a file under `testing/board-runs/` ([Testing](testing.md#board-runs)).
- The driver has a row in [Drivers and contracts](drivers.md) that says what state it is in.
- Every value it describes has a unit, and a range, a step and a limit wherever it knows them. A
  test on the simulated bus shows the description has no item without a reading type.
