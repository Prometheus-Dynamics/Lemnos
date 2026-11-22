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
  its safe state: a fan that a controller drove goes back to the duty and mode it had before the
  controller took it (its baseline, FAN-RELEASE), and a fan with no baseline goes to full duty. The
  hardware watchdog then resets the board if the stop is not followed by a start.

It writes nothing to its standard output. Its error output gets what could not be used, what
became of each driver package, and a driver's panic message.

The drivers compiled in are listed in `service/lemnosd/src/builtin.rs`, which
`scripts/generate-builtins.py` writes from the driver crates. The `fake-devices` feature adds the
four fake drivers the simulated board uses.

### Cargo features

`lemnosd` has one feature that changes what it depends on, `orion-precheck`, on by default.

- **`orion-precheck`** compiles in the Orion precheck driver (`drivers/orion-precheck`). That
  driver asks Orion's `security_precheck` before the security switch changes, and it is the only
  part of `lemnosd` that uses Orion. With the feature on, the build pulls in the `orion` crate
  through `lemnos-shipped-features/orion`.
- **Shipped builds have it.** `default` enables it, so a plain `cargo build` and every program in
  `packaging/shipped.txt` include the precheck. `scripts/check-features.sh` checks that the shipped
  build's `lemnosd` has it on.
- **Embedding consumers turn it off.** A consumer that links `lemnosd` as a library builds it with
  `default-features = false`. Then the build has no `orion` and no `orion-precheck` crate at all,
  and the security switch is not gated by Orion.
- **Lockstep if you turn it on.** A consumer that enables `orion-precheck` must pin `orion` at a
  revision whose `shm-*` crates are the same revision as its Lemnos pin, so the build holds one
  copy of each `shm-*` crate. Check the consumer's lock with
  `scripts/check-shm-one-source.sh <Cargo.lock>` and `scripts/check-lemnos-pin.sh <Cargo.lock> <rev>`.
  Enabling it is only supported in a build that keeps the root `[patch]` of the Lemnos source.

- **`testing-boot-roots`** is for the QEMU route-A stanza only, never a shipped build: with
  `LEMNOSD_TEST_BOOT_ROOTS=CHOSEN:SYS:DEV` set, the slot drivers read their boot chain from those
  three directories instead of `/proc/device-tree/chosen`, `/sys` and `/dev` (an x86 guest has no
  device tree). `testing/qemu/run.sh` builds it after the shipped daemon is copied, as
  `/lemnosd-route-a`.

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
whose `identity` names no device or the wrong one, or whose description the board file gets wrong (a
`function` that is not `udc:NAME`, a controller name that is not one directory name, a setting its
gadget driver refuses). It shows failed in `lemnos-ctl list` and in its state, with the reason, and the
devices that use or are linked to it behave as they do when a device fails to bind. Every other device
runs, and the daemon starts. A refused device is not made, so its retries repeat the same refusal; a
refusal that only a board-file edit can change says so in its reason.

A USB gadget that cannot be made yet is not refused. Its controller is not there, or its configfs is not
mounted, or a module it needs is still loading: its driver retries, on the backoff of
[the failure rule](design.md), with the gadget's setup done by the [privileged helper](#privileged-helper)
each time. A controller that appears later (a uevent for `udc:NAME`) brings the next attempt forward.
A gadget that cannot be made because the kernel lacks a feature it needs (an NCM interface name on a
kernel before 5.12) is retried all the same, under the failure rule; its reason is written to the error
output on each attempt. The one refusal that stays a restart is a board serial that cannot be read at
start, which the gadget needs for its addresses.

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
cannot set up one device refuses that device with the reason, and the daemon starts without it
([Start-up](#start-up)). A step that fails for the process itself, a failed drop, or a state after the
drop that is not the one asked for stops it, with the reason on its error output. A node the daemon cannot
open after the drop is a line on the error output, and the device that opens it fails at its bind with its
own reason. Started without root, it runs as it is and says which capabilities it lacks. The privileged
start is the list `STEPS` in `service/lemnosd/src/privilege.rs`: each step runs as root, with the board
file and the socket and ids the daemon will run as, before the drop. Its one built-in step makes the
socket's directory. After the gadget helper is forked, the drivers' root phases run (`ROOT_PHASES`):
`system-slots` (`record = "tryboot"`) and `system-security` (`record = "boot"`) open the boot partition
read-only, read whose records are on it, and open it read-write only when they are this system's; the
descriptor is kept for the driver after the drop (`lemnos_slot_store::boot_node::hand_over`), so the node
stays root's. Each prints one line, `lemnosd: root phase: <driver>: <what it did>`. The U-Boot
environment's whole-disk node, the data partition's grow and UBI volumes are not handed over: under the
unit their writes fail by name. The USB gadgets are not a step: the root phase describes them (`gadget::describe`) and
forks the [privileged helper](#privileged-helper) and its root parent for them, which keep root after the drop. No unit or
script gives the daemon a user, or a file capability. The steps and the drop are described in
[`packaging/README.md`](../packaging/README.md#one-privilege-path).

## Privileged helper

A USB gadget needs root for three things: its configfs tree, the FunctionFS mount, and the `UDC`
attribute that binds it. Each can fail at boot for a reason that goes away: the controller is not bound
yet, or is in host role, configfs is not mounted, a module is still loading. A root phase that runs once
would make such a failure final, so the gadget is set up by a helper that outlives the drop.

**What it is.** Two processes, forked by the root phase before the drop. The first is a small root
parent (`service/lemnosd/src/supervisor.rs`, `lemnosd-gsuper` in `ps`), the daemon's child. It does one thing:
it forks the helper and forks it again when it dies, so a helper crash needs neither the daemon (which has
dropped root) nor a person. The second is the helper (`service/lemnosd/src/helper.rs`, `lemnosd-gadget` in
`ps`), the parent's child. Both are the same binary as the daemon. The helper has the gadgets of the board
file, fixed when the parent is forked: each gadget's controller name, the owner of its mount (the daemon's
uid and gid), the gadget's settings, and the board's serial source (the identity device's settings).

**What they can do.** The helper keeps uid 0 and the single capability `CAP_SYS_ADMIN`, which the
FunctionFS mount and its unmount need. The parent keeps uid 0 and two: `CAP_SYS_ADMIN`, and
`CAP_SETPCAP`, which each helper it forks needs to drop everything else when it starts and drops itself at
once: `privilege::drop_to` runs in that child, and `CAP_SETPCAP` is what removes every other capability from the
helper's bounding set (`PR_CAPBSET_DROP`) before its permitted and effective sets are set to `CAP_SYS_ADMIN`
alone. It is used at every fork, so the parent keeps it. The parent holds no `CAP_SETGID` and no `CAP_SETUID`
after it has started: it has every capability when the root phase forks it, its own `drop_to` clears the
supplementary groups it got from the root phase (`setgroups`, using `CAP_SETGID`) and then, as its last step,
narrows its sets to the two it keeps, all before its seccomp filter is installed; the helper it forks inherits
no groups, and neither process changes its user or group id (root to root), so nothing after the start calls
`setgroups`, `setuid`, `setgid`, `setresuid` or `setresgid`, and none is in either allowlist. Under systemd both also run under the unit's bounding set,
`NoNewPrivileges`, `ProtectSystem=strict` and the unit's `SystemCallFilter` (which the gadget drop-in
widens by `mount` and `umount2`). Each also runs under a seccomp allowlist of its own (below). The helper
writes only under the configfs gadget tree it makes (`/sys/kernel/config/usb_gadget/lemnos-NAME`), the
FunctionFS mount point it makes (`/dev/usb-ffs/NAME`), and the `UDC` attribute of that gadget. It needs no
`CAP_CHOWN`, since `UDC` stays root's and the daemon binds through the helper.

**The FunctionFS mount lives in the unit's mount namespace.** Under `ProtectSystem=strict` the
service manager gives each start of the unit its own mount namespace, slave to the host's, so the
mount the helper makes is seen only inside the unit and goes with that start. The instance itself
lives on while a holder keeps the endpoints it was lent. A daemon started again finds the board
file's tree in configfs with no mount it can see, and asks f_fs for one: f_fs refuses a second mount
of an instance that is still mounted elsewhere or has open files (`EBUSY`). Then the tree is held. It
is left bound and as it is, and the setup fails with `EBUSY` and says so, and is tried again under the
failure rule. Once the holder lets go, the mount is taken in the new namespace and the tree is adopted,
not remade. The holder is a client the last daemon lent the pair to (`usb_endpoints_end`), and it can
live on past that daemon: `lemnos-orion` is a unit of its own, and a consumer's process is in no
unit of Lemnos's. Nothing makes it let go, so the wait is bounded. A tree still held
`lemnos_linux::HELD_LIMIT` (10 s) after the first setup that found it held is taken over: unbound,
taken down and made again on a fresh instance. The helper logs that the holder's endpoints are broken,
and the held copies fail from then on. With the 8 s cap on retries, the link is made again within
18 s of the first setup that found the tree held. The board's USB link is worth more than a clean
hand-over. The gadget check runs the daemon under `lemnos-unit-check exec`, and its hub moves into the
daemon's namespace (`ns PID`) for the mounts it makes and removes.

**Every file it touches is an entry of a directory it opened once.** It opens configfs
(`/sys/kernel/config`, which must be a mounted configfs: the directory of an unmounted configfs is "not
there yet" and is tried again at the next setup), the directory of the FunctionFS mount points, and
`/sys/class/udc`, and from then on names one entry of one of them: `usb_gadget`, `lemnos-NAME`, `UDC`. An
entry name that is empty, `.`, `..`, has a `/` or a NUL, or is over 255 bytes is refused before a system
call. An entry is opened with `openat2` (`RESOLVE_BENEATH`, `RESOLVE_NO_SYMLINKS`, `RESOLVE_NO_MAGICLINKS`)
where the kernel has it, and `O_NOFOLLOW` where it does not, so a link in the tree is never followed. The
FunctionFS mount's target is `/proc/self/fd/N` of the mount point's descriptor, and a configuration's links
name their targets the same way. No absolute path is built from anything the daemon sent.

**What they cannot do.** The helper reads nothing from the daemon after it is forked: no path, name,
string or value crosses after the start, and the board serial is read by the helper from the source the
board file names, not handed to it. They open no network socket, mount nothing else (they do not mount
configfs: that is the system's, and a missing configfs is retried), and the helper takes no capability but
`CAP_SYS_ADMIN`. Both ignore `SIGINT`, `SIGTERM` and `SIGHUP`, so a signal to the daemon's process group
does not take them. The helper acts only for the daemon that started it: the socket pair was made by the
root phase, so the kernel records the daemon's process (`SO_PEERCRED`) as the maker of both ends, and the
helper checks the peer's process id and user id on every request. Another peer is answered `refused: not
the lemnosd that started this helper (peer pid P uid U, expected pid P uid U)` and nothing runs. The socket
has no name in the file system, so nothing else can reach it.

**Seccomp.** After its start-up (names, signals, the drop to its capabilities, the opening of its
directories) the helper installs a filter that allows exactly these calls and kills the process (`SIGSYS`)
on any other: `read write close openat openat2 mkdirat unlinkat symlinkat readlinkat newfstatat fstat
fstatfs statx getdents64 lseek fsync fchownat fchmodat mount umount2 getsockopt recvfrom sendto mmap munmap mremap mprotect
madvise brk rt_sigaction rt_sigprocmask rt_sigreturn sigaltstack clock_gettime getrandom exit exit_group
fcntl`, and on x86_64 the calls glibc makes under their own names there (`mkdir rmdir unlink symlink
readlink chmod chown rename open stat lstat`). The parent's filter is that list and the calls that make, wait for and sleep
in a process and that the helper's drop makes (`clone clone3 wait4 waitid clock_nanosleep nanosleep prctl
capget capset getpid gettid set_robust_list rseq set_tid_address getppid getuid geteuid getgid getegid getgroups futex sched_yield seccomp ppoll`, and `fork vfork` on x86_64), because filters stack. A test
runs each verb under the helper's filter in a forked child, and checks that a call outside the list kills
it.

**Their lifetime.** The parent and the helper end when the daemon stops or dies: the helper when the
daemon's end of its socket closes, the parent when its own parent dies (`PR_SET_PDEATHSIG`) or when the
helper ends with status 0. When the daemon stops it takes every gadget down first: unbound, the tree
removed, the mount unmounted (`teardown`); a daemon that crashes takes nothing down, so the tree stays
and the helper of the next daemon adopts it.

**When the helper dies.** The parent says so on the error output (`lemnosd: the gadget helper (pid N) was
killed by signal 31; starting it again in 1000 ms (restart 1)`), throws away the requests the dead helper
never read, waits 1, 2, 4 and then 8 seconds (the failure rule's cap; the wait starts over after the helper
has run for 30 s), and forks a new helper, for as long as the daemon runs. The parent never gives up and
never turns anything off. The new helper adopts the tree the old one left (below). The daemon keeps its end
of the socket for life, and the parent keeps the helper's end across forks, so nothing about the socket
changes: a call that was in flight fails after its 10 s wait, or at once if the helper's end closed, and the
gadget's driver retries it on its backoff. A call that failed is not latched: the next one is served by the
new helper. Only a socket whose other end is gone for good (the parent was killed from outside) fails every
call after it, with the reason, until the daemon restarts.

**The vocabulary.** A request is one line, `VERB INDEX SEQ`. `VERB` is `setup` (read the board serial, make
the gadget's tree and mount, or adopt the one that is there), `bind` (write the controller's name to `UDC`),
`unbind` (write an empty line to it), or `teardown` (unbind, remove the tree, unmount). `INDEX` is the
gadget's index in the fixed list and `SEQ` the daemon's count of its requests, each as canonical decimal: no
sign and no leading zero, a line of at most 32 bytes. Any other line is answered `0 refused ...` and runs
nothing; a fuzz test checks this against the canonical spelling of every line it accepts. The reply starts
with the request's `SEQ`: `SEQ ok`, `SEQ fail ERRNO REASON` (the errno is the step's and the reason is one
line of text the daemon writes to its error output) or `SEQ refused REASON`. The daemon reads until it finds
its own number, so a late answer to a request it gave up on is dropped. No file descriptor passes: the
daemon opens `ep0` and the endpoint files itself, through the mount it owns, as before.

**The board serial.** A gadget that derives its NCM addresses from the board serial names an `identity`
device, whose `serial` settings say where the serial is (device tree, cpuinfo, the kernel command line or an
nvmem cell). The helper keeps those settings from the fork and reads the source again at every `setup`. A
source that cannot be read yet (an nvmem cell that probes late) makes the setup fail with `ENODATA` and the
reason (`the board serial is not readable yet: REASON`, on the daemon's error output), the gadget's state
reads `the board serial the gadget's addresses come from is not readable yet: retrying`, and the gadget
retries on its backoff until the source can be read, with no restart. The daemon reads the source once
at start as well, for the gadget's health note only (`no USB serial string`); a gadget is no longer refused
for a serial that could not be read at start. The note does not stay stale: a successful `setup` replies
`ok serial` or `ok no-serial` (the closed set; any other reply to `setup` is a failed setup, named), from
the serial string the helper finds in the tree it made or adopted, and the driver clears the note on
`serial`, and sets it (the start's reason) on `no-serial`, at every setup.

**A tree that is already there.** `setup` looks at `usb_gadget/lemnos-NAME` before it makes anything.
Nothing there: it is made. The same content as the board file asks for (every attribute, function, link and
the mount): it is adopted as it is, bound or not, with one line on the error output. The same name and a
different content with every part of a kind the helper makes, and unbound: it is taken down and made again.
The same, and bound: it is left, and the setup fails with `EBUSY` (`the gadget tree lemnos-NAME differs from
the board file's (WHY) and is bound to a controller: not remade while bound`); the daemon unbinds the
gadget, as it does before every attach, and asks once more. A part the helper never makes (a function other
than `ffs.NAME`, `ncm.usb0` or `acm.GS0`, a configuration not named `c.N`): the tree is not the helper's,
it is left alone, and the setup fails with `EEXIST` and names the part (`a gadget tree named lemnos-NAME is
there with a function mass_storage.0, which this helper never makes: it is not the helper's, and is left
alone`).

**How a gadget uses it.** The gadget's driver asks for the gadget's bind as before. Each time it enables,
the gadget asks for `setup` first, and the step's errno decides what the driver reports (a missing
controller is "not there", retried). A failure is retried on the driver's backoff: 1 s, then 2, 4 and 8 s,
and every 8 s after, with no automatic off. The first wait and the cap are the gadget's `retry_first_ms` and
`retry_cap_ms` settings ([Board files](boards.md), the gadget's keys), 1000 and 8000 when not set. A `udc:NAME` uevent (a controller coming up or switching to the
device role) brings the next attempt forward, so the gadget does not wait out its timer when the hardware
changes. The gadget's state shows the driver's fixed text for that kind of failure. Each failed attempt is one
line on the daemon's error output, with the step and the helper's reason. The gadget's state shows the attempt
count, its last reason and the next try (`retrying: attempt N, last reason: ..., next try in M ms`, see
[`list`](#list)): the driver reports them through `Lifecycle::retrying` (`engine/core`), which the host reads
beside the health text. The helper's own errno reason is not in that line: it is on the error output.

**Not on the board.** The helper's behaviour on a real controller (the controller appearing late, host-role
then device-role, configfs not mounted at start) is untested on board: [`testing/board-runs/V1g.md`](../testing/board-runs/V1g.md).
The QEMU check (`testing/crates/gadget-check`) takes `dummy_udc.0` away before the daemon starts, shows the
gadget retrying while the daemon serves its other devices, and gives the controller back.

## Running under systemd

`packaging/systemd/lemnosd.service` starts the daemon as root and runs it with `--user lemnos --group
lemnos`, so it drops to `lemnos` itself. The device nodes are given to group `lemnos` by
`packaging/udev/70-lemnos.rules`. Its one kept capability is `CAP_SYS_RAWIO`, for register mapping
through `/dev/lemnos`. The unit starts only when the OS has
supplied the board file at `/etc/lemnos/board.toml`. The OS installs the unit files and supplies the
`lemnosd` binary and the board file. The unit restarts the daemon after a constant 1 s and sets no
`WatchdogSec=`: the hardware watchdog escalates, as it does without systemd. [`packaging/files.txt`](../packaging/files.txt) lists the install
paths, and [`packaging/README.md`](../packaging/README.md) says what each unit needs and what it does not. The USB gadget driver writes configfs, which only root may do, so a board with a gadget needs a drop-in that runs the daemon as root. The PWM driver is different: its channels are granted by the root phase (`pwm channels` in `packaging/README.md`), and the daemon stays `lemnos` under the unmodified base unit.

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
- **Before lemnosd.** On a slot board, slot-init opens the same device early in the boot, arms it at
  15 s and feeds it until it hands over (`docs/slots.md`, "The watchdog chain"). The device is never
  unarmed between slot-init and lemnosd: on the boot's first start, lemnosd's root phase opens the
  keep-alive watchdog as root and feeds it once, which ends the unfed hand-over, and the bind takes
  that descriptor; the next feed is the first healthy tick, within one period. A restart takes back the
  descriptor the service manager kept for it, and feeds nothing: opening a running watchdog feeds it, so a
  daemon that dies after its root phase must not be fed again by its next start (`docs/slots.md`, "The
  watchdog chain", and `packaging/README.md`, "The hardware watchdog").
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
file links it to another device, then the reason in brackets when there is one, then one `job 0xID running 40%` for each long job open on the device (`, ` between them; the percent is left out while the driver has not said a total). The job comes from the host's job table, so it shows from any session, not only the one that started it, and `list` does not enter the driver to say it. A session that did not start a job can only read it here: it cannot wait for it or cancel it (see "What a session that did not start a job can do with its id"). A parser that reads the first columns of a row reads them as before; the rows of a device with no job are byte for byte what they were. A device that is
retrying (backed off by the host, or a driver that retries a step itself) has this reason:

`retrying: attempt N, last reason: TEXT, next try in M ms`

`N` is the failed attempts in a row, the last one included. `TEXT` is why the last one failed, cut to
160 characters with `...` at the end when longer. `M` is the time to the next try, zero when it is due.
A device switched on has no attempt to show, so its reason is the word alone (`switched on`). The same
text is the reason in `describe` and `show`, which read the state and reason of the device.

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
`value`, `control` or `action`) with its unit and range, then a last line `job 0xID running 40%` for each long job open on the device (none when there is none). A session that did not start a job can only read it here: it cannot wait for it or cancel it (see "What a session that did not start a job can do with its id").

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

A device that is not running has no values to read: it is off, or retrying its bind or its
start-up. Then `read` (and `watch`) prints nothing and fails with the device's state and reason on
stderr, and exits non-zero:

```sh
lemnos-ctl read mag
# mag is retrying: attempt 3, last reason: bind failed: fake refusal: absent, next try in 1982 ms
```

```sh
lemnos-ctl read accel
lemnos-ctl read counter count
```

### watch DEVICE ITEM [--period-ms N] [--count N] [--wait-ms N | --wait-forever]

Subscribes to a value and prints each one as it arrives. The period is 100 ms unless given. It runs
until stopped unless `--count` says how many to print.

A device that answers "try again" (AGAIN) has no value yet: it is powering up, or it has stopped
answering. `watch` then says so once on stderr, with the device, the item and the wait, and prints
nothing on stdout. It stops once the device has answered "try again" for the wait with no value,
and exits non-zero: `DEVICE ITEM: no value in N ms; the device kept answering "try again"`, with
`after K values` in it when some were printed first. Without `--wait-ms` the wait is 10 s, or 50
periods of `--period-ms` when that is longer: 10 s covers a device that comes up on the sixth retry
of the drivers' own schedule (the first at 100 ms, doubling, capped at 8 s). `--wait-ms N` sets the
wait instead, and `--wait-forever` waits without one, for a device that may take longer. Each wait
that starts after a value is announced again. A device that answers "try again" for less than the
wait and then gives values prints exactly those values.

```sh
lemnos-ctl watch accel z --period-ms 50 --count 3
lemnos-ctl watch accel z --wait-ms 2000 --count 3
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
A device that is not running (off, or retrying its bind or start-up) refuses a call, and the refusal
says its state and reason, as `read` does, rather than only that the call failed.

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
| `accelerometer/1`, `gyroscope/1` | `read_batch`, `read_batch_stamped` | the caller's buffer | whole; samples that do not fit wait for the next batch |
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
| `system-slots/1` | `info` | about 1.5 KiB (`last_attempt` adds up to about 0.8 KiB: a reason of 192 bytes appears in `reason` and again in `text`), plus the owner of the last signing key (the key file's text, under 2 KiB), plus one `key_ids` entry per held key (64 hex digits and its owner) | `BUFFER_TOO_SMALL`; a 4 KiB buffer holds any owner the key file allows in practice and a board's few keys |
| `system-security/1` | `state`, `precheck`, `set`, `request_off` | 1024 bytes (`TEXT_MAX`) | whole |
| `system-security-precondition/1` | `precondition` | 1024 bytes (`TEXT_MAX`) | whole |
| `raw-bus/1` | `i2c_transfer`, `spi_transfer` (`read_len`), `uart_read` (`max_len`), `i2c_transactions` and `spi_transfers` (their reads, summed) | the caller's length, up to 4 GiB | a length above 61440 is refused before the transfer; ask for less and repeat. A list is at most 8 ops or transfers (`MAX_I2C_OPS`, `MAX_SPI_TRANSFERS`); more is `INVALID_ARGUMENT` |
| `usb-function/1` | `control_out` | the pending request's `length`, up to 65535 | not checkable up front (no length argument). The USB driver refuses a buffer smaller than the pending request's `length` with `BUFFER_TOO_SMALL` before it asks the device, so nothing is transferred and the request stays pending; the bridge names the refusal. Tested on the mock (`drivers/usb-gadget/src/serve_tests.rs`) |
| `usb-function/1` | `data_stream` | 32 bytes (`MAX_STREAM_NAME`) | whole |

A length the caller gives (`read_len`, `max_len`) is checked before the call is made, so a bus
transfer with a length too large for the reply is never started. `lemnos-ctl`, on the board's own
socket, carries a 1 MiB frame, so the same calls go through there up to that size.

**Watch.** `lemnos-ctl watch` takes a method with no arguments and no out buffer. The daemon serves a
subscription only to a method with no arguments, and to a method with an out buffer when it declares a
reading: the buffer is then the value (a colour array, such as a light's `current`, or events, such
as a raw device's `read_edges`). `watch` on a method
with arguments, or with an out buffer and no reading, is refused by name before it is asked: the method
takes arguments or an out buffer, so call it instead. A colour array is read at most once per 20 ms
(`FRAME_PERIOD_US`): a faster period is granted as 20 ms.

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

**When a client gets `BUSY`, and when it cannot.** Every call to one device, and its tick, runs on the device's lane one at a time. A client that finds the driver in a tick or in another call waits its turn, for at most the call deadline, and is not refused for it. That holds for `describe` (and so for opening a device, which describes it), for a method call, a value read, and starting a job. `BUSY` is never the answer to "the driver is busy right now". It is the answer only to a deliberate rule: a second consumer or producer of a stream (one at a time), a second caller's hold on a USB function's endpoints, an intrusive test while another caller uses the device (see [Drivers](drivers.md)), and a job started while the daemon's job table is full. A long job does not change this and holds nothing: starting one is a short call that returns its number, and the driver does the work a step at a time from its tick, each step short (`Lifecycle::job` and `tick` "must not block"). Between steps the device is free, so a describe, an open, a value read, a second call and `list` all answer at once while an install or a flash runs; only a step in progress is waited for, like any tick. `list` does not enter drivers at all. That a job is open, with its number and progress, is in `describe` and `list` too (see the `job` record), from the host's table, so an app in another session can show "installing, 40%" without a call to the driver. A driver that does not come back within the call deadline gives `TIMED_OUT` and is contained (dropped and bound again); a device with no running driver gives `NO_DEVICE`, and its description is then made from its manifest.

**What a session that did not start a job can do with its id.** It can read it: the id, the state and the progress are in `list` and `describe` for every session. It cannot do anything else with it. Progress and the end of a job (`JobProgress`, `JobDone`) go to the connection that started it and to no other, so `wait_job` and `take_job_updates` of another session never see the job: `wait_job` answers `NOT_FOUND` at once, and its progress callback is never called. `cancel_job` from another session is refused with `NOT_PERMITTED` (only the starter, or the host's own local caller, may cancel); the job is not touched by either. The job belongs to its starter's connection: when that connection ends the host cancels the job (the driver is told with `job_cancel`), and a service restart ends every job as `INTERRUPTED`; a job is never kept running for a session that has gone, and the id of a job that ended is gone from every table. `lemnosd` has no detached or re-attachable job: pinned by `a_session_that_did_not_start_a_job_can_see_it_but_not_wait_for_it_or_cancel_it`, `only_the_starter_cancels_a_job` and `a_starter_that_goes_away_has_its_jobs_cancelled` in `service/lemnosd/tests/jobs.rs`. Through the Orion bridge a job is the bridge's own connection's: each Orion caller has a connection of its own, and a run that is a long job is started on it and ends as the job does. A reconnected app therefore cannot re-attach to an install by the job id. Re-attaching through Orion's run is not something this repository's code does either: the bridge keys a run by Orion's session and call, drops an answer for a session that has ended, and has no code that hands a run to a new session; a detached run is not cancelled when its session ends, so the job goes on and the bridge keeps its caller's holds until it ends (see the lease above), and a session connected after the starter's end re-attaches to it through Orion's `Actions::attach` (Orion's code; the bridge does nothing for it): it sees the run's progress, and its cancel ends the run as `Cancelled` to that session. Pinned by `a_detached_run_is_re_attached_from_a_session_connected_after_its_starter_ended` in `service/orion/tests/orion_bridge.rs`. What an app can do today from a new session is show that an install is running and how far it is, from `describe` or `list`, and, once the install has ended, how it ended from `system-slots`' `info` (`last_attempt`: `installed`, `refused`, `cancelled`, `interrupted` or `failed`, with the reason the starter was given, `lemnos-ctl call slots info`; `docs/slots.md`, "The last install attempt").

A refusal carries a status and a few words: `refused (-16): ...`. The numbers are negative Linux
error numbers, and Lemnos's own codes below -4095:

| Status | Name | Usual meaning |
|---|---|---|
| -1 | `NOT_PERMITTED` | The device is host-owned and the method would change it |
| -2 | `NOT_FOUND` | No such device, method, stream or resource name |
| -5 | `IO` | The hardware did not answer |
| -11 | `AGAIN` | Not ready yet: a chip still powering up |
| -16 | `BUSY` | Someone else holds the thing asked for (see "When a client gets BUSY"). Ask again |
| -19 | `NO_DEVICE` | The device is not running |
| -22 | `INVALID_ARGUMENT` | A bad setting or argument |
| -95 | `NOT_SUPPORTED` | The driver or the hardware does not do this |
| -110 | `TIMED_OUT` | The call ran past its deadline: 2 s, or the board file's `call_deadline_ms` |
| -4099 | `PANIC` | The driver panicked in the call. It has been dropped and will be bound again |
| -4100 | `LAYOUT_MISMATCH` | A frame stream's shared memory (its ring header or its marks page) is of another layout than this build reads. The message names the structure and both values; the C surface reports them with `lemnos_layout_refusal` |
| -4101 | `TORN` | A frame's buffer was reused while the frame was being read, so the bytes read were dropped and nothing was returned as the frame. Only a reader the producer cut off can see it; its next call on the stream says so. Comes from a frame stream's read, never from an action: no action ending carries it |
| -4102 | `FAILED_PRECONDITION` | The call is valid, but the device is not in the state it needs first: a step it depends on has not been taken (a mode not selected before a start or a setter). Nothing was changed. Not `INVALID_ARGUMENT` (a bad value) and not `BUSY` (in use). Added with DEBT-A2; an older peer sees the number and carries it as unknown. |

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
The bridge connects to the comm core with Orion's default patience: it keeps trying, paced by Orion
(never longer than 250 ms between tries), until the comm core welcomes it or refuses it, so it needs
no loop of its own for a comm core that starts later. The first failure is logged, and a refusal is
retried on the bridge's backoff (1 s, doubling, capped at 8 s). A stop of the service ends the trying.
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

The bridge offers the `lemnos/` prefix to Orion with `Files::offer` (at the pinned revision,
`crates/client/files/src/offer.rs`). `named::run` runs on its own thread with its own Orion session and
serves each file asked for from a snapshot. A name outside `lemnos/` is refused before lemnosd is reached.
A name the node refuses, or a node that cannot be reached before the file begins, is refused to the client
with its reason. A file that is given is digested first and then given from the client's offset. If the
node fails once the gift has begun, the read returns an error, Orion aborts the gift and the client's
fetch ends at once as `Aborted`, with the reason (the pinned revision 172054f4; 927f704a at I43). The bridge logs it. Only a
broken connection is left to the client's own silence limit, since it cannot be told. A failed or ended offer is taken
again on the bridge's backoff (1 s, doubling, capped at 8 s); nothing gives up, and the state and the log
keep the reason.

| Lemnos | Orion |
|---|---|
| a device | the resource `lemnos.<device>`, of the kind `lemnos.device`, with the labels `lemnos.driver`, `lemnos.tag` (one per tag) and `lemnos.control.<item>` (the shapes a control or action item takes) |
| its state and health | the resource's state: `Ready` while it runs, `Failed` while it retries or reports failed health, `Pending` while it is off |
| its value items | the topic `lemnos.<device>.values`: a field per item that is a number (a float in the item's unit, scaled by its exponent) or a flag (a bool), published as it changes |
| its events | the topic `lemnos.<device>.status`: `state`, `health`, `reason` and `refusal` as text, as `lemnos-ctl list` names them |
| `lemnos-ctl set`, `call`, `on`/`off` | the actions `lemnos.<device>.set` (`ITEM VALUE...`), `.call` (`METHOD ARG...`, the reply is the output, as text) and `.power` (`on` or `off`), with the arguments as text, the words `lemnos-ctl` takes. `.call describe` with no arguments gives the device's description (below). Programs on the board and operators granted them may run them |

**Topics and how each is read.** Every topic the bridge publishes is a state: its newest sample is what
matters, and it is current until the bridge publishes a change or withdraws the topic. Each is declared
`OnSubscribe::Latest` (the mode `publish::on_subscribe_for` gives a state), so a subscriber sent samples
as messages, on another board or in a browser, is given the newest sample when it starts. None is an
event today (no topic carries each job progress or frame; progress goes with the run). Each
state is a latest-value topic (history 2). The table is in code as `publish::CLASSES`.

| topic | kind | expiry | history | what it says, and why it is current until changed |
|---|---|---|---|---|
| `lemnos.<device>.values` | state | zero | 2 | a device's readings: published as each changes; withdrawn with the device |
| `lemnos.<device>.status` | state | zero | 2 | `state`, `health`, `reason`, `refusal`: published as each changes; `gone` when lemnosd stops listing the device. `refusal` is why a device waits, in decimal: `0` none, `1` absent, `2` failed, and any other number as lemnosd sent it: the bridge passes a number it does not name on unchanged, so a reader can show it as an unknown reason with its code |
| `lemnos.<device>.description` | state, a generation | zero | 2 | the device's description generation (an `int`): it moves when what `describe` says of the device changes (its state or reason, which the description carries). A client that sees it move asks `describe` again |
| `lemnos.devices` | state, a generation | zero | 2 | the device set's generation (an `int`): it moves when a device comes or goes |
| `hello.<fact>` | state | zero | 2 | a board fact: published when it changes; withdrawn when it goes or the session ends |

The hello document also carries Orion's own facts, `state`, `recovery_why`, `security` and
`security_generation` (at the pinned revision, Orion's `OWN_FACTS`). The bridge does not publish them and
does not name them; a client reads them from the node, as Orion gives them.

**Expiry.** Every state is declared with an expiry of zero, which is "valid until changed". The
device topics are declared through Orion's resource `topic()`, which sets zero whatever is asked, so
zero is the only expiry a device topic can have at this rev. That is deliberate: the bridge publishes
each change and withdraws the topic when the device goes, so a sample is never served as current
after it went stale. The facts are declared directly and get zero for the same reason.

**Late subscribers.** The mode is chosen per kind of topic by one rule, `on_subscribe_for` in
`service/orion/src/publish.rs`, and every declaration takes it from there. A state topic (a device's
values, status and description, the device set's generation, a board fact) is declared with
`OnSubscribe::Latest`: a subscriber that arrives after its last sample gets that sample at once, then
what is published from then on. An event or stream topic is declared with `OnSubscribe::FromNow`: it
does not replay, and a subscriber gets what is published from then on. The bridge does not republish
on a timer.

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

**The values.** A number is bare: decimal, or hex with `0x` (an address and a hash). A real number is its shortest round-trip form (`-1.5`, `-0.0`, `1e300`). A number the device does not give is an absent key, never a placeholder, and a real number that is NaN or infinite is absent too. An enum word is bare: `running`, `retrying`, `off`, `unknown`, `known`, `dimensionless`, `value`, `control`, `action`, `number`, `range`, `switch`, `text`, `colour_array`, `three_axes`, `events`, `true`, `false`. A text option the device does not set is the bare word `none`; the quoted `"none"` is the text `none`.

**The records.**

- `device generation lease_seconds label kind bus address location state reason live refusal`. `generation` is the device's description generation, the number its `lemnos.<device>.description` topic carries, and is absent before the device has one. `lease_seconds` is the bridge's caller lease (the hello fact `lease.seconds`), absent when the lease is off. `address` is `0xNN` or `none`. `state` is `running`, `retrying`, `off`, or `unknown` when none is given, and `reason` says why the device is not running or what its last failure was (empty when it runs). `live` is `true` when the device's running driver gave the rest of the description, and `false` when the device is not running (off, retrying, failed or never started). `refusal` is why the device waits, as the wire numbers it: `0` none, `1` absent (no such device here), `2` failed (present but not working), and any other number as it came. It is the record's last key, after `live`, so a reader from before it ignores it.
- `contract index tag kind hash methods`. `index` is the contract's position, which an item's `contract` names. `hash` is hex. `methods` is the count of the `method` records that follow for this contract: those are the records whose `contract` is `index`.
- `method contract id name params returns out_buffer job`. `params` is each `name:shape`, joined by a space, as one quoted text. `out_buffer` and `job` are `true` or `false`.
- `job id state done total percent`: one for each long job open on the device, after the items, and none when there is none. `id` is the host's number for the job, in hex, the one `start_job` gave; `state` is `running`, the only state of an open job (a finished one is gone from the table); `done` and `total` are the driver's last progress in the contract's unit (bytes for a slot install or a flash write, steps for others), both 0 until it gives one; `percent` is the whole percent `done` is of `total` and is absent while `total` is 0. They come from the host's job table, never from the driver, so a device in a step still says them. At most 64 `job` records (the host's limit on open jobs), so a description grows by at most 64 short lines. A reader from before the record ignores it (the grammar skips an unknown record word), and the wire form carries the same list appended to the identity block, which an older reader skips. Example: `job id=0x6a1b00000001 state=running done=41943040 total=104857600 percent=40`.
- `item kind name unit unit_state exponent min max real_min real_max real_step real_warn_low real_warn_high step warn_low warn_high contract method field reading group`. `min`, `max`, `real_min`, `real_max`, `real_step`, `real_warn_low`, `real_warn_high`, `step`, `warn_low` and `warn_high` are absent when the device gives no such bound. `field` and `group` are quoted or `none`. `reading` is one of the reading words, or `unknown`.

**Devices that are not running.** lemnosd answers a description for every device in the board file, whatever its state. A device whose driver is not running (off by the operator, retrying a bind that keeps failing, failed, or never started because the board file disables it) has no driver to describe it. Its device record still comes, with its `state` and `reason` and `live=false`. Its contracts and methods come from the driver's manifest, which lemnosd has for every device, and it has no `item` records, no address and no location: those are what a running driver reports. The generation moves when the state or the reason changes, as it does for a running device. Those are the only answers that differ.

**Which records each state carries.** The description has the same records for every device; the state decides what is in them.

| device's state | `device` record | `live` | `contract` and `method` records | `item` records | `address` and `location` |
|---|---|---|---|---|---|
| `running` (driver bound) | yes, with `state=running`, `reason=""` | `true` | from the driver's manifest | from the running driver | from the running driver |
| `off` (switched off by the operator, or by the board) | yes, with `state=off` and the reason: `disabled` (`disabled = true` in the board file), `manual` (an operator's `off`), `policy` (the board's auto-off), or `failed(status(N))` (a driver that keeps its hardware could not be switched off; `N` is the status its `off` returned) | `false` | from the manifest | none | `none` and empty |
| `retrying` (the bind failed or missed, or its settings were refused) | yes, with `state=retrying` and the reason the bind gave | `false` | from the manifest | none | `none` and empty |
| `unknown` (no state given) | yes, with `state=unknown` | `false` | from the manifest | none | `none` and empty |

The contract and method records come from the manifest because lemnosd has it for every device in the board file, so a device that does not run still says what it offers. A device with no manifest is not in the board file and is not described (`NO_DEVICE`).


**Bounds.** A reader takes at most 60 KiB of text, 4 KiB per line, 4096 records, 64 keys per record, 256 `contract` records, 2048 `method` records, 2048 `item` records and 64 `job` records. The encoder refuses a description that would pass one of them, by name, with the count and the bound. It never cuts a description short: the bridge sends the whole text or a failure that says why. A text value that holds a control character other than tab, newline or carriage return cannot be written in the format, so the description is refused by name too. `service/orion/tests/description_bounds.rs` describes every built-in driver through this text form and fails if one is over a bound. Measured then: the largest description is 3775 bytes (`ov9782`, 21 lines, 20 records, a manifest-only description since its driver does not bind on the simulated board), and the largest one from a running driver is 2149 bytes (`rp1-csi2`, 17 lines, 10 keys in a record). The longest line is 783 bytes. The most keys in one record is 19 (`pisp-be`). No driver comes near 60 KiB, so there is no paged form: a description that would pass a bound is refused by name. The items of a driver the simulator cannot host are not in those figures, since only a running driver gives items; a running item record is about 150 to 300 bytes, so a device would need hundreds of items to reach 60 KiB.

**Examples.** Two real replies from the walk board, and one for a device whose bind keeps failing. The running one's `location` is shortened to a `/sys` path.

```describe
describe 1
device label="zone" kind="sensor" bus="" address=none location="/sys/class/thermal/thermal_zone0" state=running reason="" live=true refusal=0
contract index=0 tag="temperature/1" kind="sensor" hash=0xc77f0ded2da1235c methods=1
method contract=0 id=1 name="read" params="" returns="f32" out_buffer=false job=false
item kind=value name="temperature" unit="°C" unit_state=known exponent=0 contract=0 method=1 field=none reading=number group=none
```

```describe
describe 1
device label="zone" kind="sensor" bus="" address=none location="" state=off reason="manual" live=false refusal=0
contract index=0 tag="temperature/1" kind="sensor" hash=0xc77f0ded2da1235c methods=1
method contract=0 id=1 name="read" params="" returns="f32" out_buffer=false job=false
```

```describe
describe 1
device label="n-a733-di" kind="compute" bus="" address=none location="" state=retrying reason="bind failed: Status(-2)" live=false refusal=0
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
strip). That is 13 of the 35 out-buffer methods. The success path of `read_batch` and of `control_out`
is not reached through the bridge, since neither has a simulated FIFO or a stream consumer to give
the request.

The other 22 are covered by the text path only (`every_out_buffer_method_of_every_contract_is_callable_through_the_text_path`
in `service/ctl/tests/out_buffers.rs`), because no simulated device can host them:

- `accelerometer` and `gyroscope` `read_batch_stamped`: the simulated devices have no FIFO;
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

**Callers.** Each Orion session has a lemnosd connection of its own, kept under the session's token
(`caller_key`), not under its identity (`local:<uid>` and the others). Two sessions of one user are two
callers to a driver that tracks callers (`TRACKS_CALLERS`), each with its own `CallerId`, and ending one
releases only its own holds. A token of zero (a node that does not say) is one shared key. A cancel
cancels its run and nothing else: the job is cancelled on lemnosd, and the caller's connection and the
driver's holds for it stay.

**Session ends.** Orion says when a session ends (`CallerGone`, at the pinned revision 8e37a8a). Each run
says whether it is detached: `Request::Run` carries `detached` (`Claim.detached` on the wire, false from a node
too old to say), and the bridge takes it as written. There is no inference from the order of messages. Orion
cancels an attached run of an ending session with reason 2 before `CallerGone` (Orion's `gone()` in
`crates/node/action-list/src/lib.rs`), and `calls::caller_gone` ends any attached run still in flight the same
way, so the bridge never ends a run the request marked detached. A session with no detached run in flight is
released as a dropped connection is: its connection closes, and lemnosd tells the drivers the caller is gone
(`caller_gone`). A session with a detached run in flight keeps its connection open, because lemnosd ends a job
with the connection it was started on; the connection is released when the last of those runs ends. A session
with no connection here is a no-op.

A detached run goes on past its caller (E12): its end goes to whoever attached, and `Actions::attach` finds it
by its number (the number the starter last ran under that caller; an earlier run is reached by handle only,
`Actions::attach_handle`). The bridge needs nothing for an attach. A detached run needs a deadline at this
revision. `the_program_is_told_which_runs_are_detached_and_how_each_one_ends` in `service/orion/tests/orion_bridge.rs`
pins the order Orion sends an attached run's cancel in, and what each run's `detached` says.

**Leases.** The bridge also bounds the holds of a caller by a lease, for the case no session end reaches it
(a caller that holds and then says nothing). When a caller has been silent for the lease, the bridge drops
that caller's lemnosd connection, its jobs end with it, and lemnosd tells the drivers the caller is gone
(`caller_gone`), which releases what it held. The lease is `LEMNOS_ORION_LEASE`
seconds, default 60. `LEMNOS_ORION_LEASE=0` turns the lease off: no caller is dropped for silence, and the
bridge says nothing of a lease (see below).

**Only silence expires a lease.** A caller's lease is renewed by each of these, and by nothing else:

- every request of the caller: a `call`, `set` or `power` run, a `cancel` of its run, and a request
  refused for its arguments. A cancel carries Orion's reason (not said, its caller asked, its caller's
  session ended, its deadline passed, or the node is stopping). The bridge logs who cancelled, as words,
  and does not pass the reason to lemnosd or the drivers: they have no use for it, and the lemnosd cancel
  carries none. A session's end (`CallerGone`) is not a renewal: it releases the holds at once;
- the end or cancel of one of its runs, which starts the lease from that moment;
- every progress report of a long job the caller started (the job's progress is news of its run);
- every file or stream chunk the bridge accepts, that is, one it answers as still receiving. Pinned Orion
  does not say which session sent a chunk, so an accepted chunk renews every caller's lease, not only its
  sender's. A dead caller's holds therefore survive while any transfer is being accepted.
  `ORION-R2 (C33)` marks where the sending session is named.

**A caller with a run in flight is alive.** A caller with any run in flight (started by it, not yet
finished and not cancelled) is not dropped for silence, whether or not the run reports progress, so a
healthy silent job (a verify, a self-test) keeps its caller's holds. A cancelled run no longer counts. A
session that ends is not waited for: its `CallerGone` releases its callers at once (see "Session ends").

Not renewed: the bridge's own housekeeping (its publishing of the hello document, its ticks), and Orion's
own traffic to the caller (its topic subscriptions and its pings), which the bridge does not see.

**The note of a lease's end.** The first reply the bridge makes to a caller after its lease ended is served
normally, on a fresh connection, and its `detail` begins with `lease-ended: holds released` (the note, given
once per expiry). The rest of `detail` is free text, as it was: a client matches the prefix `lease-ended:`
and reads the remainder as it reads any detail. A reply carries the note when the caller's run is answered
(its end or cancel included), so a run that began after the drop carries it on its reply. The bridge
remembers at most 64 callers whose note is still due; the oldest is forgotten first. A caller that comes
back after that sees no note, and `leases.ended` still counts the drop.

**A quiet link is not a gone one.** At the pinned revision a local link, the board's own comm core socket
(`Address::Local`), has no silence timeout in Orion's client (C37, `crates/client/connect/src/alive.rs`): a
closed peer shows up at once as a lost session, and silence means a comm core that is busy or stopped. Both
sides keep the session while it is quiet (the comm core marks it quiet), so a stalled but open link keeps its
session: nothing is torn down or published again, and publishing goes on on the same session when the stall
ends. Each request still times out on its own deadline. The bridge adds no probe of its own. A network link (`orion+tcp://`) cannot publish: at the pinned revision the network carrier offers only the
`CLIENT` and `PEER` roles, and the bridge publishes as a provider. A provider role over the network carrier is
not offered, by design at this pin. So the bridge refuses `orion+tcp://` by name at
start (`ORION_ADDRESS`, below), and if it is handed one only at connect, the comm core's refusal is said once and the
bridge stops; that is a configuration error, not a link fault, so it is not retried and the process exits with a
failure. The silence rule still holds for a network session a remote caller holds: its session is lost after the
idle time, which is three heartbeats at the comm core (`comm.idle_ms`, or three `comm.heartbeat_ms` when zero). These
are tested through a relay on 127.0.0.1 that stalls, resumes, closes and resets the link
(`service/orion/tests/support/tcp_relay.rs`), in `service/orion/tests/tcp_reconnect.rs`, with the threshold and its
source lines in that file's header. A remote caller holds through the bridge on its local socket, and the tests
assert on the bridge's side:

- `a_silent_tcp_caller_is_ended_and_the_bridge_releases_its_holds_and_cancels_its_run`: the node ends a silent
  caller's session, the bridge releases its holds at lemnosd and cancels its attached run, so the job is gone;
- `a_short_tcp_stall_below_the_silence_check_keeps_the_caller_its_hold_and_its_run`: a stall of one heartbeat keeps
  the caller's hold and its job, and the caller answers when the link carries on;
- `a_closed_tcp_caller_releases_its_holds_and_its_run_is_cancelled`: a link the relay closes releases the caller's
  holds and cancels its run, and the caller's session is lost;
- `a_tcp_caller_reset_with_unread_bytes_releases_its_holds`: a link closed with bytes unread is reset by the kernel,
  and the caller's holds are released;
- `a_bridge_given_a_carrier_that_cannot_publish_is_refused_once_and_stops`: the connect-time refusal is stated once
  and the bridge stops.

Not tested over TCP: a network caller's own silence check in its session when it is not pumping (only the
short-stall case and the pumping callers exercise the client's check). Orion may add a `silent_for()` accessor later
(it needs the consumer's OK); nothing here waits on it.

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

A caller that is detached from its run is not told apart from a live one by the lease (the lease does not
read the detached flag): its holds last while its run's progress does, and a session's end releases them at
once, detached or not. Power and the boot holds of VBUS are not caller holds and are not released by a lease.

**How a run ends.** A run ends with an Orion outcome and an action status code (`finish_with_code`, Orion
C44). A status code the bridge names is the magnitude of the lemnosd status (the Linux error number), so a
caller branches on the code and reads the words in `detail` for people. Zero is none given. The table is the
one in `ending_of_status` (`service/orion/src/calls.rs`); each row has a test.

| lemnosd status | number | Orion outcome | code |
|---|---|---|---|
| `CANCELLED` | -125 | `Cancelled` | 125 |
| `TIMED_OUT` | -110 | `TimedOut` | 110 |
| `NOT_PERMITTED` | -1 | `Rejected` | 1 |
| `NOT_FOUND` | -2 | `Rejected` | 2 |
| `AGAIN` | -11 | `Rejected` | 11 |
| `BUSY` | -16 | `Rejected` | 16 |
| `NO_DEVICE` | -19 | `Rejected` | 19 |
| `INVALID_ARGUMENT` | -22 | `Rejected` | 22 |
| `NOT_SUPPORTED` | -95 | `Rejected` | 95 |
| `BUFFER_TOO_SMALL` | -4098 | `Rejected` | 4098 |
| every other status: `IO`, `INTERRUPTED`, `NO_MEMORY`, `ACCESS`, `STALE_HANDLE`, `VERSION_MISMATCH`, `PANIC`, `LAYOUT_MISMATCH`, `TORN` (no action returns it: a frame read does), and any other | | `Failed` | 0 |

The rows above are the statuses lemnosd answers a call or a job with. The bridge's own endings have no
status and so code zero:

| The bridge's ending | Orion outcome | Code | When |
|---|---|---|---|
| succeeded | `Succeeded` | 0 | a call's reply, or a job's result |
| failed | `Failed` | 0 | lemnosd is not there before a call, a job ran and ended with a status that is not in the table (a job's `Rejected` reading does not apply: it ran) |
| indeterminate | `Indeterminate` | 0 | how the device-side work ended is not known: a cancel or a deadline stop that lemnosd did not confirm within 5 s, a cancel that was refused or could not be sent, a job whose connection went with it, or a call that got no answer in time (its detail says so; the run is never reported as cancelled or as done) |
| refused | `Rejected` | 0 | the bridge refused the arguments before any call: not what the device takes, no such item, a read-only item |
| cancelled | `Cancelled` | 0 | lemnosd says the job was cancelled: the caller cancelled the run, or its session ended (Orion's reason 2), and the cancel stopped the job |

A long job that ends takes the table's endings for `CANCELLED` and `TIMED_OUT` (its job was cancelled or timed
out on lemnosd), and `Failed` with code zero for any other status: it ran, so a refusal does not apply. A job the
bridge cancelled at its own deadline, which lemnosd confirms as cancelled, ends `TimedOut` with code 110.

**A cancel reports what lemnosd says the job did.** The bridge sends the cancel to lemnosd and keeps the run open
until lemnosd's own end of the job arrives. The run then ends as that end says: `Cancelled` when the cancel stopped
the job, or as the job really ended (`Succeeded` with its result, or `Failed`) when it got there first, the
driver's own rule. The run is never reported as cancelled before lemnosd answers, so an install that completed
is never reported as cancelled, and a cancelled one is never left installed. A cancel that lemnosd refuses (other
than `NOT_FOUND`, which means the job is already over and its end is on its way) ends the run as `Indeterminate` at
once. Lemnosd has up to 5 s to answer (`CANCEL_WAIT`); past that the run ends `Indeterminate`, with words that say
it is not known how the job ended. The bridge's 5 s, and its 2 s headroom, end inside Orion's 10 s confirm window
(`CONFIRM_MS`); a node configured with `actions.confirm_ms` below 7 s may end such a run itself first.


**What bounds a run.** Each kind of run has its own bound, and the lease does not bound any run: a caller with a
run in flight is never dropped for silence (see "Only silence expires a lease").

| Run | Bound | Set by | The caller sees on expiry |
|---|---|---|---|
| `call`, `set`, `power` | lemnosd's call deadline, 2 s by default, per board with `call_deadline_ms` | lemnosd | `TimedOut`, code 110 |
| a long job, its start | the same call deadline | lemnosd | `TimedOut`, code 110 |
| a long job, once started: its stall bound | the bound its method declares (see "The stall bound of each job method"), renewed by each progress report. lemnosd ends a job silent past it as `TIMED_OUT`, and its driver is told to stop it | lemnosd host | `TimedOut`, code 110, words `it went too long without progress` |
| a long job, its caller's deadline | `Run.deadline_ms`: the bridge cancels the job when it passes, and so does Orion's node. The earlier of the deadline and the stall bound ends the run | the bridge (and Orion's node) | `TimedOut`, code 110, words `it ran out of time` |
| a long job, cancelled | the caller's cancel, or its session ending (Orion cancels its attached runs). The run ends as lemnosd says the job ended (see "A cancel reports what lemnosd says") | caller, Orion | `Cancelled` when the cancel stopped the job, else the job's own ending; `Failed` when lemnosd does not answer within 5 s |
| file install | a partial file is deleted after 600 s with no chunk | the bridge | the sender is answered `Rejected` with words `ran out of time` (see below) |
| flash stream | the stream's write is cancelled after 600 s with no chunk; the commit waits up to 900 s for the device's job | the bridge | the sender is answered `Rejected` with words `ran out of time` |

**The stall bound of each job method.** A job method declares how long it may go without progress, in the
contract: `#[method(N, job, stall = SECS)]`. A job method with no bound does not build, and a packaged driver
carries its bound in the method's flags (`METHOD_STALL_SHIFT`). The host ends a job silent past its bound as
`TIMED_OUT`, with no result, and tells its driver to stop it. Each progress report restarts the bound. The host
looks at a job when its bound passes, even when its driver does not tick and nothing calls it, so a silent job
ends on time. A progress report the host has not yet seen counts from the look that sees it, so a job is never
ended sooner than its declared bound, and may be ended up to one look after it. The bound is host policy and is
not in the contract's hash.

| contract | job method | stall bound |
|---|---|---|
| `system.flash` (1) | `write` (the flash image, streamed) | 900 s |
| `system.flash` (1) | `verify` | 600 s |
| `self-test` (1) | `run` | 600 s |
| `system.slots` (1) | `install` (streamed or a file in `.incoming`) | 600 s |
| `system.slots` (1) | `install_file` | 600 s |

The stream and file bounds above (600 s with no chunk) end a transfer before the write's own bound of 900 s, so
a silent sender is answered first. A test-only contract in the repository declares 60 s and does not ship.

**Files and streams that run out of time.** Orion's file states have no timed-out state, so a transfer whose
bound passes is answered to its sender as `Rejected`, with the words `ran out of time: nothing came for N seconds,
so the file was deleted` (or `... the write is cancelled` for a stream), and its file or device job is ended. The
code 110 is not sent: `FileStatus` has no code, and at the pinned Orion revision it has no additive section to
carry one (see "The session note" below).

**The session note: typed in the bridge, not yet on the wire.** The bridge's reply to a run carries the lease's
end as a typed `SessionNote` (`HoldsReleased` or `None`), present on every run's reply whatever its outcome. The
code is the outcome's, never the note's. Orion's reply frames at the pinned revision cannot carry the note yet:
`ActionResult` has an additive section, but none for it, and `FileStatus` has no additive section at all. So the
note still reaches a client only as the prefix `lease-ended:` in `detail` (above), and file and stream answers
carry no note. Carrying it needs an additive field on Orion's `ActionResult` and `FileStatus`, which is Orion's to make.

**A push whose sender is gone.** A file or stream push is not ended when its sender goes away. Orion's file events
name no session (`ORION-R2 (C33)`), so the bridge cannot tell that a sender is gone, and each chunk it accepts renews
every caller's lease. The push ends by its idle bound: nothing arrives for 600 s, the write is cancelled, and the
sender is answered as above. So a stream's write can run for up to that long with no sender. The write runs on the
bridge's own lemnosd connection, not on a caller's, so a caller's lease ending and its release do not touch it; the
device is not released before the bound passes. Ending it sooner needs Orion to name the sending session.

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
hold the device, and the sender is answered `Rejected` with the words `ran out of time`. The bound is not set by the environment. The bridge sees chunks, not reads: Orion's
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
- `ORION_ADDRESS`: an address the bridge can publish through. When it is unset, the board's own
  `/run/orion/orion.sock`; a `unix:<path>` socket and an unsigned serial link can publish. An `orion+tcp://`
  address and a signed serial link (`#key`) cannot: at the pinned revision they offer only the `CLIENT` and
  `PEER` roles, and the bridge publishes as a provider. The bridge refuses them at start, by name, with what to
  use instead; a refusal that only shows at connect is said once and the bridge stops (a configuration error,
  not retried). `publishes_on` (`service/orion/src/config.rs`) has the start-time rule. A `unix:<path>`
  longer than a Unix socket's 107 bytes is refused at start the same way (`ClientError::PathTooLong`, Orion
  C55): a shorter path is needed, and it is not tried again;
- `ORION_NODE_ID`: when it is set, a comm core that is any other node gets nothing;
- `LEMNOS_ORION_FILES`, described above. It has been run against a real Orion node on a PC
([`B20.md`](../testing/board-runs/B20.md) has that run); not yet on a board.

```sh
kill %1
wait
```
