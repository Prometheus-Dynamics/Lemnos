# Packaging lemnosd

`lemnosd` (the hardware service) and `lemnos-ctl` (its command-line client) ship as static
aarch64 musl binaries, built with the `service` profile (`opt-level = "z"`, fat LTO,
`panic = "abort"`, stripped):

```sh
rustup target add aarch64-unknown-linux-musl
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
  cargo build -p lemnosd --profile service --target aarch64-unknown-linux-musl
```

No C cross toolchain is needed: rust-lld links Rust's self-contained musl. Sizes (stripped,
static): `lemnosd` 758 KB, `lemnos-ctl` 681 KB.

## Gaia

[gaia/lemnosd.toml](gaia/lemnosd.toml) is a Gaia layer (Gaia >= 2.1) in the style of Orion's
`orion-node` layer. It builds both binaries in a small Docker image
([gaia/docker/aarch64-musl.Dockerfile](gaia/docker/aarch64-musl.Dockerfile)), installs them in
`/usr/bin`, and stages:

| File | Installed as |
|---|---|
| [systemd/lemnosd.service](systemd/lemnosd.service) | `lemnosd.service` |
| [systemd/lemnosd.env.example](systemd/lemnosd.env.example) | `/etc/default/lemnosd.env` |
| [systemd/lemnos.sysusers](systemd/lemnos.sysusers) | `/usr/lib/sysusers.d/lemnos.conf` |
| [systemd/lemnosd.preset](systemd/lemnosd.preset) | `/usr/lib/systemd/system-preset/80-lemnosd.preset` |

A board's device package imports the layer from a pinned Lemnos checkout and stages its
board definition as `/etc/lemnos/board.toml` (see the header of `gaia/lemnosd.toml`).

## What the image provides

- systemd (the unit is `Type=notify` with a watchdog) and `systemd-sysusers`, or a `lemnos`
  user created at build time on read-only root filesystems.
- Groups that own the devices: `i2c` (`/dev/i2c-*`), `gpio` (`/dev/gpiochip*`), `video` or
  whichever group owns the LED device (`/dev/leds*`). Adjust `SupplementaryGroups=` with a
  drop-in.
- Write access for the `lemnos` user to the fan's hwmon attributes (`pwm1`, `pwm1_enable`)
  and, for a `pwm-fan` fan, its thermal cooling device's `cur_state` (the hand-back
  re-applies the governor's level there): udev rules such as
  `SUBSYSTEM=="hwmon", ATTR{name}=="pwmfan", RUN+="/bin/chgrp lemnos /sys%p/pwm1 /sys%p/pwm1_enable", RUN+="/bin/chmod g+w /sys%p/pwm1 /sys%p/pwm1_enable"` and
  `SUBSYSTEM=="thermal", KERNEL=="cooling_device*", ATTR{type}=="pwm-fan", RUN+="/bin/chgrp lemnos /sys%p/cur_state", RUN+="/bin/chmod g+w /sys%p/cur_state"`.
  (The hwmon `name` of the Linux `pwm-fan` driver is `pwmfan`.) `ExecStopPost` runs as
  root and needs neither.
- Clients (HeliOS, PhotonVision, scripts) join the `lemnos` group to reach
  `/run/lemnos/lemnosd.sock`.

## Behaviour

- `READY=1` once the board's devices are built (devices that fail stay `missing` and are
  retried) and the socket listens.
- `WatchdogSec=10s`: the loop pings every 5 s while it keeps completing passes.
- On stop the service hands its fans back to the kernel and turns the LEDs off (or shows
  the rebooting look when the system is restarting); `ExecStopPost=+lemnos-ctl fan restore
  --all` does the same after a crash, `SIGKILL` or a watchdog abort, from the plans the
  service recorded in `/run/lemnos/fan-restore`. A `pwm-fan` fan (or any fan with a linked
  thermal cooling device) gets back the `pwm1_enable` read when the service bound it, and
  its cooling device re-applies the thermal governor's level (`pwm-fan` has no automatic
  `pwm1_enable` mode; writing 2 only keeps its regulator on). Fan-controller chips with an
  automatic mode get `pwm1_enable = restore_mode` (default 2).
- It shows the device package's updates on the status light without changes to the
  updater, from the status file `LEMNOSD_UPDATE_STATUS` names. The path is set only there:
  the environment file this layer stages (`/etc/default/lemnosd.env`, Gaia item
  `lemnosd-env`) sets `/run/pd-device/update.json`; a device package whose updater writes
  elsewhere (`/run/board/update.json`) redeclares `lemnosd-env`.
