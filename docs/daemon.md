# The daemon and lemnos-ctl

`lemnosd` owns the hardware. It reads a board file, binds a driver to each device, keeps them
running, and serves every program over one Unix socket. `lemnos-ctl` is the command-line client.
It has no code for any device: everything it shows comes from the device's own description.

## lemnosd

```text
lemnosd [--sim] [--board PATH] [--socket PATH] [--check]
        [--packages DIR] [--package-keys FILE] [--package-mode signed|open]
        [--user USER] [--group GROUP]
```

| Flag | Environment | Default | Meaning |
|---|---|---|---|
| `--board PATH` | `LEMNOSD_BOARD` | `/etc/lemnos/board.toml` | The [board file](boards.md) |
| `--socket PATH` | `LEMNOSD_SOCKET` | `/run/lemnos/lemnosd.sock` | Where it listens. The directory is made if it is missing. The socket is mode 0660 |
| `--sim` | | off | Use the simulated board in place of this machine's hardware |
| `--check` | | off | Parse the board file and exit: 0 when it is well formed |
| `--packages DIR` | `LEMNOSD_PACKAGES` | none | Load driver packages from this directory |
| `--package-keys FILE` | `LEMNOSD_PACKAGE_KEYS` | none | The public keys packages must be signed with |
| `--package-mode` | `LEMNOSD_PACKAGE_MODE` | `signed` | `signed`, or `open` to run any package |
| `--user USER` | `LEMNOSD_USER` | none | Start as root, run the privileged start, then drop to this user (a name or a number). See [the root phase](#the-root-phase) |
| `--group GROUP` | `LEMNOSD_GROUP` | the user's own | The group to drop to, with `--user` (a name or a number) |

A flag wins over its environment variable. Packages are described in
[Writing a driver](writing-a-driver.md#the-package-form).

Start it on the simulated board:

```sh
export LEMNOSD_SOCKET="${TMPDIR:-/tmp}/lemnosd.sock"
lemnosd --sim --board docs/examples/sim-board.toml &
```

How it treats its drivers is the board file's `[host]` section: the call deadline and the waits
between retries, which default to 2 s and 1 s doubling to 8 s. The 8 s cap can be shortened and
never lengthened. Nothing is turned off automatically unless a device's own entry opts in with
`auto_off_after`. See [Board files](boards.md#the-keys).

What it does while it runs:

- **Binds every device**, the ones others use first. A device that cannot bind stays listed, with
  its reason, and is retried under [the failure rule](concepts.md#the-failure-rule). The daemon
  starts whatever the board file says: a board with nothing reachable still starts.
- **Uses the kernel module when it is loaded.** Without `--sim` it opens `/dev/lemnos` if the node
  is there, and says on its error output if it could not. See [The kernel module](kernel-module.md).
- **Listens for hotplug.** Without `--sim` it watches the kernel's events for the names in the
  board file and retries the devices waiting on them. If it cannot watch, devices still bind when
  their retries come round.
- **Ends everything a connection held when the connection closes**: subscriptions, streams and
  whatever a driver was holding for that caller.
- **Stops on `SIGTERM` or `SIGINT`.** Stopping drops every driver. Each one leaves its hardware in
  its safe state (a fan at full duty), and the hardware watchdog then resets the board if the stop
  is not followed by a start.

It writes nothing to its standard output. Its error output gets what could not be used, what
became of each driver package, and a driver's panic message.

The drivers compiled in are listed in `service/lemnosd/src/builtin.rs`, which
`scripts/generate-builtins.py` writes from the driver crates. The `fake-devices` feature adds the
four fake drivers the simulated board uses.

The shipped form is a small static binary:

```sh slow
cargo build -p lemnosd -p lemnos-ctl --profile service
```

For a board, add `--target aarch64-unknown-linux-musl` (after `rustup target add` for it). A
statically linked `lemnosd` cannot load driver packages and says so; build for a glibc target to
use them.

## The root phase

Started as root with `--user` (or `LEMNOSD_USER`), lemnosd runs its privileged start, then drops to that
user and group, keeps only `CAP_SYS_RAWIO`, and verifies the result before it listens. A failed step, a
failed drop, or a state after the drop that is not the one asked for stops it, with the reason on its
error output. Started without root, it runs as it is and says which capabilities it lacks. The
privileged start is the list `STEPS` in `service/lemnosd/src/privilege.rs`: each step runs as root, with
the board file and the socket and ids the daemon will run as, and a step added there (the USB gadget's
configfs setup is planned as the first) runs before the drop. No unit or script gives the daemon a user, or a
file capability. The steps and the drop are described in
[`packaging/README.md`](../packaging/README.md#one-privilege-path).

## Running under systemd

`packaging/systemd/lemnosd.service` starts the daemon as root and runs it with `--user lemnos --group
lemnos`, so it drops to `lemnos` itself. The device nodes are given to group `lemnos` by
`packaging/udev/70-lemnos.rules`. Its one kept capability is `CAP_SYS_RAWIO`, for register mapping
through `/dev/lemnos`. The unit starts only when the OS has
supplied the board file at `/etc/lemnos/board.toml`. The OS installs the unit files and supplies the
`lemnosd` binary and the board file. The unit restarts the daemon after a constant 1 s and sets no
`WatchdogSec=`: the hardware watchdog escalates, as it does without systemd. [`packaging/README.md`](../packaging/README.md) has the install
table, what each unit needs and what it does not. The PWM and USB gadget drivers write sysfs and
configfs, which only root may do, so a board that uses them needs a drop-in that runs the daemon as root.

## Running under BusyBox init

On a board without systemd, `packaging/init/S50lemnosd` starts the same root phase through `start`,
`status`, `restart` and `stop`: its supervisor runs `lemnosd --user lemnos --group lemnos`. A crash
restarts it after a constant 1 s. `stop` is for shutdown, and sends TERM, then KILL after 10 s. There is
no file capability and no `setcap`. The hardware watchdog is the escalation on
both init systems: the daemon feeds it, and the OS must not give it to systemd. [`packaging/README.md`](../packaging/README.md)
has the user's passwd and group lines, the mdev rules, and what the unit gives that the script does not.

## If the daemon dies

`lemnosd` is not allowed to go down. In this stack it plays the part of the kernel, so nothing is
designed around the daemon being stopped or dead. If it dies, the device is dead: the daemon is
restarted at once, and if it is not healthy again within a short bound, the board's hardware
watchdog resets the board.

- **Restart at once.** The unit restarts the daemon one second after it exits (`RestartSec=1s`),
  and does not hold the restart back. A driver that panics or hangs is never a reason to restart:
  it is contained on its own lane (see [Drivers](drivers.md)).
- **The bound.** The board's watchdog is set by its `hw-watchdog` device, to `timeout_s` seconds
  (15 by default, clamped to the range the device reports, and read back). Feeding stops whenever
  the daemon is not healthy, so the board resets `timeout_s` seconds after the last feed.
- **Healthy** means both of these hold. The main loop finished a pass within 10 s. The control
  socket answered a handshake within 5 s; a probe asks it every second, and an answer comes from
  the serving thread, so a wedged socket counts. A driver that panics or hangs does not make the
  daemon unhealthy: the host ticks the watchdog on its own lane, so the feed goes on.
- **A deliberate restart.** A restart is inside the bound only if the new daemon's socket answers
  before the timeout runs out. The board check measures it (see
  [`testing/board-runs/S1b.md`](../testing/board-runs/S1b.md), check 4); no figure is claimed here.
- **Clean shutdown.** A reboot or power-off stops the feeding and leaves the watchdog armed. The
  daemon never writes the magic close, so a shutdown that hangs still resets the board. Stopping
  the daemon without a restart also resets the board once the timeout runs out; that is the rule.

The board file names the watchdog as the daemon's keep-alive:

```toml
[[devices]]
id = "watchdog"
driver = "hw-watchdog"
keepalive = true               # ticked only while the daemon is healthy
config = { device = "watchdog0", timeout_s = 15 }
```

## lemnos-ctl

```text
lemnos-ctl [--socket PATH] COMMAND
```

The socket is `--socket`, else `LEMNOSD_SOCKET`, else `/run/lemnos/lemnosd.sock`. A command exits 0
on success. On failure it prints the reason on its error output and exits 1.

`lemnos-ctl` is untyped by design. It has no contract traits: it reads each device's description
and calls methods by number and by name, with arguments and replies as text it encodes and decodes
from the description. So it does not run the typed bind check, and it need not be built from the
daemon's Lemnos revision. A method the device's driver does not provide is refused with the method
named, as any caller is (see [the client's typed bind](client.md#the-typed-bind)).

### list

Every device and how it stands: name, driver, state, health, tags, then `-> NAME` when the board
file links it to another device, then the reason in brackets when there is one.

```sh
lemnos-ctl list
```

### chain DEVICE

The devices a device leads to through `link`, in order: a camera's sensor, its receiver, its ISP.
A last line says when the chain ends at a name the board does not have, or comes back on itself.

```sh
lemnos-ctl chain counter
```

### describe DEVICE

What the device provides: each contract with its methods and their types, then each item (a
`value`, `control` or `action`) with its unit and range.

```sh
lemnos-ctl describe counter
lemnos-ctl describe orientation
```

### show DEVICE

The device as a panel, from its description alone: its label, kind, bus, I2C address (`unknown`
when the driver names none), state and reason, which contracts it provides, then each value,
control and action with its unit, range, step, warning limit, reading type and vector group. A figure
the device does not give reads `unknown`. The tool holds no code for any kind of device.

```sh
lemnos-ctl show accel
```

### read DEVICE [ITEM]

Reads one value, or every value the device has. A value is printed in its unit.

```sh
lemnos-ctl read accel
lemnos-ctl read counter count
```

### watch DEVICE ITEM [--period-ms N] [--count N]

Subscribes to a value and prints each one as it arrives. The period is 100 ms unless given. It runs
until stopped unless `--count` says how many to print.

```sh
lemnos-ctl watch accel z --period-ms 50 --count 3
```

### set DEVICE ITEM [VALUE...]

Sets a control or runs an action. A value item is refused as read-only.

```sh
lemnos-ctl set counter target 7
lemnos-ctl read counter count
```

### call DEVICE METHOD [ARG...]

Calls any method by name, whether or not an item names it. When two contracts on the device have a
method of the same name, write `TAG:NAME`. The reply is printed unless the method returns nothing.
A long job (`describe` shows it as `-> job`) prints its number, then its progress as it comes, then
its result in hex when it ends. Interrupting the tool ends the connection, which cancels the job.

```sh
lemnos-ctl call counter peek
lemnos-ctl call counter example.counter/1:set 5
```

Arguments for `set` and `call` are words, in the order `describe` shows the parameters. A number is
written as a number, a bool as `true`, `false`, `on`, `off`, `1` or `0`, bytes as hex, and an
optional value that is absent as `none`. A record or an array takes one word per field or element,
in order.

**Out buffers.** A method that fills a caller's buffer (`#[out]`, shown by `describe`) takes one
more word after its arguments: the buffer's size in bytes. The word is optional. Without it the size
is the largest a frame carries, 1048544 bytes, so `lemnos-ctl call slots list` needs no size. Give a
smaller size to bound the reply, or a larger one (up to 1048544) when the reply does not fit:

```sh needs:a-board-with-a-slots-device
lemnos-ctl call slots list 65536
```

A reply that does not fit the size is refused with `BUFFER_TOO_SMALL` and the reason says how to ask
for more.

**Largest replies.** The bridge carries a reply of at most 61440 bytes (`ORION_REPLY_MAX`). This is
each out-buffer method's largest reply, from its contract's own bounds:

| contract | method | largest reply | through the bridge |
|---|---|---|---|
| `accelerometer/1`, `gyroscope/1` | `read_batch` | the caller's buffer | whole; samples that do not fit wait for the next batch |
| `camera-frames/1` | `frame_stream` | 32 bytes (`MAX_STREAM_NAME`) | whole |
| `camera-isp/1` | `config_stream`, `statistics_stream`, `input_stream`, `output_stream` | 32 bytes each | whole |
| `camera-receiver/1` | `frame_stream`, `embedded_stream` | 32 bytes each | whole |
| `camera-receiver/1` | `feeds`, `source` | 64 bytes (`MAX_FEED_NAME`) | whole |
| `camera-sensor/1` | `identity` | 64 bytes (`MAX_IDENTITY_NAME`) | whole |
| `codec-block/1` | `block` | 48 bytes (`MAX_BLOCK_NAME`) | whole |
| `codec-block/1`, `image-blend/1`, `image-convert/1`, `image-scaler/1`, `npu/1` | `job_stream` | 32 bytes (`MAX_STREAM_NAME`) | whole |
| `fan-control/1` | `curve` | 64 bytes (8 points of 8) | whole |
| `gpu-lease/1` | `last_hang` | 128 bytes (`DETAIL_MAX`) | whole |
| `system-identity/1` | `serial`, `family`, `model` | 256 bytes (`MAX_TEXT`) | whole |
| `system-identity/1` | `facts` | 2048 bytes (`MAX_FACTS_TEXT`) | whole |
| `system-slots/1` | `list` | unbounded: 1 slot is about 350 bytes, so about 170 fit | refused by name over about 170 slots; paged form in M8b |
| `raw-bus/1` | `i2c_transfer`, `spi_transfer` (`read_len`), `uart_read` (`max_len`) | the caller's length, up to 4 GiB | a length above 61440 is refused before the transfer; ask for less and repeat |
| `usb-function/1` | `control_out` | the pending request's `length`, up to 65535 | not checkable up front (no length argument): the bridge refuses a reply too large for its buffer, and the device must refuse before it accepts the request (not yet verified in the USB driver) |
| `usb-function/1` | `data_stream` | 32 bytes (`MAX_STREAM_NAME`) | whole |

A length the caller gives (`read_len`, `max_len`) is checked before the call is made, so a bus
transfer with a length too large for the reply is never started. `lemnos-ctl`, on the board's own
socket, carries a 1 MiB frame, so the same calls go through there up to that size.

**Watch.** `lemnos-ctl watch` takes a method with no arguments and no out buffer. The daemon serves a
subscription only to such a method, so `watch` on a method with arguments or an out buffer is refused
by name before it is asked: the method takes arguments or an out buffer, so call it instead.

 A reply is printed as text when its bytes are valid UTF-8 (JSON, as `system.slots` writes
it, prints as JSON), and as hex otherwise. The rule is the same for `lemnos-ctl` and the Orion
bridge's `.call` action, and is implemented once, in `lemnos_wire::text`.

### off DEVICE, on DEVICE

Turns a device off, and on again. Off drops the driver and stops the retries. The device stays
listed as `off` with the reason `manual`. On binds it again. A device the board file marks
`disabled = true` starts off, and `on` starts it.

```sh
lemnos-ctl off counter
lemnos-ctl list
lemnos-ctl on counter
```

### packages

How the daemon runs driver packages (`off`, `signed` or `open`), then each file it looked at: the
driver a loaded one became, or why a refused one was refused.

```sh
lemnos-ctl packages
```

### files

The named files a node serves (`lemnos/`, "Named files" above). With a device only, its index. With
a name, that file: the snapshot is taken once and read back in pieces, so the bytes are what the node
held when the transfer began. The file goes to stdout, or to a new file given as the last argument; the
tool never overwrites one.

```sh needs:a-daemon-with-a-named-files-device
lemnos-ctl files files                                    # the index
lemnos-ctl files files lemnos/trial-reason                # to stdout
lemnos-ctl files files lemnos/tree/overlay.dts ./overlay.dts
```

### validate

Checks a board file the way `lemnosd --check` does, with no daemon and no socket, then prints what
the host would run with: the `[host]` settings with the defaults filled in, and the devices that
opted in to auto-off. A file that is refused exits 1 with the key and the reason, so a `[host]`
value past its range, a `retry_cap_ms` over the failure rule's 8 s among them, never reaches a
running daemon. The keys are in [Board files](boards.md#the-keys).

```sh
lemnos-ctl validate docs/examples/sim-board.toml
lemnos-ctl validate docs/examples/full-board.toml
```

### When a command is refused

A refusal carries a status and a few words: `refused (-16): ...`. The numbers are negative Linux
error numbers, and Lemnos's own codes below -4095:

| Status | Name | Usual meaning |
|---|---|---|
| -1 | `NOT_PERMITTED` | The device is host-owned and the method would change it |
| -2 | `NOT_FOUND` | No such device, method, stream or resource name |
| -5 | `IO` | The hardware did not answer |
| -11 | `AGAIN` | Not ready yet: a chip still powering up |
| -16 | `BUSY` | Someone else holds it, or the driver is in a call right now. Ask again |
| -19 | `NO_DEVICE` | The device is not running |
| -22 | `INVALID_ARGUMENT` | A bad setting or argument |
| -95 | `NOT_SUPPORTED` | The driver or the hardware does not do this |
| -110 | `TIMED_OUT` | The call ran past its deadline: 2 s, or the board file's `call_deadline_ms` |
| -4099 | `PANIC` | The driver panicked in the call. It has been dropped and will be bound again |

## Named files (`lemnos/`)

The `named-files` driver serves the `lemnos/` names, which are Lemnos's own knowledge of the board. A
client (the Orion bridge, `lemnos-ctl`) gets a file by opening its name, reading it in pieces and
closing it: the contract is `lemnos.files/1`.

A file is a snapshot taken when it is opened, so its bytes are fixed for the transfer's life: a resumed
read and a digest over it agree. At most four snapshots are open at once. Each name has a bound in bytes;
a file over it is refused, never cut short. A name the node does not serve is refused by name.

`lemnos/index` is the index, in Orion's text format (`orion-index 1`):

```text
orion-index 1
total <bytes>
<name> <max_bytes> user-data|no-user-data
```

`total` is the sum of the listed names' bounds. The index lists the names served now, not every name
the driver knows: a name whose source is absent (a tree file whose key is unset, a boot history with no
boot in it yet) is not listed and is refused when asked for.

| Name | Source | Bound | User data |
| --- | --- | --- | --- |
| `lemnos/boots` | the boot history (`docs/slots.md`, "Boot history") | 16 KiB | no |
| `lemnos/storage` | statvfs of the data and boot partitions; `life_time` and `pre_eol_info` of each `mmcblk*` device | 4 KiB | no |
| `lemnos/trial-reason` | the boot partition's `trial-reason`, when present | 4 KiB | no |
| `lemnos/tree/overlay.dts` | the file `tree_overlay` names | 256 KiB | no |
| `lemnos/tree/boot.txt` | the file `tree_boot` names | 64 KiB | no |
| `lemnos/tree/modprobe.conf` | the file `tree_modprobe` names | 16 KiB | no |
| `lemnos/kernel` | the lemnos module's parameters, `name=value` sorted by name; absent while the module is not loaded | 16 KiB | no |
| `lemnos/index` | this index; always readable, and not listed in itself | 2 KiB | no |

Not yet served (later PRs of I6f): `lemnos/log`, `lemnos/board.toml`, `lemnos/description`,
`lemnos/slots`, `lemnos/identity`, `lemnos/versions`, `lemnos/self-test` (waits for PR X1). Nothing is
read in the background; each name is read when it is opened.

## lemnos-orion

`service/orion` builds `lemnos-orion`, a bridge that offers every device the daemon serves to an
Orion node, through Orion's one client crate, `orion`, pinned to a revision of its `next` branch.
It is a client of lemnosd like any other and knows no device: everything below comes from a
device's entry and its description.
The bridge is untyped by design, like `lemnos-ctl`: it holds no contract traits and runs no bind
check. Its connection is checked at connect like any client's (the core and the parts, see
[the wire](wire.md#core-and-parts)); beyond that it checks no contract, and calls the methods the
device describes.

| Lemnos | Orion |
|---|---|
| a device | the resource `lemnos.<device>`, of the kind `lemnos.device`, with the labels `lemnos.driver`, `lemnos.tag` (one per tag) and `lemnos.control.<item>` (the shapes a control or action item takes) |
| its state and health | the resource's state: `Ready` while it runs, `Failed` while it retries or reports failed health, `Pending` while it is off |
| its value items | the topic `lemnos.<device>.values`: a field per item that is a number (a float in the item's unit, scaled by its exponent) or a flag (a bool), published as it changes |
| its events | the topic `lemnos.<device>.status`: `state`, `health` and `reason` as text, as `lemnos-ctl list` names them |
| `lemnos-ctl set`, `call`, `on`/`off` | the actions `lemnos.<device>.set` (`ITEM VALUE...`), `.call` (`METHOD ARG...`, the reply is the output, as text) and `.power` (`on` or `off`), with the arguments as text, the words `lemnos-ctl` takes. Programs on the board and operators granted them may run them |

**Topics and how each is read.** Every topic the bridge publishes is a state: its newest sample is what
matters, and it is current until the bridge publishes a change or withdraws the topic. None is an
event today (no topic carries each job progress or frame; progress goes with the run). Each
state is a latest-value topic (history 2). The table is in code as `publish::CLASSES`.

| topic | kind | expiry | history | what it says, and why it is current until changed |
|---|---|---|---|---|
| `lemnos.<device>.values` | state | zero | 2 | a device's readings: published as each changes; withdrawn with the device |
| `lemnos.<device>.status` | state | zero | 2 | `state`, `health`, `reason`: published as each changes; `gone` when lemnosd stops listing the device |
| `lemnos.<device>.description` | state, a generation | zero | 2 | the device's description generation (an `int`): it moves when what `describe` says of the device changes (its state or reason, which the description carries). A client that sees it move asks `describe` again |
| `lemnos.devices` | state, a generation | zero | 2 | the device set's generation (an `int`): it moves when a device comes or goes |
| `hello.<fact>` | state | zero | 2 | a board fact: published when it changes; withdrawn when it goes or the session ends |

**Expiry.** Every state is declared with an expiry of zero, which is "valid until changed". The
device topics are declared through Orion's resource `topic()`, which sets zero whatever is asked, so
zero is the only expiry a device topic can have at this rev. That is deliberate: the bridge publishes
each change and withdraws the topic when the device goes, so a sample is never served as current
after it went stale. The facts are declared directly and get zero for the same reason.

**Late subscribers.** A subscriber that arrives after a state's last sample may miss it until Orion
sends the last value on subscribe (a later Orion rev, `on_subscribe`). Until then the bridge does not
republish on a timer. Once Orion has it, the move is setting `on_subscribe = 1` on the state rows
above and nothing else.

**Description generations.** The generation counts from 1 when a device comes. It moves once for each
change of the device's state or reason, and not for a change of health alone (the description does not
carry health). The device set's generation moves once for each change of the set, however many devices
came or went together. Each is published as a sample on change and when a session starts, and held
while nothing changes.

**The `hello` facts and offers.** `hello.offers` lists the distinct contract ids that the running
devices serve, sorted and separated by a space. A consumer grants a board only what `offers` names, so
it is recomputed as devices start and stop, and the fact is declared again or withdrawn when it
changes. Each contract id is about 14 bytes, and all 32 shipped contract ids come to 425 bytes; a
fact's value is held to 1024 bytes (`hello::MAX_VALUE_BYTES`), which keeps every shipped id. (At 256 bytes
the whole list was dropped on a board with about 19 device contracts.)

**Security.** The bridge has no switch of its own, and no hardware-control capability of its own.
Setting a settable item and calling a device's method are the device contract's own rights, and the
bridge admits the device's actions to local callers and to operators only (`publish::POLICY`); the
bridge never admits anyone else. What the operator's "security on" means for a device is the driver's
own configuration: for the slots and system-flash drivers, `require_signed` refuses a bundle no trusted
key signed (off by default). That switch governs installs, not device actions.

**Descriptions.** A device's description (what `describe` returns) is not an out-buffer method, and is
not duplicated as bridge labels. On the walk board (the largest simulated board: fake accelerometer
and gyroscope, identity, slots, raw bus, usb gadget, a WS2812 strip and its indicator; eight devices,
each running) each description measures, with `encode_description` of its manifest, from 178 bytes
(the indicator) to 764 bytes (the usb gadget), and the largest is 764 bytes. The bridge's bound is
61440 bytes, so a description on that board is far under it; no paged form is needed for those
devices. The full-board example's devices need hardware, so their descriptions cannot be measured on
this machine, and are not measured here. The measurement is `the_description_of_each_device_on_the_walk_board_is_measured_against_the_bound`.

The bridge does not yet serve `describe` through `.call`. Its client decodes a description and keeps no
bytes of it, and the manifest that re-encodes one is held by lemnosd, so the bridge cannot send the
frame as it is. A text form of the description, made by lemnosd, is the step that is not built.

**Out-buffer replies through `.call`.** An out-buffer method takes the same optional size word as
`lemnos-ctl call` (see above). The bridge's default size is 61440 bytes, not the frame limit, and a
size word above 61440 is refused. Orion's client and comm core take frames of 64 KiB, and the action's
result frame also carries the call, the outcome and the detail, so the reply is held to 60 KiB. A
reply larger than that is never cut short: the run ends `Failed`, with no output, and its detail says
that the reply is larger than 61440 bytes and that the method has no paged form. Nothing is sent in
pieces. `lemnos-ctl`, on the board's own socket, carries a frame of 1 MiB and reads such a reply
whole. A method whose reply can exceed 60 KiB needs a paged form to be called through the bridge.

**Coverage through the bridge.** `service/orion/tests/live_walk.rs` calls, through `.call` on a real
Orion core and lemnosd, every out-buffer method the simulated board can host, and checks each reply:
`identity` (`serial`, `family`, `model`, `facts`), `slots` (`list`), a `raw-bus` on the simulated I2C,
SPI and UART (`i2c_transfer`, `spi_transfer`, `uart_read`), `gadget` (`data_stream`, and `control_out`
refused with no request pending, `INVALID_ARGUMENT`), the fake accelerometer and gyroscope (`read_batch`
refused as `NOT_SUPPORTED`: a fake has no FIFO), and `lights` (`states`, an indicator on a WS2812
strip). That is 13 of the 33 out-buffer methods. The success path of `read_batch` and of `control_out`
is not reached through the bridge, since neither has a simulated FIFO or a stream consumer to give
the request.

The other 20 are covered by the text path only (`every_out_buffer_method_of_every_contract_is_callable_through_the_text_path`
in `service/ctl/tests/out_buffers.rs`), because no simulated device can host them:

- `camera-frames` `frame_stream`; `camera-isp` `config_stream`, `statistics_stream`, `input_stream`,
  `output_stream`; `camera-receiver` `frame_stream`, `feeds`, `embedded_stream`, `source`; `camera-sensor`
  `identity`: the simulator has no camera, ISP or image sensor;
- `codec-block` `block` and `job_stream`; `image-blend`, `image-convert`, `image-scaler` `job_stream`; `npu`
  `job_stream`: the simulator has no codec, 2D engine or NPU;
- `fan-control` `curve`: it needs a temperature input, and the simulator has none;
- `gpu-lease` `last_hang`; `system-flash` `info`: the simulator has no GPU and no flash device;
- `system-slots` `list_page`: the paged form, whose bridge walk is in `service/orion/tests/orion_bridge.rs`.

**Time.** A sample's time is `CLOCK_MONOTONIC` nanoseconds, the clock Orion's samples are on. A
value carries the time the driver read it, as lemnosd stamped it, and the bridge passes that on
unchanged; a status change, or a fact, is stamped when the bridge sees it. The bridge converts
nothing. Orion's `orion.time` says how far each board's wall clock is from its monotonic one, for
whoever wants a wall time.

**Callers.** Each Orion caller has a lemnosd connection of its own, so a driver that tracks callers
(`TRACKS_CALLERS`) sees each one as its own `CallerId`. A cancel cancels its run and nothing else:
the job is cancelled on lemnosd, and the caller's connection and the driver's holds for it stay.
The bridge does not send `caller_gone` yet. At Orion's pinned revision a cancel does not say why it
was sent, and the same cancel comes when the caller stops the run, when the caller's session ends,
when a deadline passes and when the node stops, so a caller that is still there cannot be told from
one that is gone. When Orion says why (its reason 2, the caller's session ended), the bridge will
close the caller's connection, and lemnosd will tell the drivers that caller is gone.

**Long jobs.** `.call` runs a method that the description marks as a long job, and the run is
that job:

- **Progress.** Each step of the job comes back as the run's progress:
  - `percent`;
  - the message `"<done> of <total>"`;
  - the data `done` and `total`, as two little-endian `u64`s.
- **End.** The job's end is the run's end. Its result is the output: text when it is UTF-8,
  hex otherwise.
- **Cancel.** Cancelling the run cancels the job, and the run then ends `Cancelled`.
- **Lost lemnosd.** If lemnosd goes away, the run ends `Failed`.

**Files for a device.** `LEMNOS_ORION_FILES` names each device that takes files sent through
Orion, and the directory those files go into, for example
`slots=/data/slots/.incoming,other=/var/lib/x`. For each one, the bridge is Orion's receiver
(a files sink) of every file sent to `lemnos/<device>/<name>`. The node never keeps a copy.

- **Before the file is taken**, the device's `accept` method, if its description has one, is
  called with the file's size. If the device refuses, the transfer is rejected with the
  device's words, before a byte is sent.
- **While it arrives**, the file is written under a hidden name, `.orion-<transfer>.part`.
- **When the sender commits**, it is checked against the SHA-256 it was sent with, then synced
  and renamed to `<name>`.
- **Deleted** are a file that fails the check, one with a bad name (one part of
  `[A-Za-z0-9._-]`, not starting with a dot), one that stops arriving for 10 minutes, and what an
  earlier run left half-written.

**Streams for a device.** `LEMNOS_ORION_STREAMS` names files a device takes as a stream, for example
`flash=image.img.xz+image.img` (each name must also be a name the device's directory takes). A transfer
with one of those names is not written to disk: the bridge reads its transport manifest (`<name>.json`,
pushed first like any file; its optional `signature` field is the signature record), calls the device's
`manifest`, starts its `write` job on the bridge's own lemnosd connection, and gives each chunk to the
job's `data` before acknowledging it. The commit checks the stream's SHA-256 and answers with the job's result
([First flash](slots.md#first-flash)).

The sender then runs the device's action that uses the file, such as `.call install_file <name>
false false` on a slot device ([Slots](slots.md#over-orion)). A transfer that is begun again
resumes from where it stopped.

The bridge needs to be able to write into the directory. See `packaging/README.md`.

**Changes are said one at a time.** The Orion session is kept while the board runs; only what
changed is said, and nothing else on the session moves:

- a device that comes is described once: its resource, topics and actions;
- a device that goes has its status topic say `gone`, then its resource is withdrawn once. Its
  topics go with it, so their subscribers are told the publisher is gone;
- a fact (`hello.<fact>`) that appears is declared once, and one that goes is withdrawn once;
- a value or a state that moves is published as it moves.

A device that is not running when the bridge reads the list has no description yet; the bridge
reads the device again when lemnosd's event says it is running (see [Events](wire.md#events)).

When lemnosd stops, every device is taken away the same way (its status says `gone`, its resource
is withdrawn), and the board's facts go with the identity device. When lemnosd is back, the devices
are described again, and so are the facts. The Orion session itself is kept while the board runs, so


When the Orion comm core restarts, the bridge connects again and says every topic, action and
resource again, and each value and state again. Both sides retry forever, and the bridge never
gives up on an idle Orion session: it keeps its own session pinging and notices a comm core that
stops answering within the session's idle time (`comm.idle_ms`), then connects again.

The board's identity facts are topics too, `hello.<fact>`, from the identity device and the
bridge's own `offers`; [Identity](identity.md#how-the-bridge-publishes-them) says which, when each
is republished, and what is absent when.

**Orion's shared-memory crates.** Orion pulls the `shm-*` crates from this repository at a pinned
revision. The root `Cargo.toml` patches that source to the workspace paths, so the bridge and Orion
are built from one copy of each (`shm-sample-time`, `shm-ring`, `shm-ring-time`, `shm-ring-doorbell`,
`shm-fdpass`). `scripts/check-orion-copies.sh` runs in the gate and fails on a second copy, or on one
that is not the workspace's.

It reads:

- `LEMNOSD_SOCKET`;
- `ORION_ADDRESS`: any address the Orion client takes. When it is unset, the board's own
  `/run/orion/orion.sock`;
- `ORION_NODE_ID`: when it is set, a comm core that is any other node gets nothing;
- `LEMNOS_ORION_FILES`, described above. It has been run against a real Orion node on a PC
([`B20.md`](../testing/board-runs/B20.md) has that run); not yet on a board.

```sh
kill %1
wait
```
