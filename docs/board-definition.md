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
bus = "i2c-1"             # bus devices: "i2c-<n>" or "spi-<bus>.<cs>"
address = 0x18            # 7-bit I2C address; the driver's default if left out
path = "/sys/class/hwmon/hwmon2"  # platform devices found by path
match = { name = "pwm-fan" }      # platform devices found by attributes
poll_ms = 10              # how often hosts read it
writers = ["helios"]      # clients allowed to write its controls (empty: any)
config = { gyro_address = 0x68, accel_range = "6g" }  # driver settings
```

Unknown fields are errors, so a typo does not silently fall back to a default.

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
| `hwmon-fan` | `fan` | `match.name` or `path` | none | `speed` rpm, `duty`, `pwm_mode`; controls `duty`, `pwm_mode` |
| `thermal-zone` | `temperature` | `match.type` or `path` | none | `temperature` °C |

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
bus = "i2c-1"
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
match = { name = "pwm-fan" }
poll_ms = 1000

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = { type = "cpu-thermal" }
poll_ms = 1000
```

The bus number, addresses and shunt value are the board's; check them against the
schematic before deploying (the example's are placeholders where the Raze's are unknown to
this repository).

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
