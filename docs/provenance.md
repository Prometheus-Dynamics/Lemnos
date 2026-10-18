# Provenance and migration record

Where the generic code in Lemnos came from, and the cross-repo migration that moved it.
This is a record for the owning repositories; the design itself is in
[foundation.md](foundation.md). Lemnos does not depend on the source repository.

## Code moved into Lemnos

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
sensor drivers (BMI088, BMM150, INA2xx) become no_std drivers (done: `lemnos-drivers-bmi088`, `lemnos-drivers-bmm150`, `lemnos-drivers-ina2xx`).

## What stays in Styx

V4L2 video nodes, the media controller, subdevices, V4L2 events and controls, dma-heaps,
the `styx-sensor-bridge` protocol and `BridgePins`, `SubdevBus` / `KernelControl` (kernel
sensor drivers), sensor descriptions and `SensorDriver`, the role-name mapping of a sensor's
power sequence (`SensorPins` becomes a thin camera trait over Lemnos pins, regulators and
clocks), `LensDescription` / `LensSchedule` / `LensMotion` (frame timing), PDAF decoding,
UVC, the ISPs and 3A. Styx's time `Clock` stays in `styx-hal`; Lemnos's clock trait is
`ClockOutput` (a clock signal) to avoid the clash.


## Migration steps (source repository)

1. Lemnos 2.0 lands (this branch): the foundation crates, Linux rewrite, VCM drivers.
2. Styx `native/hal`: `styx-hal` uses embedded-hal directly and keeps camera traits only;
   `styx-sensor`'s `RegisterBus` becomes `lemnos_hal::RegisterBus` (re-export) and
   `I2cRegisterBus` becomes `lemnos_hal::I2cRegisters<lemnos_linux::hal::I2cBus>`.
3. Styx `native`: open I2C/GPIO through `lemnos_linux::hal`, lenses through
   `lemnos_drivers_vcm`, hotplug through `lemnos_linux::uevent`.
4. Delete `styx-kernel::bus::{i2c, gpio, ioctl}`, `uevent`, `regbus.rs` and the VCM code from
   Styx. `styx-kernel` keeps V4L2/media/subdev/dma-heap/bridge.
5. Port `usbfs` into `lemnos-linux` and drop `rusb`. (The sensor drivers are ported.)

## Open cross-repo steps (camera stack)

- [ ] Styx switch-over (Styx repo): `styx-sensor`'s `RegisterBus` becomes
      `lemnos_hal::RegisterBus`; `I2cRegisterBus` becomes
      `I2cRegisters<lemnos_linux::hal::I2cBus>`; lenses use
      `lemnos-drivers-vcm`; hotplug uses `lemnos_linux::uevent`; then delete
      `styx-kernel::bus::{i2c, gpio, ioctl}`, `uevent`, `regbus.rs` and the
      VCM code from Styx.

- [ ] Port Styx's `usbfs` into `lemnos-linux` and drop `rusb`.
