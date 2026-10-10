# Board definitions

Status: implemented (`lemnos-board`, the facade's `board` feature). Builds on
[compact-model.md](compact-model.md).

A board definition is a versioned TOML or JSON file that lists a board's devices: which
generic driver each one uses, where it is (an I2C bus and address, or a kernel class
device), and its settings. It is the only board-specific artifact. Drivers are generic and
written once against the compact device model (`lemnos-device`); a board is configuration
composed of them.

Every host reads the same file:

| Host | How it uses the definition |
|---|---|
| The runtime (`lemnos`, feature `board`) | `BoardSetup` reports each device as a configured descriptor and binds it through the generic device-model adapter. HeliOS uses this. |
| `lemnosd` | hosts the devices directly (see [system-service.md](system-service.md)). |
| A small Linux program | `DriverRegistry::build` returns each device as a `BoxedDevice`; put them in a `lemnos-lite` table. |
| Firmware | does not parse files; it constructs the same drivers in code (`lemnos-lite`). |

The JSON Schema is [schemas/lemnos-board.schema.json](schemas/lemnos-board.schema.json).
`BoardDefinition::validate` checks what a schema cannot: that each driver exists and
accepts the given bus, address, `match` and `config` keys.

## Format

```toml
format = "lemnos.board"   # required, always this value
schema_version = 1        # required; readers reject newer versions

[board]
id = "raze"               # required: lowercase letters, digits, inner '-'
name = "Raze"             # optional
revision = "2027"         # optional
generated_by = "atlas 1.2.0"  # optional, set by generators

[[devices]]
id = "imu"                # required, unique within the board
driver = "bmi088"         # required, see the driver table
label = "IMU"             # optional, human-readable
backend = "auto"          # auto (default) | userspace | kernel
bus = "i2c-1"             # bus devices: "i2c-<n>", "i2c:<selector>" or "spi-<bus>.<cs>"
address = 0x18            # 7-bit I2C address; the driver's default if left out
path = "/sys/class/hwmon/hwmon2"  # platform devices found by path
match = { name = "pwmfan" }       # platform devices found by attributes
poll_ms = 10              # the fastest hosts read it (the cap on a subscription)
idle_poll_ms = 1000       # how often, with no subscriber (0: never; IMU-class: never by default)
writers = ["helios"]      # clients allowed to write its controls (empty: any)
config = { gyro_address = 0x68, accel_range = "6g" }  # driver settings
```

`writers` and the top-level `raw_clients` match client names exactly, unless an entry ends in
`*`: `"orion:*"` admits every client whose name starts with `orion:`, and `"*"` admits any
client. A `*` anywhere else is a validation error. The Orion bridge (`lemnos-orion`) writes as
`orion:<requested_by>`, and Orion has already authorized those callers, so `writers = ["orion:*"]`
is the usual way to let Orion clients write a device. Entries without `*` are still exact.

Unknown fields are errors, so a typo does not silently fall back to a default.

### Finding an I2C bus without its number

I2C bus numbers follow probe order and can change between kernels and overlays. Instead of
`bus = "i2c-<n>"`, a definition can name the adapter by what it is:
`bus = "i2c:<key>=<value>[;<key>=<value>...]"`, where every given key must match exactly one
adapter in `/sys/bus/i2c/devices`:

| Key | Matches |
|---|---|
| `name` | the adapter's `name` attribute (`Synopsys DesignWare I2C adapter`) |
| `compatible` | one of the `compatible` strings of the adapter's device-tree node, or of its parent device's node (`i2c-gpio`, `snps,designware-i2c`) |
| `of` | that node's device-tree path (on the Raze with kernel 7.2, RP1's i2c1 is `/axi/pcie@1000120000/rp1/i2c@74000`) |
| `node` | the last component of that path (`i2c@74000`: a DesignWare controller at a given address) |

```toml
bus = "i2c:compatible=i2c-gpio"                          # the board's only i2c-gpio bus
bus = "i2c:compatible=snps,designware-i2c;node=i2c@74000"
```

Hosts resolve a selector when they build the device (and the runtime's board probe at each
refresh), so a renumbered adapter is followed; no match, or more than one, fails that
device with `not-found` and leaves the rest of the board running.

Device-tree paths and node names are kernel-specific: the RP1 PCIe node, for example, is
`pcie@1000120000` on kernel 7.2 and has been spelled differently by other kernels and
overlays. Generators should take an `of` or `node` value from the running device tree
(`/sys/firmware/devicetree/base`, or an adapter's resolved `of_node`) or from a manifest
recorded on that kernel, never from documentation. Generators should emit the most robust
selector they know (a unique `compatible`, else the `of` path) and fall
back to `i2c-<n>`.

### PIO I2C buses

When no hardware controller drives a device's pins, the bus can run on the RP1
PIO block:

```toml
[[devices]]
id = "imu"
driver = "bmi088"
bus = "pio-i2c:sda=8,scl=7"       # optional ,hz=100000 (default 400000)
address = 0x18
```

`sda` and `scl` are RP1 GPIO numbers (0..27), distinct. The bus has no kernel
adapter number; it is named by its pins and clock. Requirements, the pins
being free, and the limits are in `docs/system-service.md` (PIO I2C).

Board packages must drop the kernel `i2c-gpio` overlay for those pins (Atlas:
remove the `i2c-gpio` node for GPIO7/GPIO8 from the image's overlay set, and
give the `lemnos` user access to `/dev/pio0`). Until the overlay is gone the
device reports that the pins are in use.

### Raw access: lines, PWM channels and policy

Hosts that hand hardware to clients (`lemnosd`) read three more keys:

```toml
raw_clients = ["helios", "atlas-selftest"]   # top level: who may claim raw (empty: anyone); `*` as in `writers`

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
raw = ["atlas-selftest"]   # clients allowed brokered I2C to the IMU's addresses

[[lines]]                  # GPIO lines no device owns, by name, with a safe state
name = "aux-1"
chip = "pinctrl-rp1"
line = 21
safe = "low"               # input (default: high impedance), low or high

[[pwms]]                   # PWM channels no device owns, by name
name = "buzzer"
chip = 0                   # pwmchip0
channel = 1
```

A device owns its I2C address (plus any integer `*_address` setting), its SPI chip select
and, for `gpio-*` devices, its line; those are never claimed raw. See
[system-service.md](system-service.md#raw-bus-and-line-access).

### Backends

- `userspace`: the Lemnos driver over the bus device (`/dev/i2c-N`).
- `kernel`: the chip's mainline kernel driver, read through IIO or hwmon by the generic
  binding in `lemnos-drivers-linux`. The chip crate's `KernelBinding` says which kernel
  device names and attributes map to which channels, so the device reports exactly the
  same channels, units and names as the userspace driver. The kernel driver must be bound
  (device-tree overlay); otherwise building the device fails with `not found`.
- `auto`: `kernel` if the kernel driver is bound at that bus and address, otherwise
  `userspace`. When the kernel driver owns the chip, i2c-dev access to its address would
  fail with `EBUSY`, so `auto` never fights the kernel.

## Built-in drivers

| `driver` | Class | Placement | `config` | Channels / controls |
|---|---|---|---|---|
| `bmi088` | `imu` | I2C, default 0x18 | `gyro_address` (0x68), `accel_range` (`3g` `6g` `12g` `24g`), `accel_rate` (`12.5hz`..`1600hz`), `gyro_range` (`125dps`..`2000dps`), `gyro_rate` (`2000hz-532` `2000hz-230` `1000hz-116` `400hz-47` `200hz-23` `100hz-12` `200hz-64` `100hz-32`) | `acceleration.{x,y,z}` m/s², `angular_rate.{x,y,z}` rad/s |
| `bmm150` | `magnetometer` | I2C, default 0x10 | `preset` (`low-power` `regular` `enhanced` `high-accuracy`), `data_rate` (`2hz`..`30hz`) | `magnetic_field.{x,y,z}` T |
| `ina226`, `ina238` | `power-monitor` | I2C, default 0x40 | `shunt_micro_ohms` or `shunt_ohms`, `max_current_micro_amps` or `max_current_amps` (required) | `bus_voltage`, `shunt_voltage` V, `current` A, `power` W, (`ina238`) `die_temperature` °C |
| `ina260` | `power-monitor` | I2C, default 0x40 | none (integrated shunt) | as `ina226` |
| `vcm` | `lens` | I2C, default 0x0c | `chip` (`dw9714` `dw9807` `dw9817` `ak7375`, required) | control `position` |
| `hwmon-fan` | `fan` | `match.name` or `path` | `restore_mode`: for fan-controller chips, the automatic `pwm1_enable` mode that hands the fan back when a host stops (default 2). Not used for `pwm-fan` or other fans with a thermal cooling device: they get back the `pwm1_enable` read at bind and the cooling device re-applies the governor's level | `speed` rpm, `duty`, `pwm_mode`; controls `duty`, `pwm_mode` |
| `thermal-zone` | `temperature` | `match.type` or `path` | none | `temperature` °C |
| `ws2812` | `light` | `path` (default `/dev/leds0`, the RP1 `ws2812-pio` device) | `count` (required), `wire` (`rgb`, `rgbw`), `offset` (the physical LED that is logical 0), `direction` (`cw`, `ccw`), `brightness` (0..1, the driver's own byte; not the look scale), `look_brightness` (0..1, default `0.5`: the ring-wide scale of every look, `docs/looks.md`), `gpio` (informational), `spinner_base` (the comets' dim glow), `gravity_device`, `gravity_plane`, `gravity_led0_deg`, `default_down` (which way is down for a falling sparkle: `docs/looks.md`, *Gravity*), and the look defaults below | controls `brightness`, `color`; frames |
| `gpio-output` | `gpio` | `config.chip` + `config.line` | `chip` (`gpiochipN` or a label such as `pinctrl-rp1`), `line`, `active_low`, `initial` | `level` channel and control |
| `gpio-input` | `gpio` | `config.chip` + `config.line` | `chip`, `line`, `active_low` | `level` |

A light's look defaults (used by `lemnosd`'s LED intents; any intent can override them):
`fade_ms`, `easing` (`linear`, `ease-in`, `ease-out`, `ease-in-out`, `sine`,
`cubic-bezier(x1, y1, x2, y2)`), `status_effect` and `error_effect` (`solid`, `blink`,
`breathe`), `locate_effect` (`breathe`, `blink`, `chase`), `breathe_period_ms`,
`breathe_depth` (0..1; the breathe's low point is 1 minus this), `blink_period_ms`,
`blink_duty` (0..1), `spinner_period_ms`, `spinner_tail` (LEDs), and colours
(`0xRRGGBB` or `"#rrggbb"`): `ok`, `warn`, `error`, `busy`, `locate`, `idle`, `progress`
(the arc's fill), `progress_background` (the arc's track; default `101012`, a faint
neutral white), `updating` (the update's arc), `verifying`, `writing`, `staged`,
`booting`, `rebooting` (the reboot ember, 12% of this colour), `failed`, `confirmed` (the
ripple after a trial boot).

Named looks: the board's `[looks.<name>]` tables are read on top of the built-in looks
(`status.*`, `system.*`, `pv.*`), and a look file (`LEMNOSD_LOOKS_DIR`) overrides them
all; see [looks.md](looks.md) for the blocks, the keys and reloads.

```toml
[looks."pv.searching"]
layers = [{ block = "comet", color = "00ff20", period_ms = 1600, tail = 6, base = 0.06 }]
```

The system looks' own timing and shape (see `docs/system-service.md`, "System states"):

| Key | Default | Meaning |
|---|---|---|
| `verifying_period_ms` | `1200` | the verifying comet's turn |
| `verifying_tail` | `7` | its tail, in LEDs (fractions allowed, `0` to `64`) |
| `verifying_base` | `0.18` | its floor brightness, 0 to 1 (the dim glow under the comet) |
| `spinner_base` | `0.18` | the floor of the spinner, the locate chase and an orbit (0 to 1) |
| `booting_period_ms` | `1800` | the trial boot's twin comets' turn |
| `booting_tail` | `5` | their tail, LEDs |
| `booting_base` | `0.04` | their floor brightness |
| `staged_period_ms` | `2200` | the staged breathe's period |
| `staged_depth` | `0.45` | its depth (down to 55%) |
| `failed_period_ms` | `2400` | the failed or rolled-back pulse's period |
| `failed_depth` | `0.9` | its depth (down to 10%) |

Example (the Raze, `/etc/lemnos/board.toml`): `config = { count = 16, wire = "rgb",
offset = 5, direction = "cw", gpio = 13, fade_ms = 250, easing = "ease-in-out",
status_effect = "breathe" }`; the rest is the defaults above.

Hosts can register more drivers (`DriverRegistry::register`); a `DriverEntry` names the
config keys it accepts so validation stays strict.

## The Raze

[crates/lemnos-board/examples/raze.toml](../crates/lemnos-board/examples/raze.toml):

```toml
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"
name = "Raze"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c:compatible=i2c-gpio"   # i2c-4 on the 7.2.9 image
address = 0x18
poll_ms = 10
config = { gyro_address = 0x68, accel_range = "6g", accel_rate = "400hz", gyro_range = "2000dps", gyro_rate = "400hz-47" }

[[devices]]
id = "magnetometer"
driver = "bmm150"
bus = "i2c-1"
address = 0x10
poll_ms = 100

[[devices]]
id = "power"
driver = "ina238"
bus = "i2c-1"
address = 0x40
poll_ms = 100
config = { shunt_micro_ohms = 10000, max_current_micro_amps = 5000000 }

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = { name = "pwmfan" }       # the Linux pwm-fan driver's hwmon name
poll_ms = 1000

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = { type = "cpu-thermal" }
poll_ms = 1000

[[devices]]
id = "status-ring"
driver = "ws2812"
path = "/dev/leds0"
config = { count = 16, wire = "rgb", offset = 5, direction = "cw", gpio = 13, fade_ms = 250, easing = "ease-in-out", status_effect = "breathe" }

[[devices]]
id = "usb-a-power"
driver = "gpio-output"
config = { chip = "pinctrl-rp1", line = 20, initial = true }
```

The buses and addresses were verified by chip-id reads on the Raze (PhotonVision 2027
image, kernel 7.2.9): the BMI088 is on the board's i2c-gpio bus (i2c-4 there; accelerometer
0x18, id 0x1E; gyroscope 0x68, id 0x0F), the BMM150 on i2c-1 at 0x10 (id 0x32 once power
control 0x4B bit 0 is set) and the INA238 on i2c-1 at 0x40 (die id 0x2381). The shunt value
is still the schematic's to confirm. The ring is a 16-LED SK6812 on GPIO 13 with logical
LED 0 at physical LED 5; `wire = "rgb"` ran correctly on the Raze.

## Generating it

Generators (Atlas's device package) emit the file from their manifest:

- one `[[devices]]` entry per peripheral, `driver` from the part (`BMI088` → `bmi088`),
  `bus`/`address` from the wiring, settings into `config`;
- `generated_by` set to the generator and its version;
- JSON is accepted as well (`BoardDefinition::from_json_str`, or a `.json` path), so a
  generator can serialize without a TOML writer; validate the output against the JSON
  Schema in the generator's CI and with `BoardDefinition::validate` in Lemnos's.

## Using it

On Linux without the runtime:

```rust
use lemnos_board::{BoardDefinition, DriverRegistry, LinuxBuses};

let board = BoardDefinition::from_path("/etc/lemnos/board.toml")?;
let registry = DriverRegistry::builtin();
board.validate(&registry)?;
let mut buses = LinuxBuses::default();           // /dev/i2c-N, /sys
for spec in &board.devices {
    let mut device = registry.build(spec, &mut buses)?;   // a BoxedDevice
    device.init(&mut lemnos_linux::hal::StdDelay)?;
}
```

In the runtime: see "HeliOS adoption" below; the facade's `BoardSetup` wraps the same
registry.

## HeliOS adoption

HeliOS already passes sensor files through `HELIOS_SENSOR_CONFIG_PATHS` (and watches them),
but does not parse them yet. With this format, `sensors.toml` *is* a board definition.

1. **Dependency.** Add the `board` feature:

   ```toml
   lemnos = { workspace = true, default-features = false,
              features = ["builtin-drivers", "linux", "linux-hotplug", "tokio", "board"] }
   ```

2. **Config.** Ship `/etc/helios/sensors.toml` with the Raze's devices (the file above,
   with the board's real bus, addresses and shunt) and keep
   `HELIOS_SENSOR_CONFIG_PATHS=/etc/helios/sensors.toml`.

3. **Stack.** In `LemnosPeripheralStack::new`, load the file, add its probe to the Linux
   backend and its drivers to the runtime:

   ```rust
   use lemnos::board::{BoardDefinition, BoardSetup};

   let mut backend = LinuxBackend::default();
   let mut builder = Lemnos::builder();
   for path in &config.sensor_config_paths {
       if path.exists() {
           let setup = BoardSetup::new(BoardDefinition::from_path(path)?)?;
           backend = backend.with_probe(Arc::new(setup.probe()));
           builder = builder.with_board(&setup)?;
       }
   }
   let mut lemnos = builder.with_linux_backend_ref(&backend).build();
   lemnos.register_builtin_drivers()?;
   ```

   `with_board` adds the board's drivers to the bind policy, so every refresh binds the
   sensors; the existing `refresh_incremental_with_linux` and hotplug paths run the board
   probe because it lives in the backend. On a change to `sensors.toml` (HeliOS already
   watches it), rebuild the stack.

4. **Resources.** Board devices are `DeviceKind::Unspecified(I2c)` (sensors) or
   `Unspecified(Platform)` (fan, thermal zone). Map them by the `device.class` label
   (`imu`, `magnetometer`, `power-monitor`, `fan`, `temperature`, `lens`) instead of the
   kind; `board.device` is the id from the file and a stable resource name. The Linux
   backend also discovers every thermal zone (`linux.thermal.thermal_zoneN`, class
   `temperature`), bound by the built-in thermal-zone driver.

5. **Telemetry.** `refresh_state(id)` returns the channels as `f64` in SI units, keyed by
   channel name: `acceleration.x` (m/s²), `angular_rate.z` (rad/s), `magnetic_field.y`
   (T), `bus_voltage` (V), `current` (A), `power` (W), `temperature` (°C), `speed` (rpm),
   `duty` (fraction). `realized_config` carries `device.class`, `device.model` and the
   current control values. The existing `telemetry.*` label mapping in
   `resource_from_lemnos` works unchanged.

6. **Controls.** Controls are custom interactions `<control>.set` with the value in its
   unit: `request_custom_value(fan, "pwm_mode.set", 1u64)` then
   `request_custom_value(fan, "duty.set", 0.7)`. The built-in hwmon fan driver keeps
   `fan.read`/`fan.set_duty`/`fan.set_pwm`/`fan.set_mode`, so `lemnos_fan.rs` can be
   deleted in favour of it.

7. **Polling.** `poll_ms` is in the descriptor as `board.poll_ms`; use it for
   `driver_sample_interval_ms` per device instead of one global interval.

`crates/lemnos/tests/board_definition.rs` runs this flow end to end against mock I2C
hardware and a fake sysfs tree.
