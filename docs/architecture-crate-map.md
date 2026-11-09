# Architecture Crate Map

Lemnos is intentionally layered. Higher-level crates depend on lower-level vocabulary and contracts rather than reaching directly across the workspace. [foundation.md](foundation.md) explains the 2.0 layering; [provenance.md](provenance.md) records where the generic code came from.

## Layering

0. `lemnos-hal` (`no_std`, no allocation)
   embedded-hal 1.0 and embedded-hal-async (re-exported) as the bus vocabulary, plus what they lack: register maps over I2C/SPI (`RegisterBus`, blocking and async), `Regulator`, `ClockOutput`, the portable `ErrorKind`, and mocks.
0. `lemnos-device` (`no_std`, no allocation)
   The compact device model: device classes, quantities with canonical units, fixed-point channels and controls, the `Device`/`Sensor`/`Control` traits drivers implement, their object-safe forms, kernel IIO/hwmon bindings as data, and `DeviceStatus`. See [compact-model.md](compact-model.md).
0. `lemnos-lite` (`no_std`, no allocation)
   A static table of L1 devices with status, last error and polling, for firmware and small Linux images.
1. `lemnos-core` (`no_std` + `alloc`)
   Shared device descriptors, interaction requests and responses, state snapshots, events, issues, and value types.
2. `lemnos-bus`
   Bus-specific session traits and backend contracts for GPIO, PWM, I2C, SPI, UART, and USB access (the runtime's dyn convenience layer), `lemnos_bus::hal`: embedded-hal views of those sessions, and `lemnos_bus::transfer`: one way to start, poll, wait for and cancel a transfer over memory or dma-bufs ([dma.md](dma.md)).
3. `lemnos-discovery`
   Discovery probes, inventory snapshots, probe reports, diffing, and watch events.
4. `lemnos-driver-manifest` (`no_std` + `alloc`)
   Driver identity, compatibility rules, interaction manifests, and match metadata.
5. `lemnos-driver-sdk`
   Driver authoring surface: bind contexts, bus IO helpers, conformance harnesses, state helpers, and transport adapters.
6. `lemnos-registry`
   Driver registration and best-match selection.
7. `lemnos-runtime`
   Refresh, bind, runtime state, diagnostics, subscriptions, and optional async support.
8. `lemnos`
   Consumer-facing facade that re-exports the public building blocks (including `lemnos::hal`) and composes optional features.

## Backend And Driver Crates

- `lemnos-linux` is the one Linux implementation: discovery probes, sessions, hotplug (netlink uevents, inotify fallback), and embedded-hal implementations (`lemnos_linux::hal`: i2c-dev, spidev, GPIO uAPI v2, delay). Behind features it adds DMA buffers (`dma`, [dma.md](dma.md)), V4L2 memory-to-memory sessions (`v4l2-m2m`, [m2m.md](m2m.md)) and the SPI and I2C transfer engines. It forbids `unsafe`.
- `lemnos-linux-sys` holds the kernel side of the workspace's `unsafe` code (the C boundary's is in `lemnos-capi`): Linux uAPI structures and safe wrappers over the ioctls, sockets and syscalls `lemnos-linux` needs (including dma-heap, dma-buf, V4L2, `mmap`, the vendor multi-RGA request on `/dev/rga`, the K230's `/dev/vg_lite` structures), each block with a `// SAFETY:` comment.
- `lemnos-v4l2` is the safe V4L2 layer: video nodes (formats, buffers with MMAP and DMA-buf queues, controls, capture streaming), subdevices (formats, media-bus codes, routing, selections, frame intervals), the media controller (topology, links, requests) and V4L2 events. Every call goes through `lemnos-linux-sys` (`v4l2::device`, `media`); it forbids `unsafe`. Memory-to-memory sessions stay in `lemnos-linux` (`v4l2-m2m`), which takes `lemnos-v4l2`'s format and buffer types (the sys calls under them are `lemnos-linux-sys`'s).
- `lemnos-usb` is the one USB layer: listing, hotplug, descriptors, control, bulk, interrupt and isochronous transfers in Lemnos's own types. Linux runs on usbdevfs (the `usbfs` module of `lemnos-linux-sys`); Windows and macOS run on nusb. `lemnos-linux`'s `usb` feature is its bus session. See `docs/usb.md`.
- `lemnos-drivers-gpio`, `-pwm`, `-i2c`, `-spi`, `-uart`, `-usb` provide generic runtime drivers and manifests for common interface kinds (std).
- `lemnos-drivers-vglite` is the K230's VGLite 2.5D GPU on `/dev/vg_lite`: resize, crop, flips and turns, fill, blend and palette, with the vendor library's command streams ported to Rust and no dlopen. Lines wait for the stroke port and return `Unsupported`. No `unsafe` in the crate; the ioctl calls it uses are in `lemnos-linux-sys::vg_lite`. `crates/lemnos-drivers-vglite/README.md`.
- `lemnos-drivers-rga` is Rockchip's RGA 2D engine over two routes: the mainline V4L2 mem2mem driver (convert, scale, crop, flip, turn) and the vendor multi-RGA node `/dev/rga` (fill, mask ROP, blend, palette, colour key, gray to RGBA, flips and turns), with the route table that says which route serves each operation on which SoC. No `unsafe`; Linux only. `crates/lemnos-drivers-rga/README.md`.
- `lemnos-drivers-g2d` is the Allwinner G2D 2D engine on the BSP kernel's `/dev/g2d`: fill, scale, convert, turn, flip and blend between dma-bufs. Untested on board.
- `lemnos-drivers-nonai2d` is the K230 non-AI 2D engine's format conversion, on the K230 Linux SDK's V4L2 mem2mem driver. Untested on board.
- `lemnos-drivers-vcm` is a `no_std` device driver crate (camera focus VCMs) over embedded-hal; future device drivers follow it.
- `lemnos-drivers-bmi088` (IMU), `lemnos-drivers-bmm150` (magnetometer) and `lemnos-drivers-ina2xx` (power monitors) are `no_std` sensor drivers in the same style. All four implement the `lemnos-device` model next to their own APIs.
- `lemnos-drivers-pisp` is the Raspberry Pi 5 ISP (PiSP): the uAPI layouts, the front and back end configuration builders, statistics decoding (`no_std` + `alloc`); with feature `device` it drives the `rp1-cfe` and `pispbe` nodes through `lemnos-v4l2` and `lemnos-linux`. See `crates/lemnos-drivers-pisp/README.md`.
- `lemnos-drivers-rkisp` is the Rockchip ISP (ISP21, ISP30, ISP32 and ISP32-lite): the vendor `rkisp` driver's parameter and statistics uAPI layouts, `Isp` (the generation from a SoC's compatible strings), `ParamBuffer`, and with feature `device` the driver's nodes through `lemnos-v4l2`. Untested on board. It forbids unsafe code and is `no_std` + `alloc` (std for detection and `device`); its uAPI is re-exported from `lemnos-rkisp-sys`. See `crates/lemnos-drivers-rkisp/README.md`.
- `lemnos-rkisp-sys` is the `-sys` crate of the Rockchip ISP: the uAPI layouts of ISP21, ISP30, ISP32 and ISP32-lite, `no_std`, generated by bindgen from the kernel headers (`scripts/rkisp-bindgen.sh`). Its unsafe code is the ISP32 bitfield accessors (bindgen output) and the bitfield unit's `Pod` impls, the reasons in the code. No device code. See `crates/lemnos-rkisp-sys/README.md`.
- `lemnos-sensor-bridge` is the userspace client of the sensor bridge kernel module (`kernel-modules/lemnos-sensor-bridge`, GPL-2.0, out of tree): `SensorBridge` opens the bridge's subdev, publishes its pad format and timing, and runs the stream start and stop handshake, and `find_bridges` lists the bridges bound. It talks to the module only through the V4L2 controls and events of `lemnos-v4l2`, with no `unsafe`.
- `lemnos-drivers-k230isp` is the K230D's VeriSilicon ISP8000 (`/dev/vvcam-isp.0`): the register map, the `RegisterAccess` trait, the statistics register reads, and with feature `device` the register ioctls (`lemnos-linux-sys::vvcam_isp`). No `unsafe`; untested on board. See `crates/lemnos-drivers-k230isp/README.md`.

- `lemnos-hevc` (`no_std`, no allocation) parses HEVC bitstreams for a stateless decoder's controls: Annex B NAL units, VPS, SPS, PPS and an IDR slice header. It parses; it decodes nothing. The V4L2 control structures are in `lemnos-linux-sys`, the conversion in `lemnos_linux::v4l2::hevc` (feature `v4l2-hevc`).
- `lemnos-abi` (`no_std`) holds the C-ABI building blocks every surface shares: `Status`, generation-counted `Handle`s and their `Slab`, `BufferDesc` and `FenceDesc`, `MessageSink`, the `AbiVersion` handshake with a layout hash, and a panic guard for `extern "C"` entry points.
- `lemnos-capi` is the C surface over the `lemnosd` device client: ten functions in `include/lemnos.h`, built as `staticlib` and `cdylib`, and `Client`, a Rust wrapper that calls through them. Its unsafe code is the C boundary's, one operation per block.
- `lemnos-soc` (`no_std`) holds the chip descriptors: each SoC's IO blocks and capabilities as used, unused or impossible, and the typed register layouts of the Rockchip PWM and timer and the CLINT. See [io-blocks.md](io-blocks.md).
- `lemnos-drivers-linux` holds device-model drivers over Linux kernel interfaces: `HwmonFan`, `ThermalZone`, and `KernelDevice`, the generic IIO/hwmon binding, with buffered IIO reads (std, file IO and poll).
- `lemnos-board` parses board definitions and builds device-model drivers from them through a `DriverRegistry`; hosts (the facade's `board` feature, `lemnosd`, small programs) provide buses through `Buses`.
- `lemnos_driver_sdk::l1` is the generic adapter from any device-model device to a runtime `BoundDevice`.

- `lemnos-light` (`no_std`) renders lights: easing, effects, gauges, spinners, system animations, and the arbitration of intents between owners. `lemnos-drivers-ws2812` (`no_std`) encodes strip frames; `lemnos_drivers_linux::Ws2812Pio` writes them to the RP1 PIO device.
- `lemnos-ipc` is the `lemnosd` protocol and client library; `lemnosd` is the service (a single-threaded host over device-model devices built with `lemnos-board`). `lemnos-ctl` is its command-line client, its own crate (`crates/lemnos-ctl`) so it does not link the service: board and look-file tooling is behind its `board` feature (default on).

## Authoring And Test Support

- `lemnos-macros` reduces boilerplate for configured-device and driver definitions.
- `lemnos-mock` provides fake hardware for examples and tests; `lemnos-hal`'s `mock` feature provides embedded-hal doubles for no_std drivers.
- `lemnos-macros-tests` holds compile-time and integration coverage for macro behavior and is not published.

## Typical Flow

1. A backend or probe populates `lemnos-discovery` inventory using `lemnos-core` descriptors.
2. A driver manifest is matched against discovered devices.
3. The registry selects a candidate driver.
4. The runtime opens typed sessions through `lemnos-bus`.
5. The bound driver performs interactions and updates runtime state, directly or through an embedded-hal device driver over `lemnos_bus::hal`.
6. Applications use the `lemnos` facade or the lower-level crates directly; firmware and camera stacks use `lemnos-hal`, the device drivers, and `lemnos_linux::hal` without the runtime.
