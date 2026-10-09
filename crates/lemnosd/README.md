# lemnosd

The board's hardware service. One process owns the board's devices, built from a board
definition (`/etc/lemnos/board.toml`, see `docs/board-definition.md`) with Lemnos's generic
drivers, and serves clients over `/run/lemnos/lemnosd.sock`:

- device lists in the compact model (class, channels with units, controls with ranges,
  status), readings and subscriptions;
- control writes under each device's write policy (`writers` in the board definition);
- LED intents arbitrated between clients (`locate` > system states > service alerts >
  status > application colours, frames, LEDs and gauges), rendered with eased fades and
  effects only while something moves;
- events: status, control and LED-owner changes.

It shows the device package's updates on the status light from
the status file `LEMNOSD_UPDATE_STATUS` names (no updater changes), hands fans back to the kernel when it
stops, and runs as a systemd `Type=notify` service with a watchdog. `lemnos-ctl` is its
command-line client:

```sh
lemnos-ctl list
lemnos-ctl read imu
lemnos-ctl watch imu --period 50
lemnos-ctl set fan duty 0.8
lemnos-ctl led status warn --effect breathe
lemnos-ctl led progress 0.4 --color 00ff40
lemnos-ctl led pixel 0 ff0000 8 0000ff
lemnos-ctl led locate --seconds 10
lemnos-ctl led frame ff0000,00ff00 --test   # test layer, over every status
lemnos-ctl led off
lemnos-ctl fan release fan                 # back to the kernel governor until the next write
lemnos-ctl fan restore --all
```

Libraries talk to it with `lemnos-ipc` (`DeviceClient`, `LedClient`). Packaging (systemd unit,
Gaia layer) is in `packaging/`; the design is `docs/system-service.md`.
