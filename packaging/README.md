# Packaging

How lemnosd runs on a board: a systemd unit, or a BusyBox init script, the user it runs as, the device-node rules it needs, and
the settings file. This directory ships the files; an OS's image build installs them where the table
says. Nothing here names a board or an OS. `scripts/check-packaging.sh` checks every file, and the gate
runs it. It installs nothing, enables nothing, and starts nothing.

| File | Installed as | What it is | Who installs it |
|---|---|---|---|
| `systemd/lemnosd.service` | `/usr/lib/systemd/system/lemnosd.service` | The daemon. | Always |
| `systemd/lemnos-orion.service` | `/usr/lib/systemd/system/lemnos-orion.service` | The Orion bridge (`lemnos-orion`). | Only an image that ships `lemnos-orion` |
| `systemd/lemnos.conf` | `/usr/lib/sysusers.d/lemnos.conf` | The `lemnos` user and group. | Always |
| `systemd/lemnosd.env.example` | `/etc/lemnos/lemnosd.env` | Optional settings both units read. | Always, edited as needed |
| `systemd/lemnos-module.conf` | `/usr/lib/modules-load.d/lemnos.conf` | Loads `lemnos.ko` at boot. **Optional.** | Only an image that ships `lemnos.ko` |
| `udev/70-lemnos.rules` | `/usr/lib/udev/rules.d/70-lemnos.rules` | Gives group `lemnos` the device nodes lemnosd opens. | Always |
| `init/S50lemnosd` | `/etc/init.d/S50lemnosd` (mode 0755) | BusyBox init script for the daemon, in place of the unit. | Only an image without systemd |
| `init/mdev.conf.fragment` | appended to `/etc/mdev.conf` | Device-node rules for mdev, the BusyBox counterpart of the udev rules. | Only an image without systemd |

The OS supplies what is not here: the `lemnosd` and `lemnos-orion` binaries at `/usr/bin`, and the
board file at `/etc/lemnos/board.toml` (see [docs/boards.md](../docs/boards.md)). The units refuse to
start without the board file (`ConditionPathExists=`).

## The daemon

`lemnosd.service` runs `lemnosd --board /etc/lemnos/board.toml` as user and group `lemnos`, with
`Type=notify`, `Restart=always` with a constant `RestartSec=1s` and no start limit, and no `WatchdogSec=`:
the hardware watchdog is its escalation (see "The hardware watchdog"). It is not root:

- **Devices** come through group `lemnos`, from the udev rules. The daemon opens the nodes its drivers
  name, one by one; the rules list each kind by subsystem. Vendor nodes that a board's drivers open by
  name are added by the image, one line each (the rules file says how).
- **One capability**: `CAP_SYS_RAWIO`, ambient and bounding. The kernel allows a register mapping of
  `/dev/lemnos` only with it (`kernel/lemnos_core.c`, `lem_map_region`). A board with no mapped
  register block does not use it; the OS may remove it in a drop-in, and the daemon then fails only
  the drivers that map registers.
- **The socket** is `/run/lemnos/lemnosd.sock`, mode 0660, group `lemnos`: a program that talks to the
  daemon must be in group `lemnos`. `RuntimeDirectory=lemnos` makes `/run/lemnos` with mode 0750.
- **Not root, so some sysfs writes fail.** The PWM driver writes `/sys/class/pwm` (export and
  attributes), and the USB gadget driver writes configfs under `/sys/kernel/config`; both need root (or
  an image's own rules). A board that uses them sets that in a drop-in: `User=root` with the
  capabilities those writes need. The unit does not do this by default.
- **Hotplug** reads the kernel's uevent socket (`AF_NETLINK`). No capability is needed for that.
- **No state.** The daemon writes nothing to disk but its socket. It has no state directory, and no `tmpfiles.d` file
  is needed.

The hardening (`ProtectSystem=strict`, `NoNewPrivileges`, `SystemCallFilter=@system-service`,
`RestrictAddressFamilies=AF_UNIX AF_NETLINK`) is what the daemon's code needs and no more. The unit does
not set `PrivateDevices=` (the nodes are its job) or `PrivateNetwork=` (the uevent socket must stay in the
host network namespace).

## The Orion bridge

`lemnos-orion.service` runs `/usr/bin/lemnos-orion` as the same user. It connects to lemnosd's socket and
to the Orion node's socket, and writes no files. The node's socket belongs to Orion's group `orion`; an
image that ships Orion adds `SupplementaryGroups=orion` in a drop-in. The unit is hardened to
`AF_UNIX` only, with no capabilities and no devices. Widen the address families only if a node is reached
over TCP.

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
in the mdev fragment) so the daemon can open it as its own user. Nothing else should.

## Without systemd (BusyBox init)

A small board (64 to 128 MB) runs BusyBox init instead. The same daemon, the same user, socket, device
nodes and watchdog escalation, with `packaging/init/` in place of the units. The OS installs the files
above and does these four things, which the units would otherwise do.

**The user and group** are made by the image builder, not by sysusers. Add these lines (uid and gid 999,
or any free system id, the same in both files):

```text
/etc/group:   lemnos:x:999:
/etc/passwd:  lemnos:x:999:999:Lemnos daemon:/nonexistent:/bin/false
/etc/shadow:  lemnos:!:0:0:99999:7:::
```

Nothing keys on the id, as with the unit. The OS adds program users to group `lemnos` as usual.

**The device nodes** are given to group `lemnos` by mdev, not udev. Append `init/mdev.conf.fragment` to
`/etc/mdev.conf`. Each line is the same node as a udev rule: `^lemnos$` is `/dev/lemnos`, `^dma_heap/.*$`
the dma heaps, `^i2c-[0-9]+$` the I2C buses, `^spidev...$` SPI, `^gpiochip[0-9]+$` GPIO, the `ttyS`,
`ttyAMA`, `ttyUSB` and `ttyACM` serial ports, `^video[0-9]+$` and `^media[0-9]+$` for cameras,
`^watchdog[0-9]*$` for the watchdog, and `^bus/usb/.../...$` for usbfs. mdev runs at boot (`mdev -s`) and
on hotplug only if the image sets `/proc/sys/kernel/hotplug` to `/sbin/mdev`.

**The one capability** is granted to the binary, not to a process: `setcap cap_sys_rawio=ep
/usr/bin/lemnosd`. The init script starts the daemon as `lemnos` through `start-stop-daemon`, so no
process runs as root after boot, and no code in the script drops privileges. File capabilities work
for a non-root process on exec, so the daemon gets `CAP_SYS_RAWIO` where it is needed: the register
mapping. The rootfs must keep extended attributes (ext4, f2fs and squashfs with xattrs do). Where it
does not, the daemon runs without the capability and the drivers that map registers report
`PermissionDenied`; nothing else changes. The other choice, running the daemon as root and dropping
privileges in the script, was rejected: it adds a root process and privilege-drop code that the file
capability does without.

**The runtime directory and log.** The script makes `/run/lemnos` (mode 0750, owner `lemnos`) and
`/var/log/lemnosd.log` at start. Nothing needs a tmpfiles file.

The script (`/etc/init.d/S50lemnosd`) runs with `start`, `status`, `restart` and `stop`. It needs
`start-stop-daemon` (BusyBox with `CONFIG_START_STOP_DAEMON`). A daemon that exits is started again
after 1 s, a constant delay, and never given up on. `stop` is for shutdown: it sends TERM, waits up to
10 s, then KILLs. On a live board it must not be used, since the hardware watchdog would then reboot
the board. The script does not feed the watchdog. It does not depend on systemd-notify: lemnosd sends
nothing when `NOTIFY_SOCKET` and `WATCHDOG_USEC` are absent (`service/lemnosd/src/notify.rs`).

**What is lost without systemd** is the hardening, and the unit's bookkeeping:

- Each hardening option of the unit: `ProtectSystem=strict` (read-only filesystem view), `ProtectHome`,
  `PrivateTmp`, `ProtectKernelTunables`, `ProtectKernelModules`, `ProtectKernelLogs`,
  `ProtectControlGroups`, `ProtectHostname`, `ProtectClock`, `ProtectProc=invisible`, `LockPersonality`,
  `MemoryDenyWriteExecute`, `RestrictRealtime`, `RestrictSUIDSGID`, `RestrictNamespaces`,
  `SystemCallArchitectures`, `RestrictAddressFamilies` (the daemon is unconfined in its address
  families), `SystemCallFilter`, `SystemCallErrorNumber`, and `NoNewPrivileges`. Only `CAP_SYS_RAWIO` is
  granted, but the capability bounding set is not narrowed to it as the unit does.
- `sysusers` (the user is made by the image's passwd lines), `tmpfiles` (none needed), and
  `RuntimeDirectory=` (the script makes the directory and removes the socket at stop).
- Logging to the journal: the daemon's error output goes to `/var/log/lemnosd.log`.
- `ConditionPathExists=` for the board file: `start` checks it instead.

Nothing else changes: the watchdog escalation, the restart and the device groups are the same on both.
The sanity checks in `scripts/check-packaging.sh` run `sh -n` on the script, `busybox sh -n` when
BusyBox is installed, and `shellcheck` when it is installed. None of them runs the script. The init
script and the mdev fragment are untested on board.

## Settings

`/etc/lemnos/lemnosd.env` is read by both units (`EnvironmentFile=-`, so it may be absent). It takes the
`LEMNOSD_*` variables of [docs/daemon.md](../docs/daemon.md) and `ORION_NODE_ID`. A flag in the unit's
`ExecStart` wins over its variable, so `LEMNOSD_BOARD` does nothing while `ExecStart` names `--board`.

## Kernel module (optional)

`lemnos-module.conf` loads `lemnos` at boot. The module claims nothing by itself: a claim needs both the
`owned=` option and the device's `driver_override` (see [docs/kernel-module.md](../docs/kernel-module.md)).
`owned=` is set by the image in a `modprobe.d` file, not here, because it names the board's blocks.

## Changing a unit

Use a drop-in (`/etc/systemd/system/<unit>.d/*.conf`), not an edited copy. `systemd-analyze verify`
and `systemd-analyze security --offline=true` run on every file here (`scripts/check-packaging.sh`); the
gate fails above an exposure of 2.5.

## Not verified here

This PC installs nothing, and no board runs these units yet. The checks above read the files; they do
not start a daemon. Untested on board: the units, rules and sysusers file, end to end on a board image,
including `NoNewPrivileges` with the driver packages and `MemoryDenyWriteExecute` with them loaded.
