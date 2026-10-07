# TODO

Tracks Lemnos work driven by its integration into HeliOS
(`HeliOS-architecture-overhaul`, `backend/src/helios/peripherals`) and by
Lemnos 2.0 becoming the hardware foundation under Styx
([docs/foundation.md](docs/foundation.md)). See [CHANGELOG.md](CHANGELOG.md)
for the user-facing summary of finished work.

## Second pass ([docs/compact-model.md](docs/compact-model.md))

- [x] Phase 1: `lemnos-device` (compact L1 model) implemented by the BMI088, BMM150,
      INA2xx and VCM drivers; decisions confirmed; size images `device-static`, `device`.
- [x] Phase 2: `lemnos-lite` (static table, status, polling); firmware image `lite`,
      Linux images `sensors`/`sensors-lite`, example `linux_sensors`.
- [x] Phase 3: the generic L3 adapter (`lemnos_driver_sdk::l1`); board definitions
      (`lemnos-board`, facade `board`); hwmon fan and thermal zones as L1 devices; generic
      IIO/hwmon binding; HeliOS can bind the Raze sensors from `sensors.toml`
      ([docs/board-definition.md](docs/board-definition.md)).
- [ ] Phase 4: `lemnosd` (see `docs/system-service.md`).
- [ ] Phase 5: rich-model slimming (`Arc` in events, optional retention/diagnostics,
      shared probe code).

## Foundation (2.0)

### Done

- [x] Design: [docs/foundation.md](docs/foundation.md).
- [x] `lemnos-hal`: embedded-hal 1.0 + async re-exported; `RegisterBus`
      (blocking + async), `I2cRegisters`, `SpiRegisters`; `Regulator`,
      `ClockOutput`, `GpioRegulator`, `FixedClock`; `ErrorKind` (moved from
      `lemnos-core`); mocks. CI builds for thumbv7em, riscv32imac, wasm32.
- [x] `lemnos-core` and `lemnos-driver-manifest` build as `no_std` + `alloc`.
- [x] `lemnos_bus::hal`: sessions as embedded-hal buses and pins.
- [x] `lemnos-linux-sys` + `lemnos_linux::hal`: i2c-dev, spidev, GPIO uAPI v2,
      netlink uevents, inotify, poll in pure Rust; sessions and hotplug run on
      them; `i2cdev`, `spidev`, `gpio-cdev`, `inotify` dropped.
- [x] `lemnos-drivers-vcm`: DW9714, DW9807, DW9817, AK7375 (blocking + async).
- [x] Verified on the CM5: discovery, GPIO v2 line info and reads, I2C chip-id
      read of the camera sensor on bus 10, uevent hotplug on a USB
      `authorized` toggle, the Linux test suites on aarch64.

### Next

- [ ] Styx switch-over (Styx repo): `styx-sensor`'s `RegisterBus` becomes
      `lemnos_hal::RegisterBus`; `I2cRegisterBus` becomes
      `I2cRegisters<lemnos_linux::hal::I2cBus>`; lenses use
      `lemnos-drivers-vcm`; hotplug uses `lemnos_linux::uevent`; then delete
      `styx-kernel::bus::{i2c, gpio, ioctl}`, `uevent`, `regbus.rs` and the
      VCM code from Styx.
- [x] Sensor drivers as `no_std` crates over embedded-hal:
      `lemnos-drivers-bmi088`, `lemnos-drivers-bmm150`,
      `lemnos-drivers-ina2xx` (INA226/INA238/INA260), reached from the runtime
      through `lemnos_bus::hal` (see `tests/nostd_driver_in_runtime.rs`).
- [ ] Board validator example drivers
      (`crates/lemnos/examples/support/board_validator/drivers/`) still carry
      their own register code; switch them to the new crates. Needs a hardware
      run on the board. Note the validator also accepts the BMI055, which
      `lemnos-drivers-bmi088` does not support.
- [x] Runtime adapter drivers for the sensor crates: one generic adapter
      (`lemnos_driver_sdk::l1`) plus board definitions, so HeliOS binds them from
      `sensors.toml`.
- [ ] Slimmer runtime for small Linux targets (no fixed budget: as small as
      reasonably possible; `scripts/check-sizes.sh` tracks it). Linux lite
      facade consumer, stripped at `opt-level = "z"`: 640 KB -> 575 KB after
      removing sort instantiations and making threaded probing optional.
      - Toolchain lever (no code change): nightly `-Z build-std` with
        `-Cpanic=immediate-abort` and `-Cforce-unwind-tables=no` takes an
        empty std `main` from 282 KB to 14 KB and Linux lite to 234 KB.
        Worth documenting as the recommended build for constrained images.
      - What remains is structural: `Runtime::finish_refresh` (~28 KB, the
        inlined diff/event/retention/rebind pipeline), six Linux probes
        (~5 KB each, building `String`-keyed descriptors), and ~39 KB of
        `BTreeMap<String, Value>`/`BTreeMap<String, String>` code. Next step
        is the compact device model (`no_std`, no `String` keys) with today's
        descriptors layered on top, plus a lite runtime (static device
        table, no discovery/registry/event log) that MCUs and small SoCs can
        share. Design before refactoring `lemnos-core`.
- [ ] Port Styx's `usbfs` into `lemnos-linux` and drop `rusb`.
- [ ] Linux regulators and clocks behind `Regulator`/`ClockOutput` (regulator
      userspace-consumer and clock sysfs where the kernel exposes them).
- [ ] GPIO discovery from the character devices (line names, consumers) when
      `/sys/class/gpio` is absent (`CONFIG_GPIO_SYSFS=n`).
- [ ] `embedded_hal_async::digital::Wait` for `GpioLine` through a reactor;
      `SetDutyCycle` directly on sysfs PWM (today via `lemnos_bus::hal::HalPwm`).
- [ ] Publish 2.0.0 (crates.io order: hal, linux-sys, core, ...).

## Done

- [x] Async hotplug: `AsyncLinuxHotplugWatcher` (`lemnos-linux` `tokio`
      feature) waits on the watcher's descriptor (uevent socket or inotify)
      through Tokio's reactor; `LinuxHotplugWatcher` implements
      `AsFd`/`AsRawFd`.
- [x] Bind policy: `RuntimeBindPolicy` binds matching devices on refresh
      (interface, kind, or driver id). Default binds nothing; failed binds are
      not retried until the device changes.
- [x] Unbound status: `Runtime::device_status` and
      `DeviceStateSnapshot::status` return `DeviceStatus` without binding.
- [x] Built-in `HwmonFanDriver` (`lemnos.pwm.hwmon-fan`) with `fan.read`,
      `fan.set_duty`, `fan.set_pwm`, and `fan.set_mode`. Replaces the HeliOS
      copy and drops its `period_ns = 255` PWM mapping.
- [x] `MockHwmonFan` in `lemnos-mock` for sysfs-free fan driver tests.
- [x] `ErrorKind` plus `kind()` on every Lemnos error type, delegating through
      wrapped sources.
- [x] `Value::to_label_string` and `Value::flatten_labels`.
- [x] Hwmon probe no longer publishes live `fan.pwm`/`fan.pwm_mode`/`fan.rpm`
      descriptor properties, so speed changes don't show up as inventory diffs.

## Next

- [ ] Sensor drivers (BMI088, BMM150, INA238). On hold: decide on an
      extensible driver system first, so drivers can be added without
      rebuilding the crate. The existing drivers live only in
      `crates/lemnos/examples/support/board_validator/drivers/`. Devices on
      non-probeable buses can already be declared through
      `lemnos-core`'s `ConfiguredDeviceModel`.
- [ ] Memory breakdown for a Linux refresh on the CM5, measured with and
      without eager binds. HeliOS measured ~6-7 MiB PSS for a refresh including
      Styx/libcamera probing; Lemnos's share is unknown.
- [ ] Decide whether the `tokio` facade feature is worth its cost for
      consumers that only need `AsyncLinuxHotplugWatcher`.

## Release (do last)

- [ ] Merge `dev` into `main`.
- [x] Version: 2.0.0 (the foundation work is breaking; it also carries the
      HeliOS additions that were planned as 1.1.0).
- [ ] Tag the release; optionally publish to crates.io.
- [ ] HeliOS pins Lemnos to the tag instead of `branch = "main"`.

## HeliOS migration (HeliOS repo, not here)

- [ ] Replace the 250 ms hotplug sleep loop with
      `AsyncLinuxHotplugWatcher::next_events` and run refreshes in
      `spawn_blocking`.
- [ ] Set a bind policy for the fan driver and delete the eager bind loop in
      `LemnosDiscoveryProbe::discover`; drop the manual bind in `apply`
      (`auto_bind_on_request` covers it).
- [ ] Delete `lemnos_fan.rs` and use the built-in fan driver.
- [ ] Use `device_status`, `ErrorKind`, and `Value::flatten_labels` in place of
      the local mapping helpers.
- [ ] Publish deltas from `RuntimeWatchedRefreshReport::refresh.diff`.
- [ ] Add `lemnos-mock` tests for the discover/apply paths.
- [ ] Parse `sensors.toml` as a board definition (`lemnos::board::BoardSetup`, feature
      `board`); steps in [docs/board-definition.md](docs/board-definition.md).
