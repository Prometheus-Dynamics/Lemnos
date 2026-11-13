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
  its reason, and is retried under [the failure rule](concepts.md#the-failure-rule). A device it
  cannot start at all is refused with its reason, which [Start-up](#start-up) lists. A board with
  nothing reachable still starts.
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

## Start-up

One device's problem never stops the daemon. A device the daemon cannot start is refused with its
reason: a setting its driver refuses, a device entry that is wrong on its own (a duplicate id, a
`config` value that is not a number, boolean, string or list of strings, a stream its driver cannot
take), a loop of `uses` (the device that closes the loop is refused, so the others bind), a gadget
whose `identity` names no device or the wrong one, or whose root-phase setup fails. It shows failed in
`lemnos-ctl list` and in its state, with the reason, and the devices that use or are linked to it
behave as they do when a device fails to bind. Every other device runs, and the daemon starts. A
refused device is not made, so its retries repeat the same refusal; a refusal that only a board-file
edit or a restart can change says so in its reason (a gadget's root phase runs as root, once, before
the drop, so it needs a restart to retry).

The daemon starts, or stops at start, for these reasons only:

- The board file is not TOML, has no `[board]`, has a `format` or `schema_version` this build does not
  read, or has a `[host]` value out of range. Nothing in such a file can run, so the file is refused
  whole, by name, on the error output (exit 1). The daemon never listens, so a keep-alive device is not fed
  and the hardware watchdog resets the board after its timeout, which is the fail-safe.
- The process cannot run as asked: it cannot create its socket or the socket's directory, cannot
  listen, cannot start a thread or its signal handler, cannot read its credentials, the user or group
  it is asked to run as does not exist, the drop to that user fails, or the state after the drop is
  not the one asked for. Each stops the daemon with the reason on its error output (exit 1).
- The command line is wrong (an unknown option, `--group` without `--user`).

`lemnosd --check` refuses a board for every device refused above, as `lemnos-ctl validate` does, and
exits 1. Nothing else is checked at start: a check that needs the hardware is made at bind.

## The root phase

Started as root with `--user` (or `LEMNOSD_USER`), lemnosd runs its privileged start, then drops to that
user and group, keeps only `CAP_SYS_RAWIO`, and verifies the result before it listens. A step that
cannot set up one device (a gadget whose configfs setup the kernel refuses) refuses that device with
the reason, and the daemon starts without it ([Start-up](#start-up)). A step that fails for the process
itself, a failed drop, or a state after the drop that is not the one asked for stops it, with the
reason on its error output. A node the daemon cannot open after the drop is a line on the error output,
and the device that opens it fails at its bind with its own reason. Started without root, it runs as it is and says which capabilities it lacks. The
privileged start is the list `STEPS` in `service/lemnosd/src/privilege.rs`: each step runs as root, with
the board file and the socket and ids the daemon will run as, and a step added there (the USB gadget's
configfs setup is the second) runs before the drop. A step returns the devices it cannot set up, with
their reasons, and returns an error only for the process itself. No unit or script gives the daemon a user, or a
file capability. The steps and the drop are described in
[`packaging/README.md`](../packaging/README.md#one-privilege-path).

## Running under systemd

`packaging/systemd/lemnosd.service` starts the daemon as root and runs it with `--user lemnos --group
lemnos`, so it drops to `lemnos` itself. The device nodes are given to group `lemnos` by
`packaging/udev/70-lemnos.rules`. Its one kept capability is `CAP_SYS_RAWIO`, for register mapping
through `/dev/lemnos`. The unit starts only when the OS has
supplied the board file at `/etc/lemnos/board.toml`. The OS installs the unit files and supplies the
`lemnosd` binary and the board file. The unit restarts the daemon after a constant 1 s and sets no
`WatchdogSec=`: the hardware watchdog escalates, as it does without systemd. [`packaging/files.txt`](../packaging/files.txt) lists the install
paths, and [`packaging/README.md`](../packaging/README.md) says what each unit needs and what it does not. The PWM and USB gadget drivers write sysfs and
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
| `camera-receiver/1` | `inputs` | 768 bytes (`MAX_INPUTS` records of `INPUT_RECORD`, 48 bytes each) | whole |
| `camera-sensor/1` | `identity` | 64 bytes (`MAX_IDENTITY_NAME`) | whole |
| `codec-block/1` | `block` | 48 bytes (`MAX_BLOCK_NAME`) | whole |
| `codec-block/1`, `image-blend/1`, `image-convert/1`, `image-scaler/1`, `npu/1` | `job_stream` | 32 bytes (`MAX_STREAM_NAME`) | whole |
| `fan-control/1` | `curve` | 64 bytes (8 points of 8) | whole |
| `gpu-lease/1` | `last_hang` | 128 bytes (`DETAIL_MAX`) | whole |
| `system-identity/1` | `serial`, `family`, `model` | 256 bytes (`MAX_TEXT`) | whole |
| `system-identity/1` | `facts` | 2048 bytes (`MAX_FACTS_TEXT`) | whole |
| `self-test/1` | `tests` | a device: 1 + 8 tests of 249 bytes, 1993 bytes; a board: 1 + 129 entries, 32122 bytes | whole |
| `self-test/1` | `last` | a device: 1 + 8 results of 454 bytes, 3633 bytes; a board: 1 + 129 entries, 58567 bytes | whole, under 61440 |
| `system-slots/1` | `list` | unbounded: 1 slot is about 350 bytes, so about 170 fit | refused by name over about 170 slots; use `list_page` |
| `system-slots/1` | `list_page` | one page: the whole slots that fit the size, about 170 in 60 KiB | whole: the page is at most the size given, and the walk continues from the page's `next` (see `docs/slots.md`) |
| `system-slots/1` | `info` | about 1 KiB, plus the owner of the last signing key (the key file's text, under 2 KiB), plus one `key_ids` entry per held key (64 hex digits and its owner) | `BUFFER_TOO_SMALL`; a 4 KiB buffer holds any owner the key file allows in practice and a board's few keys |
| `system-security/1` | `state`, `precheck` | 1024 bytes (`TEXT_MAX`) | whole |
| `system-security-precondition/1` | `precondition` | 1024 bytes (`TEXT_MAX`) | whole |
| `raw-bus/1` | `i2c_transfer`, `spi_transfer` (`read_len`), `uart_read` (`max_len`) | the caller's length, up to 4 GiB | a length above 61440 is refused before the transfer; ask for less and repeat |
| `usb-function/1` | `control_out` | the pending request's `length`, up to 65535 | not checkable up front (no length argument). The USB driver refuses a buffer smaller than the pending request's `length` with `BUFFER_TOO_SMALL` before it asks the device, so nothing is transferred and the request stays pending; the bridge names the refusal. Tested on the mock (`drivers/usb-gadget/src/serve_tests.rs`) |
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
| -4100 | `LAYOUT_MISMATCH` | A frame stream's shared memory (its ring header or its marks page) is of another layout than this build reads. The message names the structure and both values; the C surface reports them with `lemnos_layout_refusal` |

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

### Named files over Orion

The bridge serves the node's `lemnos/` names as files (the "Named files" section above) through
`service/orion/src/named.rs`. A file Orion sends is a snapshot: a `Read + Seek` over the bytes the node
held when the transfer opened, read from the node in pieces and closed when dropped; a resumed transfer
seeks to its offset. The node is named by `LEMNOS_ORION_NAMED_FILES` (the device running
`named-files`); unset, nothing is served. A name without the `lemnos/` prefix is refused and nothing is
opened.

Orion's offer for a prefix (`Files::offer`) is not in the pinned Orion revision yet, so the loop that
connects the offer to these snapshots is not compiled: the marked `ORION-OFFER` comment in
`named.rs` gives it. Until that switch is on, the bridge serves no `lemnos/` names over Orion.

| Lemnos | Orion |
|---|---|
| a device | the resource `lemnos.<device>`, of the kind `lemnos.device`, with the labels `lemnos.driver`, `lemnos.tag` (one per tag) and `lemnos.control.<item>` (the shapes a control or action item takes) |
| its state and health | the resource's state: `Ready` while it runs, `Failed` while it retries or reports failed health, `Pending` while it is off |
| its value items | the topic `lemnos.<device>.values`: a field per item that is a number (a float in the item's unit, scaled by its exponent) or a flag (a bool), published as it changes |
| its events | the topic `lemnos.<device>.status`: `state`, `health` and `reason` as text, as `lemnos-ctl list` names them |
| `lemnos-ctl set`, `call`, `on`/`off` | the actions `lemnos.<device>.set` (`ITEM VALUE...`), `.call` (`METHOD ARG...`, the reply is the output, as text) and `.power` (`on` or `off`), with the arguments as text, the words `lemnos-ctl` takes. `.call describe` with no arguments gives the device's description (below). Programs on the board and operators granted them may run them |

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

`.call describe` answers the description as text, through the bridge, from the description the bridge
decodes (see "The description through `.call`" above). The bytes the client drops are not kept.

**Out-buffer replies through `.call`.** An out-buffer method takes the same optional size word as
`lemnos-ctl call` (see above). The bridge's default size is 61440 bytes, not the frame limit, and a
size word above 61440 is refused. Orion's client and comm core take frames of 64 KiB, and the action's
result frame also carries the call, the outcome and the detail, so the reply is held to 60 KiB. A
reply larger than that is never cut short: the run ends `Failed`, with no output, and its detail says
that the reply is larger than 61440 bytes and that the method has no paged form. Nothing is sent in
pieces. `lemnos-ctl`, on the board's own socket, carries a frame of 1 MiB and reads such a reply
whole. A method whose reply can exceed 60 KiB needs a paged form to be called through the bridge.

**The description through `.call`.** `.call describe`, with no arguments, answers the device's description as text in format version 1. `lemnos_orion::description::render` writes it and `lemnos_orion::description::parse` reads it, both in `service/orion/src/description.rs`.

**The grammar.**

- The text is lines, each ended by one `\n`, the last one included. No line is empty and no line has a trailing space.
- The first line is exactly `describe 1`. A reader refuses any other first line, so a change of format is a new number.
- Every other line is a record: a record word, `[a-z_][a-z0-9_]*`, then zero or more pairs, each preceded by exactly one space, `key=value`. A key is `[a-z0-9_]+`.
- A value is a text value or a bare token. A bare token is `[A-Za-z0-9_.:+-]+`. A text value is in double quotes. Inside the quotes `\\` is a backslash, `\"` a quote, `\n` a newline, `\r` a carriage return and `\t` a tab. Those are the only escapes: any other backslash sequence is an error. A raw control character is an error too (a tab, a newline or a carriage return is written as its escape). Every other character, non-ASCII text included, is written as it is. The closing quote is followed by a space or the end of the line.
- A key appears once per record, a record has at most 64 keys, and a line is at most 4096 bytes without its newline.
- A reader ignores a key it does not know and a record whose word it does not know. Anything else is refused by name, with its line number.

**The values.** A number is bare: decimal, or hex with `0x` (an address and a hash). A real number is its shortest round-trip form (`-1.5`, `-0.0`, `1e300`). A number the device does not give is an absent key, never a placeholder, and a real number that is NaN or infinite is absent too. An enum word is bare: `running`, `retrying`, `off`, `unknown`, `known`, `dimensionless`, `value`, `control`, `action`, `number`, `range`, `switch`, `text`, `colour_array`, `three_axes`, `true`, `false`. A text option the device does not set is the bare word `none`; the quoted `"none"` is the text `none`.

**The records.**

- `device generation lease_seconds label kind bus address location state reason live`. `generation` is the device's description generation, the number its `lemnos.<device>.description` topic carries, and is absent before the device has one. `lease_seconds` is the bridge's caller lease (the hello fact `lease.seconds`), absent when the lease is off. `address` is `0xNN` or `none`. `state` is `running`, `retrying`, `off`, or `unknown` when none is given, and `reason` says why the device is not running or what its last failure was (empty when it runs). `live` is `true` when the device's running driver gave the rest of the description, and `false` when the device is not running (off, retrying, failed or never started).
- `contract index tag kind hash methods`. `index` is the contract's position, which an item's `contract` names. `hash` is hex. `methods` is the count of the `method` records that follow for this contract: those are the records whose `contract` is `index`.
- `method contract id name params returns out_buffer job`. `params` is each `name:shape`, joined by a space, as one quoted text. `out_buffer` and `job` are `true` or `false`.
- `item kind name unit unit_state exponent min max real_min real_max real_step step warn_low warn_high contract method field reading group`. `min`, `max`, `real_min`, `real_max`, `real_step`, `step`, `warn_low` and `warn_high` are absent when the device gives no such bound. `field` and `group` are quoted or `none`. `reading` is one of the reading words, or `unknown`.

**Devices that are not running.** lemnosd answers a description for every device in the board file, whatever its state. A device whose driver is not running (off by the operator, retrying a bind that keeps failing, failed, or never started because the board file disables it) has no driver to describe it. Its device record still comes, with its `state` and `reason` and `live=false`. Its contracts and methods come from the driver's manifest, which lemnosd has for every device, and it has no `item` records, no address and no location: those are what a running driver reports. The generation moves when the state or the reason changes, as it does for a running device. Those are the only answers that differ: a call in flight answers `BUSY`, and the bridge asks again.

**Which records each state carries.** The description has the same records for every device; the state decides what is in them.

| device's state | `device` record | `live` | `contract` and `method` records | `item` records | `address` and `location` |
|---|---|---|---|---|---|
| `running` (driver bound) | yes, with `state=running`, `reason=""` | `true` | from the driver's manifest | from the running driver | from the running driver |
| `off` (switched off by the operator) | yes, with `state=off` and the reason | `false` | from the manifest | none | `none` and empty |
| `retrying` (the bind failed or missed, or its settings were refused) | yes, with `state=retrying` and the reason the bind gave | `false` | from the manifest | none | `none` and empty |
| `unknown` (no state given) | yes, with `state=unknown` | `false` | from the manifest | none | `none` and empty |

The contract and method records come from the manifest because lemnosd has it for every device in the board file, so a device that does not run still says what it offers. A device with no manifest is not in the board file and is not described (`NO_DEVICE`).


**Bounds.** A reader takes at most 60 KiB of text, 4 KiB per line, 4096 records, 64 keys per record, 256 `contract` records, 2048 `method` records and 2048 `item` records. The encoder refuses a description that would pass one of them, by name, with the count and the bound. It never cuts a description short: the bridge sends the whole text or a failure that says why. A text value that holds a control character other than tab, newline or carriage return cannot be written in the format, so the description is refused by name too. `service/orion/tests/description_bounds.rs` describes every built-in driver through this text form and fails if one is over a bound. Measured then: the largest description is 3775 bytes (`ov9782`, 21 lines, 20 records, a manifest-only description since its driver does not bind on the simulated board), and the largest one from a running driver is 2149 bytes (`rp1-csi2`, 17 lines, 10 keys in a record). The longest line is 783 bytes. The most keys in one record is 19 (`pisp-be`). No driver comes near 60 KiB, so there is no paged form: a description that would pass a bound is refused by name. The items of a driver the simulator cannot host are not in those figures, since only a running driver gives items; a running item record is about 150 to 300 bytes, so a device would need hundreds of items to reach 60 KiB.

**Examples.** Two real replies from the walk board, and one for a device whose bind keeps failing. The running one's `location` is shortened to a `/sys` path.

```describe
describe 1
device label="zone" kind="sensor" bus="" address=none location="/sys/class/thermal/thermal_zone0" state=running reason="" live=true
contract index=0 tag="temperature/1" kind="sensor" hash=0xc77f0ded2da1235c methods=1
method contract=0 id=1 name="read" params="" returns="f32" out_buffer=false job=false
item kind=value name="temperature" unit="°C" unit_state=known exponent=0 contract=0 method=1 field=none reading=number group=none
```

```describe
describe 1
device label="zone" kind="sensor" bus="" address=none location="" state=off reason="manual" live=false
contract index=0 tag="temperature/1" kind="sensor" hash=0xc77f0ded2da1235c methods=1
method contract=0 id=1 name="read" params="" returns="f32" out_buffer=false job=false
```

```describe
describe 1
device label="n-a733-di" kind="compute" bus="" address=none location="" state=retrying reason="bind failed: Status(-2)" live=false
contract index=0 tag="codec.block/1" kind="compute" hash=0x7c1ff4f41c048c36 methods=4
method contract=0 id=1 name="info" params="" returns="{max_work_bytes: u32, max_refs: u16, max_items: u16, max_read_back: u32, max_buffers: u32}" out_buffer=false job=false
method contract=0 id=2 name="block" params="" returns="bytes" out_buffer=true job=false
method contract=0 id=3 name="job_stream" params="" returns="bytes" out_buffer=true job=false
method contract=0 id=4 name="counters" params="" returns="{jobs: u64, refused: u64, failed: u64}" out_buffer=false job=false
```

`describe` is a word of the bridge, not a method: no device has a method of that name. `describe x` is
refused. The text is held to 60 KiB like any reply.

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

**Callers.** Each Orion caller has a lemnosd connection of its own, one per Orion caller identity
(`local:<uid>` and the others), not per session: Orion names every local session of one user the same
caller. A driver that tracks callers (`TRACKS_CALLERS`) sees each caller as its own `CallerId`. A cancel
cancels its run and nothing else: the job is cancelled on lemnosd, and the caller's connection and the
driver's holds for it stay.

**Leases.** At Orion's pinned revision the bridge learns of a session's end only as a cancel of a run in
progress, and that cancel does not say why it was sent. A silent live session cannot be told from a dead
one, so the bridge bounds the holds of a caller by a lease instead. When a caller has been silent for the
lease, the bridge drops that caller's lemnosd connection, its jobs end with it, and lemnosd tells the
drivers the caller is gone (`caller_gone`), which releases what it held. The lease is `LEMNOS_ORION_LEASE`
seconds, default 60. `LEMNOS_ORION_LEASE=0` turns the lease off: no caller is dropped for silence, and the
bridge says nothing of a lease (see below).

**Only silence expires a lease.** A caller's lease is renewed by each of these, and by nothing else:

- every request of the caller: a `call`, `set` or `power` run, a `cancel` of its run, and a request
  refused for its arguments. At Orion's pinned revision a cancel does not say who sent it, so a session's
  end that arrives as a cancel renews too;
- the end or cancel of one of its runs, which starts the lease from that moment;
- every progress report of a long job the caller started (the job's progress is news of its run);
- every file or stream chunk the bridge accepts, that is, one it answers as still receiving. Pinned Orion
  does not say which session sent a chunk, so an accepted chunk renews every caller's lease, not only its
  sender's. A dead caller's holds therefore survive while any transfer is being accepted.
  `ORION-R2 (C33)` marks where the sending session is named.

**A caller with a run in flight is alive.** A caller with any run in flight (started by it, not yet
finished and not cancelled) is not dropped for silence, whether or not the run reports progress, so a
healthy silent job (a verify, a self-test) keeps its caller's holds. The comm core cancels the runs of a
session that ends (a reason-less Cancel for a non-detached run), and a cancelled run no longer counts, so
a dead caller's run stops shielding it. The one residual case: a DETACHED run from a dead caller is not
cancelled by the comm core, and the bridge cannot see the detached flag, so it shields that caller's holds
until the run itself ends. The later protocol revision closes that (`ORION-R2 (C33)` in
`service/orion/src/calls.rs`).

Not renewed: the bridge's own housekeeping (its publishing of the hello document, its ticks), and Orion's
own traffic to the caller (its topic subscriptions and its pings), which the bridge does not see.

**The note of a lease's end.** The first reply the bridge makes to a caller after its lease ended is served
normally, on a fresh connection, and its `detail` begins with `lease-ended: holds released` (the note, given
once per expiry). The rest of `detail` is free text, as it was: a client matches the prefix `lease-ended:`
and reads the remainder as it reads any detail. A reply carries the note when the caller's run is answered
(its end or cancel included), so a run that began after the drop carries it on its reply. The bridge
remembers at most 64 callers whose note is still due; the oldest is forgotten first. A caller that comes
back after that sees no note, and `leases.ended` still counts the drop.

**The lease is readable.** While the lease is on, the hello document carries `lease.seconds` (the lease
in whole seconds, so no client hard-codes it) and `leases.ended` (how many callers' leases have ended
since the bridge started), and the describe reply's device record carries `lease_seconds`. With
`LEMNOS_ORION_LEASE=0` all three are absent: a client reads the lease as off when `lease.seconds` is not in
the hello document, which is also what an image without the lease says. Each drop is logged by the caller's
class (`local`, `operator`, `peer`, `device` or `anonymous`, never its key or uid), with one connection released.

**This is a breaking change for clients, and it is breaking for a silent holder.** A client that holds
state on a device (a lease, a claim, a hold taken through `.call`) renews it with any request before the
lease runs out. `.call describe` is the renewal with nothing else to do: a client holding state and
otherwise silent sends it at least every lease divided by three, or keeps a run in flight, or a
push that keeps sending. A silent holder, one that renews nothing for the lease and has no run in flight, loses its holds when the
lease runs out, whether it is alive or not. The bridge cannot tell the two apart at this revision.

A caller that is detached from its run cannot be told apart from a live one at this revision (the bridge
does not see the detached flag), so its holds last as long as its run's progress does. When Orion's cancel
says why it was sent, the connection will be closed at once on a session's end and the lease will not be
needed for it (`ORION-R2 (C33)` in `service/orion/src/calls.rs`). Power and the boot holds of VBUS are not
caller holds and are not released by a lease.

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

A transfer or stream that gets no chunk for the files' idle bound (600 seconds, the same bound after which
the receiver deletes a partial file) is dropped with its device job, so a sender that went quiet does not
hold the device. The bound is not set by the environment. The bridge sees chunks, not reads: Orion's
sender packs its reads into 32 KiB chunks, so a sender keeps its stream alive by sending a chunk within
the bound, and each chunk restarts it. The bridge does not tell the sender; the same bound applies whether
the sender is alive or gone, because Orion says nothing when a session ends except for a run in progress.

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
