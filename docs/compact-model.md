# Compact device model and lite runtime

Status: proposal. Builds on [foundation.md](foundation.md).

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

## Decisions to confirm

- **Fixed-point values with a decimal exponent in L1.** Floats remain an opt-in
  conversion. *Recommended: yes.* It matches the drivers and keeps FPU-less targets free
  of float code.
- **`&'static str` names and static tables in L1/L2.** Dynamic names only in L3.
  *Recommended: yes.*
- **`dyn` dispatch in L2.** Generics would inline better for one or two devices but
  multiply code with many. *Recommended: `dyn`, measured both ways in phase 2.*
- **Bridge priority.** *Recommended: after phase 3, once there's a concrete MCU-plus-Linux
  product that needs it.*
