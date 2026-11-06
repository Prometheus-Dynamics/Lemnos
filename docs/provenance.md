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
| `crates/kernel/src/{v4l2,subdev,media,event}*` (video nodes, buffers, controls, formats, capture, MMAP queues, subdevices, media topology, events; with `error`, `flags`, `fourcc`) | `lemnos-v4l2`, over the uAPI and calls in `lemnos-linux-sys::v4l2::device` and `lemnos-linux-sys::media` (2026-11-05). `bus` (the sensor bridge), `dma_heap` (in `lemnos-linux::dma`) and `v4l2::m2m` (in `lemnos-linux::v4l2::m2m`) did not move. |
| graph runtime's `crates/rga` (commit 2c3f7a0) | `lemnos-drivers-rga`: the V4L2 M2M route, and the multi-RGA route on `/dev/rga` (request from librga's Apache-2.0 `rga.h`, commit 331d5750, in `lemnos-linux-sys::rga`). The VGLite half is in the next row. |
| graph runtime's `crates/vglite` (commit a7ac3b4) | `lemnos-drivers-vglite`: resize, crop, flips and turns, fill, blend and palette on `/dev/vg_lite`, without the dlopen of `libvg_lite.so`. The command streams are ported from the K230 Linux SDK v0.6.1's `package/vg_lite/VGLite/vg_lite.c` and `vg_lite_image.c` (Vivante, MIT; the notice is kept in `command.rs`). The ioctl calls are in `lemnos-linux-sys::vg_lite`, with the layouts checked against `vg_lite_kernel.h`. Lines (the stroke path) are `Unsupported` until phase 2. Untested on board. |

| `crates/pisp` at `bb34279c` (Styx, branch `dev`): uAPI layouts, front and back end builders, tiling, statistics, `device` over the stock `rp1-cfe` and `pispbe` nodes | `lemnos-drivers-pisp` (`device` now on `lemnos-v4l2` and `lemnos-linux`, with the same names; debug variables `STYX_PISP_*` renamed `LEMNOS_PISP_*`; `styx.pisp.*` capability names renamed `lemnos.pisp.*`; untested on board since the move) |
| the camera stack's `crates/rkisp` (d17de2ed): the uAPI layouts and bindgen headers, `Isp`, `ParamBuffer`, `isp32::{Variant, nodes}`, and the `device` layer over the vendor `rkisp` driver's nodes | the layouts in `lemnos-rkisp-sys` (headers in its `headers/`, `scripts/rkisp-bindgen.sh`); `lemnos-drivers-rkisp` for `Isp`, `ParamBuffer`, `isp32` and `device` (on `lemnos-v4l2`; untested on board) |
| the camera stack's `kernel-modules/styx-sensor-bridge` (d17de2ed): the sensor bridge module (`styx_sensor_bridge.c/.h`), its device-tree overlays, Buildroot extension, install and spike scripts, `PROTOCOL.md`, and `kernel-env.sh` | `kernel-modules/lemnos-sensor-bridge` and `kernel-modules/kernel-env.sh` (GPL-2.0, out of tree). Renamed for Lemnos: the module, the platform driver, the device-tree compatible and properties, the overlays, the controls (the same ids; protocol version 1 unchanged) and the service unit. Untested on board. |
| the camera stack's `crates/kernel/src/bus` (d17de2ed): `bus.rs`, `bus/bridge.rs`, `bus/bridge_sys.rs` | `lemnos-sensor-bridge`: the bridge client over `lemnos-v4l2` (`bridge_sys.rs` holds the ABI constants and has no `unsafe`). Untested on board. |
| the camera stack's `crates/k230isp` (d17de2ed): the register map, `RegisterAccess`, the statistics register reads (`stats::{Raw, read_raw}`), and the `device` layer over the `vvcam-isp` ioctls | `lemnos-drivers-k230isp`; the ioctls are in `lemnos-linux-sys::vvcam_isp` (untested on board) |

Later (not in this change): `crates/kernel/src/usbfs.rs` replaces `rusb` in the USB transport;
sensor drivers (BMI088, BMM150, INA2xx) become no_std drivers (done: `lemnos-drivers-bmi088`, `lemnos-drivers-bmm150`, `lemnos-drivers-ina2xx`).

## What stays in Styx

Until the camera stack switches to `lemnos-v4l2` (block-move step 2): its own copies of the V4L2
video nodes, the media controller, subdevices, V4L2 events and controls. Kept: dma-heaps,
`BridgePins` (the bridge's pins; the bridge protocol moved, see above), `SubdevBus` / `KernelControl` (kernel
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

- [x] Port Styx's `usbfs` into `lemnos-linux` and drop `rusb`. Done as `lemnos-usb` (usbdevfs in `lemnos-linux-sys`, nusb elsewhere); see `docs/usb.md`. The Styx-side removal is Styx's own item.
