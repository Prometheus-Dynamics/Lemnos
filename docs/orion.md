# lemnos-orion: lemnosd's devices in Orion

Status: implemented (`crates/lemnosd/src/orion`, binary `lemnos-orion`, feature `orion` of
`lemnosd`). Builds on [system-service.md](system-service.md) and
[board-definition.md](board-definition.md). This page is the contract for Atlas's Sensors panel and
for HeliOS. Changing a name or unit here is a breaking change.

`lemnos-orion` is a lemnosd client (name `orion`) and an Orion provider over local IPC (client name
`lemnos-orion`, provider `lemnos`). It publishes each board device as an Orion resource, mirrors its
readings and status, and runs its actions on it (`set`, `restore`, `release`, `read`, and the calibration actions below). Raw GPIO, PWM, I2C and SPI stay direct to lemnosd
and are not exposed here. Device operations such as calibration are not built yet.

Pinned against Orion `main` at `4fadba9` (`TypedConfigValue::F64` for readings). The actions use the
provider's `watch_action_requests`, which works with or without Orion's newer request/response API.

## Resources

| | |
|---|---|
| Resource type | `lemnos.device` |
| Resource id | `lemnos.<board>.<device>`, for example `lemnos.raze.imu` (`<board>` is the board id, `<device>` the board's device id) |
| Provider id | `lemnos`, on the node named by `ORION_NODE_ID` (default `node.local`) |

Availability and health, from the device status:

| lemnosd status | availability | health |
|---|---|---|
| `available` | `Available` | `Healthy` |
| `degraded` | `Available` | `Degraded` |
| `faulted` | `Unavailable` | `Failed` |
| `missing` | `Unavailable` | `Unknown` |

### Labels

Labels are `key=value` strings on the resource.

| Key | Value |
|---|---|
| `lemnos.class` | the device class (`imu`, `magnetometer`, `power-monitor`, `fan`, `temperature`, `lens`, ...) |
| `lemnos.model` | the device model from the driver |
| `lemnos.board` | the board id |
| `lemnos.driver` | the driver from the board definition (`bmi088`, `ina238`, ...); omitted when the board file is not readable |
| `lemnos.unit.<channel>` | the unit symbol of a channel (`m/s²`, `rad/s`, `T`, `V`, `A`, `W`, `°C`, `rpm`, ...); empty for a dimensionless one |
| `lemnos.control.<name>` | the control's range in its unit: `<min>..<max>`, with the unit symbol after a space when there is one (`0..1`, `0..1023`) |

## Status keys

Status entries are on the resource subject (`resource/lemnos.<board>.<device>`). Their TTL is 90 s.

| Key | Value | When |
|---|---|---|
| `status` | `available`, `degraded`, `faulted` or `missing` | on change, and on every heartbeat |
| `reason` | text: why the device is not available (`init: ...`, `read: ...`, or the board's own error); empty while available | on change, and on every heartbeat |
| `read_us` | unsigned integer: lemnosd's monotonic clock in microseconds at the last published reading | with a published reading, and on heartbeat |
| `<channel>` | `F64` in the channel's unit (`bus_voltage` in V, `acceleration.x` in m/s², ...) | when the value moves by its deadband, and on heartbeat |
| `control.<name>` | `F64` in the control's unit | when the value changes (any client's write or a restore), and on heartbeat |

Channel and control names come from the device, so they can contain dots (`acceleration.x`). A
channel with no value yet is not published.

## Readings: cadence and deadbands

- At most `LEMNOS_ORION_RATE_HZ` readings per device per second (default `2`). lemnosd is subscribed
  at that period, and the bridge also enforces it.
- A channel is published only when it has moved by at least its deadband since the last value
  published. A reading with no change publishes nothing.

| Quantity | Deadband (unit) |
|---|---|
| acceleration | 0.05 m/s² |
| angular rate | 0.01 rad/s |
| magnetic field | 1e-7 T |
| voltage, current | 0.005 V, 0.005 A |
| power | 0.05 W |
| temperature | 0.2 °C |
| rotational speed | 20 rpm |
| ratio | 0.005 |
| position, frequency | 1 |
| angle | 0.01 |
| pressure | 100 Pa |
| others (level, mode, colour) | exact |

- Status changes are published at once, not rate limited.
- Heartbeat: every 30 s, every status entry the bridge can publish is republished. That keeps the 90 s TTL
  from lapsing. The resources are republished on the same heartbeat.

## Actions

Actions target a resource: `ActionTarget::Resource(lemnos.<board>.<device>)`. The bridge is the
handler, so Orion routes an action here only while the bridge is connected.

| Name | Arguments | Output on success | Effect |
|---|---|---|---|
| `set` | `control` (string, required), `value` (number, required, finite) | `control` (string), `applied` (`F64`, the value lemnosd applied) | writes the control in its unit |
| `restore` | `control` (string, optional; all of the caller's writes when absent) | none | undoes the caller's writes |
| `release` | none | none | hands a fan back to the kernel's governor |
| `read` | none | `status` (string), `read_us` (`UInt`), one `F64` per channel with a value | the device's latest reading |
| `power.set` | `on` (bool, required) | `control` (`power.on`), `applied` (`F64`) | switches a power switch on or off (`gpio-power-switch`; a latched setting, see [system-service.md](system-service.md), *Power switches*) |
| `power.reset` | `off_ms` (number, optional, default 1000; 0 to 10000) | `control` (`power.reset`), `applied` (`F64`) | turns a power switch off for `off_ms` and back on (recovers a port that latched off) |
| `calibration.start` | `routine` (string, required: `accel-six`, `mag-rotate` or `gyro-hold`) | none | starts a forced routine (the IMU takes `accel-six` and `gyro-hold`, the magnetometer `mag-rotate`) |
| `calibration.stop` | none | none | ends the running routine; a finished candidate is kept |
| `calibration.apply` | none | none | makes the candidate the applied calibration (replaces it at once); lemnosd saves it |
| `calibration.discard` | none | none | drops the candidate |
| `calibration.reset` | none | none | returns the device to its factory calibration |
| `calibration.status` | none | `revision` (`UInt`), `running` (string: `none` or the routine), `progress` (`F64`, 0 to 1), `candidate` and `failed` (`Bool`), and for each of `accel`, `gyro`, `mag`: `samples` (`UInt`), `confidence`, `coverage`, `residual` (`F64`, 0 to 1), `active` (`Bool`) | the calibration's state, read now |

The IMU and magnetometer resources also carry the calibration confidence as status keys, polled about
every 10 s and published when they change: `calibration.revision`, `calibration.candidate`,
`calibration.accel.confidence`, `calibration.gyro.confidence`, `calibration.mag.confidence` (`F64`, 0 to 1)
and `calibration.<part>.active` (`Bool`). A device with no such part reads as zero.

Writer naming: each caller (`requested_by`) gets its own lemnosd connection named `orion:<requested_by>`,
created on its first action. lemnosd applies a device's write policy to that name, and it ends the
writes when that connection ends. A restore therefore only undoes the writes made through the same
caller, and it needs the control to be readable first: lemnosd keeps the value from before the first
write (a control that cannot be read has nothing to restore to, so the write stays).

After every action the bridge reads back each control of the device, so the status lane shows the
value that lemnosd now holds. lemnosd's restore and release do not name the control they changed.

### Refusals

A refused action is `Rejected` with the reason. A failure of the device is `Failed`.

| Outcome | Reason text | Cause |
|---|---|---|
| Rejected | `only resources take lemnos actions` | target is not a resource |
| Rejected | `no lemnos device <resource>` | the resource is not on this board |
| Rejected | `unsupported action `<name>` (one of set, restore, release, read, calibration.start, calibration.stop, calibration.apply, calibration.discard, calibration.reset, calibration.status)` | unknown name |
| Rejected | `` `routine` (accel-six, mag-rotate or gyro-hold) is required `` / `` unknown routine `<name>` `` | bad `calibration.start` arguments |
| Rejected | `` `control` (string) is required `` / `` `value` (number) is required `` / `` `value` must be finite `` | bad `set` arguments |
| Rejected | `not allowed by the device's write policy` | the caller is not in the device's `writers` |
| Rejected | `value out of range` | outside the control's range |
| Rejected | `no such control` | unknown control |
| Failed | `device error: <kind>` | the device failed (its error kind) |
| Failed | `lemnosd: <error>` | lemnosd could not be reached, or timed out |

A power switch's status is on its channels: `power.on` (1 on, 0 off, `F64`) and, when
the switch has a fault input, `power.fault` (1 asserted). They publish under the
channel names, as the other status keys do; a fault also makes the status
`degraded`.

## Connections and failure behaviour

- Without lemnosd: the bridge retries its connection every `retry` (1 s) and publishes nothing.
- Without Orion: the bridge retries the provider registration every second. It reads lemnosd meanwhile.
- After an Orion restart: the bridge reconnects, registers again, and republishes the resources and
  status. Within one heartbeat (30 s) the resources and status are back. Actions sent before that are
  refused (no handler), with the usual Orion rejection.
- After a lemnosd restart: its client reconnects and resubscribes, and the device list is read again.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `LEMNOSD_SOCKET` | `/run/lemnos/lemnosd.sock` | lemnosd's socket |
| `LEMNOSD_BOARD` | `/etc/lemnos/board.toml` | the board definition (for `lemnos.driver` labels only) |
| `LEMNOS_ORION_RATE_HZ` | `2` | readings per device per second, in (0, 50] |
| `LEMNOS_ORION_CHANNELS` | (unset) | `device=channel,channel;device=...`: subscribe to those channels only (`angular_rate.z`, `acceleration.*`, `*`); other devices are whole |
| `ORION_NODE_ID` | `node.local` | the node the provider runs on (must match the node) |
| `ORION_NODE_IPC_SOCKET` | `/run/orion/control.sock` (unit) | Orion's local IPC socket, as orion-node's unit sets it |
| `ORION_NODE_IPC_STREAM_SOCKET` | `/run/orion/control-stream.sock` (unit) | Orion's local IPC stream socket, as orion-node's unit sets it |

The unit is `packaging/systemd/lemnos-orion.service` (`After=lemnosd.service orion-node.service`,
`Wants=` both, `Restart=always`). It is optional: the Gaia layer is `packaging/gaia/lemnos-orion.toml`,
which an image imports on top of `lemnosd.toml` when it runs Orion.

## Not in this contract

- Raw GPIO, PWM, I2C and SPI: direct to lemnosd only.
- Device operations (calibrate, and similar): not yet.
- Look presets and LED controls (`looks.preset.apply`, `looks.show_inline`, the ring brightness): not on
  this bridge yet. They need an action target that is the provider rather than a resource, which the
  bridge does not route. The lemnosd operations exist (`lemnos-ctl looks preset`, `LedClient`).
- `release` is the fan hand-back; on a device that is not a fan, lemnosd's answer is passed on as the outcome.
