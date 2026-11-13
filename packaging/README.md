# Packaging

How lemnosd runs on a board: a systemd unit, the user it runs as, the device-node rules it needs, and
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

The OS supplies what is not here: the `lemnosd` and `lemnos-orion` binaries at `/usr/bin`, and the
board file at `/etc/lemnos/board.toml` (see [docs/boards.md](../docs/boards.md)). The units refuse to
start without the board file (`ConditionPathExists=`).

## The daemon

`lemnosd.service` runs `lemnosd --board /etc/lemnos/board.toml` as user and group `lemnos`, with
`Type=notify`, a 30 s watchdog, and `Restart=always` (1 s, growing to 30 s; no start limit). It is not
root:

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
