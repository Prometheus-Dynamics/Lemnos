# Packaging

How lemnosd and the Orion bridge run on a board: a systemd unit or a BusyBox init script, one privilege
path for both, the device-node rules, and the settings files. This directory ships the files; an OS's
image build installs them where `files.txt` says. Nothing here names a board or an OS.
`scripts/check-packaging.sh` checks every file, and the gate runs it. It installs nothing, enables
nothing, and starts nothing.

**What an image installs is listed in [`files.txt`](files.txt)**, one line per file under this directory:
its source, the path it installs to, its mode, and when (`always`, `systemd`, `busybox`, `udev`, `orion`,
`gadget`, `kmod`, `uboot`, `ubi`), or `never` for the documents and the lists themselves. A packager reads that
list and nothing else for the files. The programs are in [`shipped.txt`](shipped.txt). The gate's
`scripts/check-packaging-files.py` fails when a file under this directory is not listed, when a listed file is
missing, or when a condition is not in the vocabulary, so the list cannot fall behind the files.

One file a packager installs is generated rather than copied from here: `lemnos-ctl tree BOARD --udev FILE` writes the
board's own device-node rules, for `/usr/lib/udev/rules.d/71-lemnos-board.rules`, beside `70-lemnos.rules`.
It is not in `files.txt`, because it depends on the board, and it is produced by the tree command, not copied
from here (see [docs/boards.md](../docs/boards.md#the-daemons-device-permissions)).

The mdev counterpart is generated the same way: `lemnos-ctl tree BOARD --mdev FILE` writes the
board's own lines, appended to `/etc/mdev.conf` after `init/mdev.conf.fragment` (the packaged kinds,
which stay in that file). Its lines are run against BusyBox 1.37.0 and 1.38.0 in the gate, which is the
version range they are known to work with. It is not in `files.txt`, for the same reason as the udev file.
See [docs/boards.md](../docs/boards.md#the-daemons-device-permissions).

The OS supplies what is not here: the `lemnosd` and `lemnos-orion` binaries at `/usr/bin`, the board file
at `/etc/lemnos/board.toml` (see [docs/boards.md](../docs/boards.md)), and the Orion node's group name
for `lemnos-orion`. The daemon's unit and script refuse to start without the board file.

## One privilege path

Both init systems start `lemnosd` the same way, and it drops its own privileges. There is no file
capability, no `setcap`, and no `User=` or `AmbientCapabilities=` in the units: a binary needs no xattr,
so a read-only or verity-protected root filesystem works.

`lemnosd --board /etc/lemnos/board.toml --user lemnos --group lemnos` (the unit and the script pass
these; `LEMNOSD_USER` and `LEMNOSD_GROUP` do the same from the environment, and a flag wins) runs as
follows:

1. **The root phase**, while the process is root. The privileged start runs every step of
   `STEPS` in `service/lemnosd/src/privilege.rs`, in order, with the board file. The first step is built
   in: it makes the socket's directory, owned by `lemnos`, mode 0750. A step that fails stops the daemon.
2. **The drop**, in `lemnos_linux_sys::privilege::drop_to`: the bounding set loses every capability the
   daemon does not keep; inheritable and ambient are cleared; `PR_SET_KEEPCAPS` keeps the permitted set
   across the change of ids; the supplementary groups, the group id and the user id are set; then the
   effective and permitted sets are exactly the kept capabilities (`CAP_SYS_RAWIO` today), and
   `no_new_privs` is set.
3. **The verification**, `privilege::verify`: the real and effective ids, the groups, both capability
   sets, the inheritable and ambient sets, the bounding set outside the kept capabilities, and
   `no_new_privs` must be what was asked. Any difference is printed and the daemon refuses to run.

Started as a user that is not root, the daemon runs as it is, with no drop, and says which capabilities it
lacks (`CAP_SYS_RAWIO`: a block's registers mapped through `/dev/lemnos` are refused). Started as root with
no `--user`, it runs as root and says so.

**The privileged-start hook, for board-file-driven setup.** It is `STEPS` in
`service/lemnosd/src/privilege.rs`, a list of `Step { name, run }`. `run` is `fn(&Context) -> io::Result<()>`,
and the `Context` holds `board: &BoardFile`, `socket: &Path`, `uid` and `gid` (the ids the daemon will run as,
so a step can `chown` what it creates). A step runs as root, before the drop, and a failure is an error
that names the step. The USB gadget's configfs and FunctionFS setup is the second entry, `usb gadget`
(`service/lemnosd/src/gadget.rs`): it makes each gadget's configfs tree and mounts its FunctionFS with
the daemon's uid and gid, and chowns each `UDC` attribute to it. The daemon then writes `UDC` and ep0
after the drop. Do not add an `ExecStartPre=+` for it: that would be a second privileged path.
The gadget's three unit settings are in an optional drop-in, `packaging/systemd/lemnosd.service.d/gadget.conf`,
not in the base unit. Install it as `/etc/systemd/system/lemnosd.service.d/gadget.conf` on a board whose
board file has a `usb-gadget` device, and only then (its `when` in `files.txt` is `gadget`). The board file is the
source: a `[[devices]]` table with `driver = "usb-gadget"` (see [docs/boards.md](../docs/boards.md)). `lemnos-ctl tree`
does not write this drop-in, and does not say whether a board has a gadget. Its settings: `CAP_SYS_ADMIN` in
`CapabilityBoundingSet` (the FunctionFS mount and its unmount need it; the daemon drops it with the rest
after the root phase, `backends/linux-sys/src/privilege.rs` `drop_to`), `mount umount2` in
`SystemCallFilter` (not in `@system-service`), and `ReadWritePaths=-/sys/kernel/config/usb_gadget`, which
keeps the tree writable under `ProtectKernelTunables`. The QEMU check (`testing/crates/gadget-check`)
runs the daemon's privileged start and reads the capability sets after it. The sandbox itself has not run
on a board (`testing/board-runs/R1d.md`).

**The exposure limit.** `scripts/check-packaging.sh` reads each unit's score from `systemd-analyze security`.
The base units stay at 2.5 with nothing added. The gadget fragment is checked on top of the base unit, and
may add exactly the items in the script's `additions` list, each at the score it has:

| Addition | Score | Why |
|---|---|---|
| `CapabilityBoundingSet=~CAP_SYS_ADMIN` | 0.3 | the root phase's FunctionFS mount needs it; dropped after the root phase |
| `SystemCallFilter=~@mount` | 0.2 | the mount and its unmount; without the capability they fail with `EPERM` |

The check fails if the fragment's combined score is above 2.5 plus the listed scores, if a listed addition is
not in the unit (remove it from the list), if a listed score is not what the unit gives, or if a base unit
has an addition. Other exposure is not allowed. Candidate hardenings that are not in the unit are in
`testing/board-runs/R1d.md`.

The bridge uses the same drop, from its environment: `LEMNOS_ORION_USER`, `LEMNOS_ORION_GROUP` and
`LEMNOS_ORION_GROUPS` (`service/orion/src/privilege.rs`). It keeps no capability.

## The daemon under systemd

`lemnosd.service` runs `lemnosd --board /etc/lemnos/board.toml --user lemnos --group lemnos` as root
until the drop, with `Type=notify`, `Restart=always` and a constant `RestartSec=1s`. It has no
`StartLimit`, and no `WatchdogSec=` (see "The hardware watchdog").

The hardening that still applies to a daemon that starts as root: `NoNewPrivileges`, `ProtectSystem=strict`
(the root phase writes only its runtime directory, which the unit makes), `ProtectHome`, `PrivateTmp`,
`ProtectKernelTunables`, `ProtectKernelModules`, `ProtectKernelLogs`, `ProtectControlGroups`,
`ProtectHostname`, `ProtectClock`, `ProtectProc=invisible`, `LockPersonality`, `MemoryDenyWriteExecute`,
`RestrictRealtime`, `RestrictSUIDSGID`, `RestrictNamespaces`, `SystemCallArchitectures`, and
`RestrictAddressFamilies=AF_UNIX AF_NETLINK`. Two are changed on purpose:

- `CapabilityBoundingSet=` is the five capabilities the root phase needs (`CAP_CHOWN`, `CAP_SETGID`,
  `CAP_SETUID`, `CAP_SETPCAP`, `CAP_SYS_RAWIO`). The daemon removes all but `CAP_SYS_RAWIO` itself after the
  drop, so this list only bounds the root phase. A root-phase step that needs another capability adds it
  here.
- `SystemCallFilter=@system-service @setuid @chown prctl`: `@system-service` lacks the id changes, the
  owner change and `prctl`, which the root phase uses. `capget` and `capset` are already in it.

What no longer applies, and why:

- `User=`, `Group=` and `AmbientCapabilities=` are gone. The daemon's own drop replaces them, and it clears
  the ambient set itself. The daemon keeps `CAP_SYS_RAWIO` by its own `capset`.
- The owner of `/run/lemnos`. systemd still makes the directory (`RuntimeDirectory=`), root-owned, and the
  root phase gives it to `lemnos` before the drop.

`systemd-analyze security --offline=true` now rates `lemnosd.service` 2.5, up from 1.8 before this change.
Systemd scores `User=` and `AmbientCapabilities=` as hardening, and the unit no longer has them. The gate's
limit is still 2.5, so the unit is at the limit, not over it.

## The hardware watchdog

lemnosd must not stay down. A board with a managed hardware watchdog (the `hw-watchdog` driver, on by
default) reboots when the daemon stops feeding it. The daemon feeds it itself, and sets its timeout
(15 s by default). So the watchdog is the escalation on every board, systemd or BusyBox, and the
supervisor's restart is the first step: a restart must come back well inside that bound, which is why
the restart delay is a constant 1 s.

Three rules follow, and the OS must keep them:

- **Do not give the watchdog to systemd.** The device has one opener, which is lemnosd's `hw-watchdog`
  driver. Leave `RuntimeWatchdogSec=` and `ShutdownWatchdogSec=` unset in `/etc/systemd/system.conf`
  (and its drop-ins): either would have systemd open the same device.
- **Do not run a second feeder** on BusyBox: no `watchdog` daemon, and no init step that opens the node.
- **The units set no `WatchdogSec=`.** A software timeout that restarts a hung daemon would race the
  hardware reboot with a second, shorter clock. Dropping it keeps one escalation, identical on both
  init systems. `Type=notify` stays, for readiness only.

The `lemnos` group can open the watchdog node (`SUBSYSTEM=="watchdog"` in the udev rules, `^watchdog`
in the mdev fragment), so the daemon can open it as its own user. Nothing else should.

## Without systemd (BusyBox init)

A small board (64 to 128 MB) runs BusyBox init instead, with the same daemon, the same privilege path,
the same socket, device nodes and watchdog escalation, and `packaging/init/` in place of the units. The
OS installs the files above and does these things, which the units would otherwise do.

**The users and groups** are made by the image builder, not by sysusers. Add these lines (ids are any
free system ids, the same in both files; the example uses 999 for `lemnos` and 998 for `lemnos-orion`):

```text
/etc/group:   lemnos:x:999:
/etc/group:   lemnos-orion:x:998:
/etc/passwd:  lemnos:x:999:999:Lemnos daemon:/nonexistent:/bin/false
/etc/passwd:  lemnos-orion:x:998:998:Lemnos bridge to Orion:/nonexistent:/bin/false
/etc/shadow:  lemnos:!:0:0:99999:7:::
/etc/shadow:  lemnos-orion:!:0:0:99999:7:::
```

The daemon looks the names up when it starts, so the lines must be in place before first boot. Nothing keys
on the ids. The OS adds program users to group `lemnos` as usual.

**The device nodes** are given to group `lemnos` by mdev, not udev. Append `init/mdev.conf.fragment` to
`/etc/mdev.conf`, then the board's own lines, `lemnos-ctl tree BOARD --mdev FILE` (the same permissions as
the generated udev rules, from the same list; see [docs/boards.md](../docs/boards.md)).
Each line of the fragment is the same node as a udev rule: `^lemnos$` is `/dev/lemnos`, `^dma_heap/.*$`
the dma heaps, `^i2c-[0-9]+$` the I2C buses, `^spidev...$` SPI, `^gpiochip[0-9]+$` GPIO, the `ttyS`,
`ttyAMA`, `ttyUSB` and `ttyACM` serial ports, `^video[0-9]+$` and `^media[0-9]+$` for cameras,
`^watchdog[0-9]*$` for the watchdog, and `^bus/usb/.../...$` for usbfs. mdev runs at boot (`mdev -s`) and
on hotplug only if the image sets `/proc/sys/kernel/hotplug` to `/sbin/mdev`.

**The script** (`/etc/init.d/S50lemnosd`) runs with `start`, `status`, `restart` and `stop`. `start` creates
`/run/lemnos` for the pid files, and starts a supervisor as root through `start-stop-daemon`, which it needs. The supervisor runs `lemnosd --board ... --user lemnos --group lemnos`, so the daemon
does its own root phase and drop. A daemon that exits is started again after 1 s, a constant delay, and
never given up on. `stop` is for shutdown: it sends TERM, waits up to 10 s, then KILLs. On a live board it
must not be used, since the hardware watchdog would reboot the board. The script does not feed the
watchdog, and does not depend on systemd-notify: lemnosd sends nothing when `NOTIFY_SOCKET` and
`WATCHDOG_USEC` are absent (`service/lemnosd/src/notify.rs`).

**The bridge's script** (`/etc/init.d/S51lemnos-orion`) runs after lemnosd. `start` waits up to 30 s for
lemnosd's socket, and does not start the bridge without it (it says so and returns 1). The supervisor
then runs `lemnos-orion` as root, which drops to `LEMNOS_ORION_USER` and its groups from
`/etc/lemnos/lemnos-orion.env`. The restart and the bounded stop are the same as the daemon's.

**What is lost without systemd** is the unit's hardening and bookkeeping, and the privilege path is the
same on both:

- Every hardening option listed under "The daemon under systemd": `ProtectSystem=strict`, `ProtectHome`,
  `PrivateTmp`, the `Protect*` set, `LockPersonality`, `MemoryDenyWriteExecute`, `RestrictRealtime`,
  `RestrictSUIDSGID`, `RestrictNamespaces`, `SystemCallArchitectures`, `RestrictAddressFamilies`,
  `SystemCallFilter` and `SystemCallErrorNumber`, and `NoNewPrivileges` from systemd (the daemon sets
  `no_new_privs` itself). The bounding set is the daemon's own, not the unit's.
- `sysusers` (the users are made by the passwd lines above), `tmpfiles` (none needed), and `RuntimeDirectory=`
  (the daemon makes its directory in the root phase, and the script removes the socket at stop).
- Logging to the journal: the daemon's and the bridge's error output go to `/var/log/lemnosd.log` and
  `/var/log/lemnos-orion.log`.
- `ConditionPathExists=` for the board file: `start` checks it instead.

Nothing else changes: the privilege path, the watchdog escalation, the restart and the device groups are
the same on both. The sanity checks in `scripts/check-packaging.sh` run `sh -n` on every init script,
`busybox sh -n` when BusyBox is installed, and `shellcheck` when it is installed. None of them runs a
script. The init scripts and the mdev fragment are untested on board.

## The Orion bridge

`lemnos-orion` is a client of lemnosd and of the Orion node. It starts as root and drops to
`LEMNOS_ORION_USER` (`lemnos-orion`), with the supplementary groups `LEMNOS_ORION_GROUPS` (`lemnos` for
lemnosd's socket and the slot's `.incoming` directory, and the Orion node's group, which the image names).
Its settings are in `/etc/lemnos/lemnos-orion.env`, which both `lemnos-orion.service` and
`S51lemnos-orion` read.

The bridge writes files only into the directories `LEMNOS_ORION_FILES` names (`slots=/data/slots/.incoming`):
files sent through Orion to a device that takes them (see [docs/daemon.md](../docs/daemon.md#lemnos-orion)).
The group `lemnos` gives it a slot device's `.incoming`, which the driver keeps mode 0770. Under systemd,
`ProtectSystem=strict` makes the filesystem read-only, so each directory goes into a drop-in as
`ReadWritePaths=<dir>`. The bridge writes nothing else, and its unit is hardened to `AF_UNIX` only, with no capability. Widen the address families only if the node is reached
over TCP.

## The security view (the image's part)

The security switch's view is `/etc/board/security`, and every reader watches the directory `/etc/board/`
and reads `security` in it (`docs/slots.md`, "The security switch"). The image makes that directory a
symlink to the tmpfs directory `/run/board/`:

    ln -s /run/board /etc/board

The daemon and the bridge never write `/etc/board` itself: they write `/run/board/security`, by a new
file renamed over the old one, so the directory watch sees each render. The view is rendered from the
board's record before the daemon and the comm core start, by the unit that
`lemnos-ctl tree BOARD --security-unit FILE` writes (`lemnos-board-security.service`, from the board
file's `[security]` table). It runs `lemnos-ctl security render`, which needs no daemon. Install the unit
as `/usr/lib/systemd/system/lemnos-board-security.service` and enable it; on a BusyBox init, run the same
command from the early init script, before `S50lemnosd`. On a board with slots the slot tool renders the
same view earlier still; that step is part of the slot image (not built here).

On an image without Lemnos, `/etc/board` is a real directory and Orion owns it: not this packaging's concern.

## Settings

`/etc/lemnos/lemnosd.env` is read by the daemon's unit and script (`EnvironmentFile=-`, so it may be
absent). It takes the `LEMNOSD_*` variables of [docs/daemon.md](../docs/daemon.md), including
`LEMNOSD_USER` and `LEMNOSD_GROUP`. A flag in the unit's `ExecStart` wins over its variable, so
`LEMNOSD_BOARD` does nothing while `ExecStart` names `--board`.

## Kernel module (optional)

`lemnos-module.conf` loads `lemnos` at boot. The module claims nothing by itself: a claim needs both the
`owned=` option and the device's `driver_override` (see [docs/kernel-module.md](../docs/kernel-module.md)).
`owned=` is set by the image in a `modprobe.d` file, not here, because it names the board's blocks.

## Changing a unit

Use a drop-in (`/etc/systemd/system/<unit>.d/*.conf`), not an edited copy. `systemd-analyze verify`
and `systemd-analyze security --offline=true` run on every file here (`scripts/check-packaging.sh`); the
gate fails above an exposure of 2.5.

## Not verified here

This PC installs nothing, and no board runs these units or scripts yet. The checks above read the files;
they do not start a daemon, except for the user-namespace test described in
[testing/board-runs/L3.md](../testing/board-runs/L3.md). Untested on board: the units, rules, scripts and
sysusers file end to end on a board image, the privilege path as root in the init namespace, and the
`MemoryDenyWriteExecute` setting with the driver packages loaded.
