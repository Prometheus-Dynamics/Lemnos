# Lemnos 2.0: the hardware foundation under Styx

Status: decided and implemented (branch `foundation`, Lemnos 2.0.0); verified on the CM5. Lemnos owns every generic driver and
communication layer (buses, pins, power, hotplug, discovery). Styx keeps cameras only and
consumes Lemnos, on Linux and on microcontrollers. Generic code that grew up in Styx moves
here and is later deleted from Styx.

## Layering

```
 HeliOS, apps                Styx (cameras: V4L2, media, ISP, sensors, 3A)       MCU firmware
 ───────────────────────────────────────────────────────────────────────────────────────────
 lemnos            facade                                                    std
 lemnos-runtime    refresh, bind, state, async                               std
 lemnos-registry, lemnos-discovery, lemnos-driver-sdk                        std
 lemnos-drivers-{gpio,pwm,i2c,spi,uart,usb}  runtime drivers (interactions)  std
 lemnos-bus        dyn sessions (convenience layer) + session→embedded-hal   std
 lemnos-linux      the one Linux implementation: discovery, sessions,        std, no unsafe
                   embedded-hal impls (I2C, SPI, GPIO, delay, PWM),
                   hotplug (netlink uevent, inotify fallback)
 lemnos-linux-sys  ioctls, sockets, inotify, #[repr(C)] uAPI structs         std, the only unsafe
 ───────────────────────────────────────────────────────────────────────────────────────────
 lemnos-drivers-vcm (and future device drivers): generic over embedded-hal   no_std, no alloc
 lemnos-core, lemnos-driver-manifest: descriptors, values, requests          no_std + alloc
 lemnos-hal        embedded-hal 1.0 + embedded-hal-async (re-exported),      no_std, no alloc
                   RegisterBus (I2C/SPI register maps, blocking + async),
                   Regulator, ClockOutput, ErrorKind, mock platform
```

Rules:

1. **embedded-hal is the bus vocabulary.** Device drivers take `I2c`, `SpiDevice`,
   `OutputPin`, `DelayNs` (or the async traits) and nothing else. They never see Linux or the
   Lemnos runtime, so the same driver runs on a CM5 and on a Cortex-M.
2. **`lemnos-hal` adds only what embedded-hal lacks**: register maps over I2C/SPI (8/16-bit
   addresses, 1-4 byte big-endian values, bursts, read-back, read-modify-write, zero
   allocation), regulators, clock outputs, and one portable `ErrorKind`.
3. **One Linux implementation.** `lemnos-linux` implements embedded-hal over i2c-dev,
   spidev, GPIO uAPI v2 and sysfs PWM, and the existing `lemnos-bus` sessions over the same
   code. No `i2cdev`, `spidev`, `gpio-cdev` (v1 ABI, deprecated upstream) or `inotify` crates.
4. **Unsafe lives in one crate.** `unsafe_code = "forbid"` stays workspace-wide;
   `lemnos-linux-sys` opts out (its own lint table), exposes only safe functions, and every
   `unsafe` block carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks` is
   denied there).
5. **The dyn session API stays** as a convenience layer for the runtime: discovery, binding,
   interactions and HeliOS keep working unchanged. `lemnos-bus::hal` adapts any session to
   embedded-hal, so no_std drivers also run inside the runtime.

## What moves from Styx

| Styx source (branch `dev`) | Lemnos destination |
|---|---|
| `crates/kernel/src/bus/ioctl.rs` (`_IOC` encoding, `EINTR`-retrying ioctl) | `lemnos-linux-sys::ioctl` |
| `crates/kernel/src/bus/i2c.rs` (`I2C_SLAVE`, `I2C_FUNCS`, `I2C_RDWR`, message limits, encoders) | `lemnos-linux-sys::i2c` (uAPI) + `lemnos_linux::hal::I2cBus` (embedded-hal `I2c`, sessions) |
| `crates/kernel/src/bus/gpio.rs` (GPIO v2 chip info, line info, requests, values) | `lemnos-linux-sys::gpio` + `lemnos_linux::hal::{GpioChip, GpioLines, GpioLine}`; extended with inputs, bias, debounce, edge events, reconfiguration |
| `crates/kernel/src/uevent.rs` (`NETLINK_KOBJECT_UEVENT`, parser) | `lemnos-linux-sys::netlink` + `lemnos_linux::uevent`; drives `LinuxHotplugWatcher` |
| `crates/kernel/src/ioctl.rs` (`poll_fd`) | `lemnos-linux-sys::poll` |
| `crates/sensor/src/bus.rs` (`RegisterBus`, `RegWrite`, `MockBus`) | `lemnos_hal::register::{RegisterBus, RegWrite}`, `lemnos_hal::mock` |
| `crates/sensor/src/bus_error.rs` (`BusErrorKind`) | `lemnos_hal::ErrorKind` (superset) |
| `crates/native/src/regbus.rs` (`I2cRegisterBus`: big-endian encoding, `with_bursts`, one message per transfer on RP1) | `lemnos_hal::register::I2cRegisters` (blocking + async) |
| `crates/sensor/src/lens.rs` (`VcmChip`, `VcmFormat::encode`) + `crates/native/src/lens.rs` (`I2cVcm`) | `lemnos-drivers-vcm` (DW9714, DW9807, DW9817, AK7375, custom formats; blocking + async) |
| `SensorPins::{set_gpio, set_clock, set_supply}` (generic halves) | `OutputPin`, `lemnos_hal::{Regulator, ClockOutput}`; `GpioRegulator`, `FixedClock` |

Later (not in this change): `crates/kernel/src/usbfs.rs` replaces `rusb` in the USB transport;
sensor drivers (BMI088, BMM150, INA2xx) become no_std drivers.

## What stays in Styx

V4L2 video nodes, the media controller, subdevices, V4L2 events and controls, dma-heaps,
the `styx-sensor-bridge` protocol and `BridgePins`, `SubdevBus` / `KernelControl` (kernel
sensor drivers), sensor descriptions and `SensorDriver`, the role-name mapping of a sensor's
power sequence (`SensorPins` becomes a thin camera trait over Lemnos pins, regulators and
clocks), `LensDescription` / `LensSchedule` / `LensMotion` (frame timing), PDAF decoding,
UVC, the ISPs and 3A. Styx's time `Clock` stays in `styx-hal`; Lemnos's clock trait is
`ClockOutput` (a clock signal) to avoid the clash.

## Crate-by-crate changes

- **`lemnos-hal` (new).** `#![no_std]`, no alloc. Re-exports `embedded_hal` and
  `embedded_hal_async`. `ErrorKind` (moved from `lemnos-core`, plus `Nack` and
  `Overrun`), the `HalError` trait, conversions from embedded-hal's I2C/SPI/digital error
  kinds and (feature `std`) from `std::io::ErrorKind`. `register`: `RegisterBus` and
  `asynch::RegisterBus`, `I2cRegisters`, `SpiRegisters`, `RegWrite`, encoders. `power`:
  `Regulator`, `ClockOutput`, `GpioRegulator`, `FixedClock`. `mock` (feature, alloc): I2C
  register-file targets, SPI, pins, delay, regulator, clock; blocking and async.
- **`lemnos-core`.** `no_std` + `alloc`; default feature `std`. `ErrorKind` is re-exported
  from `lemnos-hal` (same path, same variants plus two).
- **`lemnos-driver-manifest`.** `no_std` + `alloc`; default feature `std`.
- **`lemnos-bus`.** Unchanged session traits. `BusError` implements embedded-hal's
  `i2c::Error`, `spi::Error` and `digital::Error`; `lemnos_bus::hal` adapts
  `I2cSession`/`I2cControllerSession`/`SpiSession`/`GpioSession` to embedded-hal.
- **`lemnos-linux-sys` (new).** Safe wrappers over i2c-dev (incl. SMBus), spidev, GPIO v2,
  netlink uevent sockets, inotify, `poll`. Layout tests against the kernel headers.
- **`lemnos-linux`.** Transports rewritten over `lemnos-linux-sys`; `pub mod hal` with the
  embedded-hal implementations; `pub mod uevent`. The `gpio-cdev` feature name is kept and now
  means GPIO uAPI v2 (Linux ≥ 5.10); bias and debounce are supported, and edge streams
  (`open_gpio_edge_stream`) work on cdev lines. `LinuxHotplugWatcher` uses netlink uevents on
  the real `/sys` and falls back to inotify for test roots or when the socket is refused.
- **`lemnos-drivers-vcm` (new).** no_std VCM lens drivers.
- **Runtime crates** (`discovery`, `registry`, `runtime`, `driver-sdk`, `lemnos`, the
  built-in `lemnos-drivers-*`, `mock`, `macros`) stay std. The built-in drivers are runtime
  adapters (interactions over sessions), not device drivers, so making them no_std would buy
  nothing; device drivers go into no_std crates and reach the runtime through
  `lemnos_bus::hal`.

## Compatibility layer

- Every public `lemnos-bus` trait, `LinuxBackend`, the probes, `LinuxHotplugWatcher`
  (`AsFd`, `InventoryWatcher`) and `AsyncLinuxHotplugWatcher` keep their signatures.
- `lemnos_core::ErrorKind` still resolves (re-export); `ErrorKind::from_io` still exists with
  `std`. Matches on it already needed a wildcard (`#[non_exhaustive]`).
- Feature names are unchanged (`gpio-cdev`, `hotplug`, `i2c`, `spi`, ...).
- Facade users (`lemnos = { features = [...] }`) change only the version.

## Versioning

The workspace becomes **2.0.0**: the dependency graph changes (crates removed, new crates),
`lemnos-core` gains a default `std` feature (users with `default-features = false` must add
`std` or accept no_std), `ErrorKind` gains variants, and GPIO cdev requires the v2 uAPI.
MSRV stays 1.94. HeliOS (pinned to a git rev) updates when it chooses; no code change is
expected beyond the version.

## Migration steps

1. Lemnos 2.0 lands (this branch): the foundation crates, Linux rewrite, VCM drivers.
2. Styx `native/hal`: `styx-hal` uses embedded-hal directly and keeps camera traits only;
   `styx-sensor`'s `RegisterBus` becomes `lemnos_hal::RegisterBus` (re-export) and
   `I2cRegisterBus` becomes `lemnos_hal::I2cRegisters<lemnos_linux::hal::I2cBus>`.
3. Styx `native`: open I2C/GPIO through `lemnos_linux::hal`, lenses through
   `lemnos_drivers_vcm`, hotplug through `lemnos_linux::uevent`.
4. Delete `styx-kernel::bus::{i2c, gpio, ioctl}`, `uevent`, `regbus.rs` and the VCM code from
   Styx. `styx-kernel` keeps V4L2/media/subdev/dma-heap/bridge.
5. Port `usbfs` into `lemnos-linux` and drop `rusb`; port the sensor drivers.

## How Styx consumes Lemnos

On Linux:

```rust
use lemnos_hal::register::{I2cRegisters, AddressWidth};
use lemnos_linux::hal::{I2cBus, StdDelay};

// /dev/i2c-10; each target address is claimed with I2C_SLAVE (never _FORCE) before use.
let mut sensor = I2cRegisters::new(I2cBus::open(10)?, 0x60, AddressWidth::Bits16).with_bursts(32);
let id = sensor.read(0x300a, 2)?;                       // chip id: 0x9281 on the CM5 module
let mut lens = lemnos_drivers_vcm::Vcm::dw9817(I2cBus::open(10)?);
lens.power_up(&mut StdDelay)?;
lens.move_to(512)?;
```

On an MCU, the firmware's HAL supplies the `I2c` (e.g. `embassy-stm32`'s async I2C) and the
same `I2cRegisters` and `Vcm` run over it with `default-features = false`; nothing from the
std crates is linked. CI builds `lemnos-hal`, `lemnos-core`, `lemnos-driver-manifest` and
`lemnos-drivers-vcm` for `thumbv7em-none-eabihf`, `riscv32imac-unknown-none-elf` and
`wasm32-unknown-unknown`.
