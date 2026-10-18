# Lemnos 2.0: the hardware foundation

Status: decided and implemented (branch `foundation`, Lemnos 2.0.0); verified on the BCM2712 board. Lemnos owns every generic driver and
communication layer (buses, pins, power, hotplug, discovery). Camera stacks keep cameras only and
consume Lemnos, on Linux and on microcontrollers. Where the generic code came from, and the
cross-repo migration, is recorded in [provenance.md](provenance.md).

## Layering

```
 consuming apps          camera stack (V4L2, media, ISP, 3A)                 MCU firmware
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
 lemnos-drivers-{vcm,bmi088,bmm150,ina2xx}: device drivers over embedded-hal no_std, no alloc
 lemnos-core, lemnos-driver-manifest: descriptors, values, requests          no_std + alloc
 lemnos-hal        embedded-hal 1.0 + embedded-hal-async (re-exported),      no_std, no alloc
                   RegisterBus (I2C/SPI register maps, blocking + async),
                   Regulator, ClockOutput, ErrorKind, mock platform
```

Rules:

1. **embedded-hal is the bus vocabulary.** Device drivers take `I2c`, `SpiDevice`,
   `OutputPin`, `DelayNs` (or the async traits) and nothing else. They never see Linux or the
   Lemnos runtime, so the same driver runs on a BCM2712 and on a Cortex-M.
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
   interactions and the app keep working unchanged. `lemnos-bus::hal` adapts any session to
   embedded-hal, so no_std drivers also run inside the runtime.

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
MSRV stays 1.94. Consumers pinned to a git rev update when they choose; no code change is
expected beyond the version.

## Using it from Linux and from an MCU

On Linux:

```rust
use lemnos_hal::register::{I2cRegisters, AddressWidth};
use lemnos_linux::hal::{I2cBus, StdDelay};

// /dev/i2c-10; each target address is claimed with I2C_SLAVE (never _FORCE) before use.
let mut sensor = I2cRegisters::new(I2cBus::open(10)?, 0x60, AddressWidth::Bits16).with_bursts(32);
let id = sensor.read(0x300a, 2)?;                       // chip id: 0x9281 on the BCM2712 board module
let mut lens = lemnos_drivers_vcm::Vcm::dw9817(I2cBus::open(10)?);
lens.power_up(&mut StdDelay)?;
lens.move_to(512)?;
```

On an MCU, the firmware's HAL supplies the `I2c` (e.g. `embassy-stm32`'s async I2C) and the
same `I2cRegisters` and `Vcm` run over it with `default-features = false`; nothing from the
std crates is linked. CI builds `lemnos-hal`, `lemnos-core`, `lemnos-driver-manifest`,
`lemnos-drivers-vcm`, `lemnos-drivers-bmi088`, `lemnos-drivers-bmm150` and
`lemnos-drivers-ina2xx` for `thumbv7em-none-eabihf`, `riscv32imac-unknown-none-elf` and
`wasm32-unknown-unknown`.
