# TODO

Tracks Lemnos work driven by the apps that integrate it and by the camera stack that
consumes its foundation
([docs/foundation.md](docs/foundation.md)). See [CHANGELOG.md](CHANGELOG.md)
for the user-facing summary of finished work.

## Next release (1.0.0)

The release list: the 2026-10-27 feature freeze plus the hardware-support work added on 2026-10-30
(the last addition). Everything else stays under the headings below. Every item: what, why, done
when, blocked by.

### 1. Freeze items still open

- [ ] **IMU above 100 Hz.** Why: the frozen IMU-rate item. The BMI088 sits on bit-banged I2C
      (i2c-gpio GPIO8/7), which costs about a third of a core at 100 Hz. Done when: on a board with a
      hardware I2C pair, the IMU runs at 200 Hz or more through lemnosd within a stated CPU budget,
      measured on the board. Blocked by: the next Raze board revision (PCB).
- [x] **The tokio facade decision.** The user keeps the `tokio` facade feature (2026-10-30).
- [x] BCM2712 bring-up and timing (a5496ee), the size baseline, the surface trim and the API dedup
      (1ed6fdd, f5754ec, 66a354d, b87f76c).

### 2. Hardware support (added 2026-10-30)

- [ ] **riscv64 and armv7 builds.** Why: lemnosd runs on the K230D (riscv64) and Flame (RV1106G2,
      Cortex-A7). Done when: lemnosd and its client build for riscv64gc-unknown-linux-gnu/musl and
      armv7-unknown-linux-gnueabihf in CI, with the size reported per target. Blocked by: nothing.
- [ ] **Board definitions for the new boards.** Why: each board's LEDs, fan, power monitor, IMU and
      spare I/O come from its board file. Done when: board files for the K230D board, the Rockchip
      modules (RK3588S, RK3576, RK3566 and RK3562) and Flame load in lemnosd on the real board,
      with their devices bound and the LED ring and fan driven. Blocked by: boards (PCB).
- [ ] **Drivers those boards need.** Why: any LED, fan, sensor or power chip on the new boards that
      Lemnos doesn't drive yet. Done when: each is a driver behind the extensible driver system,
      tested on its board. Blocked by: the boards' BOMs (PCB) and the driver-system decision under
      "Next".
- [ ] **Memory on small boards.** Why: the K230D has 128 MB in total. Done when: lemnosd PSS is
      measured on the K230D and Flame boards (1.6 MB RSS on the CM5 today). Blocked by: boards.

### 3. Release mechanics

Version 1.0.0 (agreed with the user on 2026-10-30, tagged when this feature push is done; the
workspace says 2.0.0 today and is set to 1.0.0 in the release commit). See "Release (do last)"
below: merge `dev` into `main`, tag 1.0.0, and have consumers (HeliOS,
Styx, Atlas, the Gaia `lemnos` source) pin the tag. Gates on the release commit: fmt, clippy -D
warnings, tests, and the size images.

## Second pass ([docs/compact-model.md](docs/compact-model.md))

- [x] Phase 1: `lemnos-device` (compact L1 model) implemented by the BMI088, BMM150,
      INA2xx and VCM drivers; decisions confirmed; size images `device-static`, `device`.
- [x] Phase 2: `lemnos-lite` (static table, status, polling); firmware image `lite`,
      Linux images `sensors`/`sensors-lite`, example `linux_sensors`.
- [x] Phase 3: the generic L3 adapter (`lemnos_driver_sdk::l1`); board definitions
      (`lemnos-board`, facade `board`); hwmon fan and thermal zones as L1 devices; generic
      IIO/hwmon binding; an app can bind a board's sensors from its board file
      ([docs/board-definition.md](docs/board-definition.md)).
- [x] Phase 4: `lemnosd`, `lemnos-ipc`, `lemnos-light`, `lemnos-drivers-ws2812`, packaging
      ([docs/system-service.md](docs/system-service.md)).
- [ ] `lemnosd` on the BCM2712 board: run the example board definition against the real ring, fan and
      sensors; check the ring's `wire` (the manager's manifest says RGBW) and the fan's
      `restore_mode`; measure the aarch64 binary. Measured on a Raze (HeliOS dev.800, kernel
      7.2.9, 2026-10-25 board clock):
      - Ring `wire = "rgb"` (GRB24) is right: the manifest has `wire_format: "grb24"` and
        `rgbw: false`; its RGBW is the 4-byte userspace layout (W ignored), which the driver
        writes. The live overlay has no `rgbw` property. Pure R/G/B sent through the test layer
        show as logical frames 00590000, 00005900, 00000059. Pure W can't be sent through
        `lemnos-ctl` or lemnosd (colours are 24-bit), so W and the colours on the LEDs are not
        verified here.
      - Fan: no `restore_mode` (`pwm-fan` hands back through its cooling device). A stop and
        start kept `pwm1_enable = 1`, `pwm1 = 179`, `cur_state = 0`; the start re-recorded
        `/run/lemnos/fan-restore`.
      - `lemnosd` binary: 1240648 bytes on disk; RSS 1600 kB, PSS 1588 kB; 0.43 % of a core
        idle over 60 s, 1.35 % with an IMU subscriber at 100 Hz.
- [ ] Composite devices and the fan controller: design only
      ([docs/composite-devices.md](docs/composite-devices.md)), waiting for a decision.
- [ ] IIO buffered reads (`/dev/iio:deviceN`, IRQ timestamps) in the generic kernel
      binding.
- [x] BMI088 FIFO batch reads (opt-in, `fifo = true`): `lemnos-drivers-bmi088::fifo`, one
      batch per drain, samples stamped from the output rate. Not a win on the installed
      board (100 Hz accelerometer, 2 kHz gyroscope defaults: 87 % of a core at 100 Hz vs
      36 % polled); needs matched output rates and a hardware I2C pair for 200 Hz and up.
- [ ] Board: the IMU's output rate (`accel_rate`, `gyro_rate`) is the limit above 100 Hz;
      the manager to set it for the rate the consumers need, and to move the IMU off i2c-gpio.
- [ ] Measure the scheduler on the BCM2712 board (`tests/schedule.rs` runs on the host with a mock
      bus): the IMU at 100 Hz beside the real power monitor and magnetometer on the board's
      buses, with the reads' real durations. Partly measured on the Raze: 6201 IMU readings in
      about 62 s with the real service polling the power monitor and magnetometer; spacing mean
      10000 us, deviation from the 10 ms grid at most 251 us (client-side, from the readings'
      timestamps). Read durations are not exposed (`read_cost_us` stays inside lemnosd), so
      they still need instrumentation.
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
- [x] Verified on the BCM2712 board: discovery, GPIO v2 line info and reads, I2C chip-id
      read of the camera sensor on bus 10, uevent hotplug on a USB
      `authorized` toggle, the Linux test suites on aarch64.

### Next

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
      (`lemnos_driver_sdk::l1`) plus board definitions, so apps bind them from
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
- [x] Linux regulators and clocks behind `Regulator`/`ClockOutput`:
      `lemnos_drivers_linux::{UserspaceRegulator, DebugfsClock}` (the
      `reg-userspace-consumer` `state` attribute; debugfs clock rates, gating left to the
      kernel).
- [x] GPIO discovery from the character devices (chip labels, line names, consumers)
      when `/sys/class/gpio` is absent (`CONFIG_GPIO_SYSFS=n`), feature `gpio-cdev`.
- [x] `embedded_hal_async::digital::Wait` for GPIO lines through Tokio's reactor:
      `lemnos_linux::hal::AsyncGpioLine` (`gpio-cdev` + `tokio`). Needs a hardware check.
- [ ] `SetDutyCycle` directly on sysfs PWM (today via `lemnos_bus::hal::HalPwm`).
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
      `fan.set_duty`, `fan.set_pwm`, and `fan.set_mode`. Replaces the app's
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
- [ ] Memory breakdown for a Linux refresh on the BCM2712 board, measured with and
      without eager binds. A consuming app measured ~6-7 MiB PSS for a refresh including
      camera probing; Lemnos's share is unknown. Measured without binds (`examples/bringup_memory.rs`,
      cross-built for aarch64 musl, run on a Raze): PSS 648 kB at start, 1344 kB after building the
      facade with the builtin drivers, 1696 kB after one refresh (RSS 1708 kB; refresh 1 ms).
      Eager binds are not measured on the live board: `RuntimeBindPolicy::all()` would bind the
      camera's I2C bus and the USB power GPIO lines.
- [x] The `tokio` facade feature stays (user, 2026-10-30).

## Release (do last)

- [ ] Merge `dev` into `main`.
- [x] Version: 1.0.0 (user, 2026-10-30; replaces the earlier 2.0.0 plan).
- [ ] Tag the release; optionally publish to crates.io.
- [ ] Consumers pin Lemnos to the tag instead of `branch = "main"`.

Consumer-side work (apps and the camera stack) is tracked in those repositories.
