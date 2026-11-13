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

## lemnos-orion

`service/orion` builds `lemnos-orion`, a bridge that offers every device the daemon serves to an
Orion node, through Orion's one client crate, `orion`, pinned to a revision of its `next` branch.
It is a client of lemnosd like any other and knows no device: everything below comes from a
device's entry and its description.

| Lemnos | Orion |
|---|---|
| a device | the resource `lemnos.<device>`, of the kind `lemnos.device`, with the labels `lemnos.driver`, `lemnos.tag` (one per tag) and `lemnos.control.<item>` (the shapes a control or action item takes) |
| its state and health | the resource's state: `Ready` while it runs, `Failed` while it retries or reports failed health, `Pending` while it is off |
| its value items | the topic `lemnos.<device>.values`: a field per item that is a number (a float in the item's unit, scaled by its exponent) or a flag (a bool), published as it changes |
| its events | the topic `lemnos.<device>.status`: `state`, `health` and `reason` as text, as `lemnos-ctl list` names them |
| `lemnos-ctl set`, `call`, `on`/`off` | the actions `lemnos.<device>.set` (`ITEM VALUE...`), `.call` (`METHOD ARG...`, the reply is the output, as text) and `.power` (`on` or `off`), with the arguments as text, the words `lemnos-ctl` takes. Programs on the board and operators granted them may run them |

A sample's time is the wall clock when the bridge read it, as Orion's own publishers stamp them.

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

The sender then runs the device's action that uses the file, such as `.call install_file <name>
false false` on a slot device ([Slots](slots.md#over-orion)). A transfer that is begun again
resumes from where it stopped.

The bridge needs to be able to write into the directory. See `packaging/README.md`.

When lemnosd stops or comes back with another device list, the bridge publishes `state = gone`
on every status topic, withdraws every resource (their records are deleted) and ends its Orion
session, which is how Orion tells each subscriber
of a topic that its publisher is gone: Orion has no way to take back one topic on its own. When
lemnosd is back, the same topics are declared again and their subscribers carry on. A device
that is not running when the bridge reads the list has no description yet; the bridge reads the
device again when lemnosd's event says it is running (see [Events](wire.md#events)). When the
Orion comm core restarts, the bridge connects again and says every topic, action and resource
again. Both sides retry forever.

The board's identity facts are topics too, `hello.<fact>`, from the identity device and the
bridge's own `offers`; [Identity](identity.md#how-the-bridge-publishes-them) says which, when each
is republished, and what is absent when.

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
