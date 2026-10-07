# Architecture Crate Map

Lemnos is intentionally layered. Higher-level crates depend on lower-level vocabulary and contracts rather than reaching directly across the workspace. [foundation.md](foundation.md) explains the 2.0 layering and the Styx migration.

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
   Bus-specific session traits and backend contracts for GPIO, PWM, I2C, SPI, UART, and USB access (the runtime's dyn convenience layer), and `lemnos_bus::hal`: embedded-hal views of those sessions.
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

- `lemnos-linux` is the one Linux implementation: discovery probes, sessions, hotplug (netlink uevents, inotify fallback), and embedded-hal implementations (`lemnos_linux::hal`: i2c-dev, spidev, GPIO uAPI v2, delay). It forbids `unsafe`.
- `lemnos-linux-sys` holds the workspace's only `unsafe` code: Linux uAPI structures and safe wrappers over the ioctls, sockets and syscalls `lemnos-linux` needs, each block with a `// SAFETY:` comment.
- `lemnos-drivers-gpio`, `-pwm`, `-i2c`, `-spi`, `-uart`, `-usb` provide generic runtime drivers and manifests for common interface kinds (std).
- `lemnos-drivers-vcm` is a `no_std` device driver crate (camera focus VCMs) over embedded-hal; future device drivers follow it.
- `lemnos-drivers-bmi088` (IMU), `lemnos-drivers-bmm150` (magnetometer) and `lemnos-drivers-ina2xx` (power monitors) are `no_std` sensor drivers in the same style. All four implement the `lemnos-device` model next to their own APIs.

- `lemnos-drivers-linux` holds device-model drivers over Linux kernel interfaces: `HwmonFan`, `ThermalZone`, and `KernelDevice`, the generic IIO/hwmon binding (std, file IO only).
- `lemnos-board` parses board definitions and builds device-model drivers from them through a `DriverRegistry`; hosts (the facade's `board` feature, `lemnosd`, small programs) provide buses through `Buses`.
- `lemnos_driver_sdk::l1` is the generic adapter from any device-model device to a runtime `BoundDevice`.

- `lemnos-light` (`no_std`) renders lights: easing, effects, gauges, spinners, system animations, and the arbitration of intents between owners. `lemnos-drivers-ws2812` (`no_std`) encodes strip frames; `lemnos_drivers_linux::Ws2812Pio` writes them to the RP1 PIO device.
- `lemnos-ipc` is the `lemnosd` protocol and client library; `lemnosd` is the service (a single-threaded host over device-model devices built with `lemnos-board`) and `lemnos-ctl`.

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
6. Applications use the `lemnos` facade or the lower-level crates directly; firmware and Styx use `lemnos-hal`, the device drivers, and `lemnos_linux::hal` without the runtime.
