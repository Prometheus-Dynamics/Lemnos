# Compact device model and lite runtime

Status: phases 1 and 2 implemented (`lemnos-device`, `lemnos-lite`); the decisions below are
confirmed. Builds on [foundation.md](foundation.md).

## Goal

Lemnos should fit anywhere it is useful: a sensor on a small microcontroller, a Linux SoC
with a few megabytes of flash, and the CM5 under HeliOS. There is no fixed size budget.
Smaller is better, because the smaller Lemnos is, the more hardware it can run on.
`scripts/check-sizes.sh` tracks every image, so growth is always a deliberate choice.

Two things should hold on every target:

1. **One driver per device.** A BMI088 driver written once serves firmware, a small Linux
   program, and HeliOS.
2. **Pay for what you use.** Discovery, hotplug, driver matching, event history and
   string-keyed metadata cost nothing unless a target asks for them.

## Where the bytes go today

Measured on x86_64, stripped, `opt-level = "z"`, LTO, `panic = "abort"`. "Minimal std"
means nightly `-Z build-std` with `-Cpanic=immediate-abort -Cforce-unwind-tables=no`.

| Build | Stable | Minimal std |
|---|---|---|
| Empty `main` | 282 KB | 14 KB |
| BMI088 + BMM150 + INA238 on `/dev/i2c-N` via `lemnos_linux::hal`, no runtime | 312 KB | **34 KB** |
| Facade + Linux backend (no USB/UART), one refresh | 575 KB | **234 KB** |

On a microcontroller, all four device drivers (VCM, INA2xx, BMM150, BMI088) take 5.3 KB of
flash on a Cortex-M4F and 5.6 KB on an FPU-less RISC-V core. They use no RAM and no heap.

So the drivers are already small. The cost is the runtime: about 200 KB separates "three
sensors on Linux" from "three sensors through Lemnos". Most of it comes from three places:

- **The dynamic descriptor model.** Every device is a `DeviceDescriptor` with `String`
  names, `BTreeMap<String, String>` labels, a `BTreeMap<String, Value>` of properties,
  capability and link vectors, and match hints. Telemetry and configuration are also
  `String`-keyed `Value` maps. The generic map and string code for these is about 40 KB.
- **The refresh pipeline.** Discovery, inventory diffing, driver matching and ranking,
  rebinding, and an event log that clones whole descriptors into `Added`/`Changed`
  events. `Runtime::finish_refresh` alone is about 28 KB.
- **Linux discovery.** Six sysfs probes at about 5 KB each, mostly building descriptors.

HeliOS needs all of that. A sensor node needs none of it.

There is also a modeling gap: `DeviceKind` only says how a device is connected (an I2C
device, a PWM channel). The BMI088 and the hwmon fan are both `Unspecified(...)` and
expose behavior through string-named custom interactions, so nothing generic can tell
that one is an IMU and the other a fan.

## Proposal

Four layers. Each is useful on its own, and each only depends on the ones below it.

```
 L3  lemnos (runtime)      discovery, hotplug, registry, events, rich descriptors     std, alloc
       └─ generic adapter: any L1 device becomes a runtime driver
 L2  lemnos-lite           static device table, status, polling                        no_std, no alloc
 L1  lemnos-device         device classes, channels, units, Sensor/Control traits      no_std, no alloc
 L0  lemnos-hal + drivers  embedded-hal buses, register maps, chip drivers (today)     no_std, no alloc
```

### L1: `lemnos-device`, the compact model

A new `no_std` crate with no allocation and no `String`s. It describes what a device *is*
and what it measures or controls, using static tables:

```rust
#[non_exhaustive]
pub enum DeviceClass { Imu, Accelerometer, Gyroscope, Magnetometer, PowerMonitor,
                       Temperature, Fan, Lens, /* ... */ }

#[non_exhaustive]
pub enum Quantity { Acceleration, AngularRate, MagneticField, Voltage, Current, Power,
                    Temperature, Speed, Ratio, Position, /* ... */ }

/// One measured or controlled value: integer counts times 10^exponent, in the
/// quantity's unit (for example Acceleration in g with exponent -3 is milli-g).
pub struct Channel {
    pub quantity: Quantity,
    pub axis: Axis,          // None, X, Y, Z
    pub exponent: i8,
}

pub struct DeviceInfo {
    pub class: DeviceClass,
    pub model: &'static str, // "BMI088"
    pub channels: &'static [Channel],
    pub controls: &'static [Control],
}

pub trait Sensor {
    type Error: HalError;
    fn info(&self) -> &'static DeviceInfo;
    /// Fills one `i32` per channel, in `info().channels` order.
    fn read(&mut self, out: &mut [i32]) -> Result<(), Self::Error>;
}

pub trait Control {
    type Error: HalError;
    fn set(&mut self, control: usize, value: i32) -> Result<(), Self::Error>;
}
```

Async versions mirror these, as the drivers already do.

Values are fixed-point integers, matching the drivers' `read_fixed`. Floats stay an
optional conversion (`Channel::to_f32`) behind each driver's `float` feature. `info()`
returns a `'static` table, so describing a device costs a few dozen bytes of read-only data
and no code.

Every device driver implements L1 next to its own API: `Bmi088` becomes a `Sensor` with six
channels, `Ina` with four or five, `Bmm150` with three, and `Vcm` a `Control`. The hwmon
fan becomes a `Sensor` plus `Control` in `lemnos-linux`.

### L2: `lemnos-lite`, a static device table

For firmware and small Linux images that know their hardware at build time:

```rust
let mut imu = Bmi088::new(i2c_imu);
let mut power = Ina::new(i2c_power, 0x40, Model::Ina238, cfg)?;
let mut devices = lemnos_lite::Devices::new([
    lemnos_lite::sensor("imu", &mut imu),
    lemnos_lite::sensor("power", &mut power),
]);
devices.init_all(&mut delay);
devices.read("power", &mut buf)?;                // or by index
let status: DeviceStatus = devices.status(1);    // same DeviceStatus as L3
```

What it does:
- It holds `&mut dyn` devices in a fixed array, so code size stays flat as devices are
  added. Names are `&'static str`.
- It tracks per-device status and the last error kind.
- It offers optional polling helpers.

What it leaves out: discovery, driver matching, events and allocation. It runs unchanged
on a microcontroller (buses from the firmware's HAL) and on Linux (buses from
`lemnos_linux::hal`). The aim is to stay within a few kilobytes of the 34 KB "no runtime"
floor above.

### L3: the runtime, consuming L1

A generic adapter turns any L1 device into a runtime driver:
- `DeviceInfo` becomes a `DeviceDescriptor`, and channels become telemetry keys such as
  `acceleration.x`.
- `read` becomes `state()`, and controls become interactions.
- The device is bound through `lemnos_bus::hal` sessions, as
  `tests/nostd_driver_in_runtime.rs` already shows for the BMI088.

This replaces the planned per-sensor runtime adapters. Each new driver crate reaches HeliOS
without extra code, and HeliOS can bind sensors from `sensors.toml` through configured
devices.

The rich model stays for HeliOS, but stops being the foundation. Size work there continues
separately:
- events hold `Arc<DeviceDescriptor>` instead of clones
- event retention and diagnostics become optional features
- Linux probes share descriptor-building code

### Optional later: a bridge

A compact wire encoding of `DeviceInfo`, readings and control writes would let a
microcontroller's L2 devices appear in a Linux L3 runtime over UART or USB. That would
make a sensor board a remote Lemnos device. It isn't needed for the first phases, but the
L1 types should stay plain data so they can be encoded later.

## What was built

### L1: `lemnos-device`

`#![no_std]`, no allocation, no `String`s. Everything a device says about itself is a
`'static` table:

- `DeviceClass` (`Imu`, `Accelerometer`, `Gyroscope`, `Magnetometer`, `PowerMonitor`,
  `Temperature`, `Fan`, `Lens`, `Light`, `Gpio`, `Orientation`, `Other`).
- `Quantity`, each with one canonical `Unit`: acceleration m/s², angular rate rad/s,
  magnetic field T, voltage V, current A, power W, temperature °C, rotational speed rpm,
  ratio, position, level, mode, colour, angle rad, pressure Pa, frequency Hz. A reading
  means the same thing whichever driver or backend produced it.
- `Channel { name, quantity, axis, exponent }`: the value is `raw × 10^exponent` in the
  unit. `name` is the stable telemetry key (`acceleration.x`, `bus_voltage`): the
  proposal's `{quantity, axis}` alone could not tell an INA2xx's bus and shunt voltages
  apart. `NO_VALUE` (`i32::MIN`) marks a channel without a reading.
- `ControlInfo { name, quantity, exponent, min, max }`.
- `DeviceInfo { class, model, channels, controls }`.
- `DeviceStatus` moved here from `lemnos-core` (which re-exports it), so L2 and L3 report
  the same status.

Traits: `Device` (`info`, `init(&mut dyn DelayNs)`), `Sensor: Device` (`read(&mut [i32])`)
and `Control: Device` (`set(index, value) -> applied`, `get(index)`), with errors wrapped in
`DeviceError<E>` (`Driver(E)`, `Unsupported`, `OutOfRange`, `BufferTooSmall`).
`asynch::{Device, Sensor, Control}` mirror them. `erased` has the object-safe forms
(`DynSensor`, `DynControl`, `DynSensorControl`, errors reduced to `ErrorKind`), the borrowed
`DeviceRef` and, with `alloc`, the owned `BoxedDevice`. `kernel::KernelBinding` describes,
as data, how a chip's channels map onto its mainline IIO or hwmon driver, so a generic
binding can serve the same `DeviceInfo` without code per chip (phase 3).

The drivers implement it next to their own APIs:

| Driver | Class | Channels | Controls | Kernel binding |
|---|---|---|---|---|
| `Bmi088` | `Imu` | `acceleration.{x,y,z}` (mm/s²), `angular_rate.{x,y,z}` (µrad/s) | | `bmi088-accel` + `bmg160` (IIO) |
| `Bmm150` | `Magnetometer` | `magnetic_field.{x,y,z}` (nT) | | `bmc150_magn` (IIO) |
| `Ina` (INA226/260) | `PowerMonitor` | `bus_voltage` (µV), `shunt_voltage` (nV), `current` (µA), `power` (µW) | | `ina2xx` (hwmon) |
| `Ina` (INA238) | `PowerMonitor` | the above plus `die_temperature` (m°C) | | `ina238` (hwmon) |
| `Vcm` | `Lens` | | `position` (steps, 0..2^bits-1) | |

`Device::init` applies the configuration set with `with_config` (BMI088, BMM150), the
calibration given to `new` (INA2xx), or powers the lens up (VCM). Conversions to the
canonical units are one 32×32→64-bit multiply and a shift per value, with no 64-bit
division: that is a library call on 32-bit MCUs.

### L2: `lemnos-lite`

`Devices<'a, N>` is a fixed array of entries built with `sensor(name, &mut dev)`,
`control(...)` or `device(...)` (both), each holding a `DeviceRef`, a `DeviceStatus`, the
last `ErrorKind` and an optional polling period (`.every(ms)`). It reads, sets and gets by
index or name, and `poll(now_ms, delay, buf, on_reading)` reads due sensors and
re-initializes devices that are not up on their schedule. Status starts `missing`, becomes
`available` after a successful `init`, and follows `DeviceStatus::after_error` on failures
(not found or NACK: `missing`; transient: `degraded`; otherwise `faulted`).

`crates/lemnos-lite/examples/linux_sensors.rs` reads the Raze's three I2C sensors on Linux;
the firmware size image `lite` is the microcontroller example.

### Measured

Firmware, flash bytes (`.text + .rodata + .data`), all four drivers, integer readings:

| Image | Cortex-M4F | RISC-V (no FPU) |
|---|---|---|
| each driver's own API (`init`, `read_fixed`) | 5 294 | 5 562 |
| L1 traits, static dispatch (`device-static`) | 6 322 | 7 084 |
| L1 as `dyn` devices (`device`) | 7 644 | 8 340 |
| L2 table with status and polling (`lite`) | 8 196 | 9 048 |

So the device model costs about 1 KB over the drivers alone (channel tables and names in
read-only data, unit conversion into the caller's buffer, buffer and initialization
checks), `dyn` dispatch another 1.3 KB for four drivers (each driver's operations become
separate functions instead of being inlined into one loop), and the L2 table 0.5-0.7 KB.
That is more than the "few hundred bytes" the proposal guessed for L1; it is still under
9 KB of flash, with no RAM beyond the table and the caller's buffer and no software float.

Linux, x86_64, stripped, `opt-level = "z"`, stable toolchain: the three I2C sensors through
their own APIs (`sensors`) take 317.5 KB, through a `lemnos-lite` table (`sensors-lite`)
314.8 KB; the table is smaller because it prints plain value slices instead of each
driver's `Debug` structs. Both are within 35 KB of an empty `main` (282 KB); the facade with
the Linux backend (`linux-lite`) is 564 KB.

## Phases

1. **`lemnos-device`.** Types and traits, implementations for the four device drivers, and
   size-harness images that use them through `dyn Sensor`. Measure what the L1 layer adds
   over the drivers alone; it should be a few hundred bytes.
2. **`lemnos-lite`.** The static table, with firmware and Linux images in
   `testing/size/`. Measure against the 34 KB floor.
3. **The generic L3 adapter.** The runtime binds any L1 device. HeliOS reads sensors
   through it and `sensors.toml` maps onto configured devices.
4. **Rich-model slimming.** `Arc` in events, optional retention and diagnostics, shared
   probe code. Each change is measured.
5. **Bridge (optional).** The wire format plus a Linux-side probe.

None of these phases breaks HeliOS: L3's public API stays as it is, and L1/L2 are new
crates.

## Decisions

Confirmed in phases 1 and 2:

- **Fixed-point values with a decimal exponent in L1.** Yes: `i32` counts times
  `10^exponent` in one canonical unit per quantity, with `NO_VALUE` for a missing reading.
  Floats are an opt-in conversion (`Channel::to_f32`, `ControlInfo::from_f32`, feature
  `float`); FPU-less images link no float code.
- **`&'static str` names and static tables in L1/L2.** Yes: channel and control names are
  the telemetry keys; device names in a `lemnos-lite` table are `&'static str`. Dynamic
  names exist only in L3 and in services built on it.
- **`dyn` dispatch in L2.** Yes, measured: static dispatch of the four drivers through the
  L1 traits is 1.3 KB smaller than `dyn` on a Cortex-M4F, but every additional device type
  costs only its own functions under `dyn`, while a generic table would need a type per
  combination of devices. The table stays `dyn`; firmware that wants the last kilobyte can
  call the L1 traits directly.
- **Bridge priority.** Unchanged: after phase 3, once a product needs it.
