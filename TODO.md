# TODO

Tracks Lemnos work driven by the apps that integrate it and by the camera stack that
consumes its foundation
([docs/foundation.md](docs/foundation.md)). See [CHANGELOG.md](CHANGELOG.md)
for the user-facing summary of finished work.

## Alpha (BCM2712 board)

The user, 2026-11-03: the next release is an alpha for the BCM2712 board hardware only. These come
first; the other boards' items below are still needed, after it.

- [ ] **RP1 IO blocks used in full on the BCM2712 board.** The RP1 part of "IO hardware blocks, used in
      full" below (PIO capacity and sharing, frame-sync and IMU timestamps on one clock, fan tach).
      Status 2026-11-02, from [docs/io-blocks.md](docs/io-blocks.md):
      - Done, host-tested: GPIO edge events are stamped on CLOCK_BOOTTIME in microseconds, the
        clock of the readings (1e0c504, `GpioEdgeEvent::observed_at_us`). Written, untested on board.
      - Done, host-tested: the PIO slot map. The ws2812 program (0..3) and the I2C master
        (4..30) fill 31 of 32 slots on `/dev/pio0`; the test `resident_programs_fit_the_32_slot_block`
        and `first_fit` in `lemnos-linux-sys` check it (cd0a111). One slot stays free.
      - Open, needs board wiring (the board file is in the consuming app's `devices/`, not this
        repo): the BMI088 INT1 line and the camera strobe as edge streams, so every IMU sample and
        frame has an acquisition stamp on the boot clock.
      - Blocked, fan tach: the board definitions here name no tach line, and the kernel's PWM
        counter mode is not reachable from sysfs. A PIO counter needs two slots, and only one is free.
        Next: confirm the tach wiring and `ls /dev/pio*` on the board; a second block or a program
        swap decides the rest.
- [ ] **The new USB layer on a real device.** ae792df and 3c0909f are host-tested only: caller-driven
      reaping, `next_block_until`, `driver()`/`speed()`, a mapped buffer on a real endpoint, through
      the consuming product's userspace UVC on the BCM2712 board.
- [ ] **lemnosd on the BCM2712 board under the consuming product's load.** The IMU batching test is load-sensitive on the
      host (17.9 ms gap once); measure the batched IMU timestamps and wakeup p99 on the BCM2712 board.

## Next release (1.0.0)

The release list: the 2026-10-27 feature freeze plus the hardware-support work added on 2026-10-30
(the last addition). Everything else stays under the headings below. Every item: what, why, done
when, blocked by.

### 1. Freeze items still open

- [x] **IMU above 100 Hz.** Done on the current board: the BMI088 is on the RP1 PIO I2C bus
      (`pio-i2c:sda=8,scl=7`, 068427e), not i2c-gpio. Measured on the BCM2712 board #1 (2026-10-31) with
      `lemnos-ctl watch imu --period <poll>` streaming at the full rate, lemnosd CPU over 30 s:
      100 Hz 1.9 %, 200 Hz (`200hz` / `200hz-23`, poll 5 ms) 2.9 %, 400 Hz (`400hz` / `400hz-47`,
      poll 2 ms) 6.0 % of a core, every sample delivered. The board's default rate is the device
      package's choice (the consuming app's devices/<board>/board.toml).
- [x] **The tokio facade decision.** The user keeps the `tokio` facade feature (2026-10-30).
- [x] BCM2712 bring-up and timing (a5496ee), the size baseline, the surface trim and the API dedup
      (1ed6fdd, f5754ec, 66a354d, b87f76c).

### 2. Hardware support (added 2026-10-30)

- [x] **riscv64 and armv7 builds.** Why: lemnosd runs on the K230D (riscv64) and Flame (RV1106G2,
      Cortex-A7). Done in 5f54be0 (CI job `cross32`, `./scripts/ci.sh cross32`): lemnosd and
      lemnos-ctl (the client, over lemnos-ipc) build with the service profile for
      riscv64gc-unknown-linux-gnu and armv7-unknown-linux-gnueabihf, and are checked stripped;
      lemnosd --help runs under qemu-riscv64. Stripped sizes: riscv64gc lemnosd 1,195,920 B,
      lemnos-ctl 902,592 B; armv7 lemnosd 1,221,944 B, lemnos-ctl 914,600 B. Checked locally with
      zig cc against glibc 2.39 and a Debian trixie sysroot; the GitHub job is the first run on
      ubuntu-latest. Blocked by: nothing.
- [ ] **Board definitions for the new boards.** Why: each board's LEDs, fan, power monitor, IMU and
      spare I/O come from its board file. Done when: board files for the K230D board, the Rockchip
      modules (RK3588S, RK3576, RK3566 and RK3562) and Flame are written, marked untested on board,
      and then load in lemnosd on the real board with their devices bound and the LED ring and fan
      driven. Blocked by: the boards' pinouts (PCB) for the files; boards only for the test.
- [ ] **Drivers those boards need.** Why: any LED, fan, sensor or power chip on the new boards that
      Lemnos doesn't drive yet. Done when: each has a driver written from its datasheet, marked
      untested on board, and then tested on its board. Blocked by: the boards' BOMs (PCB) for the
      list; boards only for the test.
- [ ] **Memory on small boards.** Why: the K230D has 128 MB in total. Done when: lemnosd PSS is
      measured on the K230D and Flame boards (1.6 MB RSS on the BCM2712 board today). Blocked by: boards.
- [ ] **IO hardware blocks, used in full.** Why: the user's rule after the PIO fix (2026-10-31):
      every board IO block is used for everything it can do, not one feature. Today the RP1 PIO runs
      ws2812 (offsets 0..3) and I2C (27 instructions), 31 of its 32 instruction slots. Done when
      docs list per board each IO block and each capability as used / unused / impossible, with
      the software path it would replace:
      - RP1: PIO (capacity: free slots, a second PIO image swap, which programs share), the
        hardware PWMs, RP1 DMA, GPIO edge timestamps;
      - candidates: camera frame-sync and IMU sample timestamps on one clock, the fan tach, any
        remaining bit-banged line;
      - K230D, Rockchip and the RV1106's on-chip RISC-V MCU core: their IO and timer blocks.
      Status (2026-11-02): the RP1 part is in docs/io-blocks.md (the capability table and plan);
      its open items are the Alpha item above. Blocked by: nothing for the RP1; the boards for the rest.

### 3. Release mechanics

For when the user calls the release; only the user decides when. Version target 1.0.0 (agreed with
the user on 2026-10-30; the workspace says 2.0.1-dev today, from the dropped 2.0.0 plan, and is set
to 1.0.0 in the release commit). See "Release (do last)" below: merge `dev` into `main`, tag 1.0.0,
and have consumers (the consuming apps, the Gaia `lemnos` source) pin the tag. Gates on the release
commit: fmt, clippy -D warnings, tests, and the size images.

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
- [x] `lemnosd` on the BCM2712 board (board #1, a consuming app's dev.800, 2026-10-25): ring `wire = "rgb"`
      (GRB24) matches the manifest (`grb24`, `rgbw: false`); the fan has no `restore_mode` and hands
      back through its cooling device; lemnosd is 1240648 bytes, PSS 1588 kB, 0.43 % of a core idle.
      The W channel isn't reachable (colours are 24-bit) and the ring has none to verify.
- [ ] Composite devices and the fan controller: design only
      ([docs/composite-devices.md](docs/composite-devices.md)); not built until the user asks.
- [ ] IIO buffered reads (`/dev/iio:deviceN`, IRQ timestamps) in the generic kernel
      binding.
- [x] BMI088 FIFO batch reads (opt-in, `fifo = true`): `lemnos-drivers-bmi088::fifo`, one
      batch per drain, samples stamped from the output rate. Measured on i2c-gpio with mismatched
      rates (100 Hz accel, 2 kHz gyro): 87 % of a core vs 36 % polled.
- [ ] Re-measure FIFO reads on the PIO bus with matched rates (200 Hz accel and `200hz-23` gyro,
      then 400 Hz): the earlier loss came from i2c-gpio and the 2 kHz gyro, neither of which holds
      now. Done when: CPU and sample timing, FIFO vs polled, are recorded here. Blocked by: nothing.
- [x] Board: the IMU off i2c-gpio and above 100 Hz: the RP1 PIO I2C bus (068427e) and the
      device package 20cd0c4 (200 Hz). Board #1, full-rate subscriber, lemnosd CPU: 100 Hz 1.9 %, 200 Hz 2.9 %,
      400 Hz 6.0 % of a core (2026-10-31).
- [ ] Measure the scheduler on the BCM2712 board (`tests/schedule.rs` runs on the host with a mock
      bus): the IMU at 100 Hz beside the real power monitor and magnetometer on the board's
      buses, with the reads' real durations. Partly measured on board #1: 6201 IMU readings in
      about 62 s with the real service polling the power monitor and magnetometer; spacing mean
      10000 us, deviation from the 10 ms grid at most 251 us (client-side, from the readings'
      timestamps). Read durations are not exposed (`read_cost_us` stays inside lemnosd), so
      they still need instrumentation.
- [ ] **A mock FIFO that drains.** Why: the mock I2C returns the same register bytes on every
      read, so the batched-IMU test in `crates/lemnosd/tests/schedule.rs` sees a window replayed
      when two drains land within one window (17.5 ms). That test therefore allows a step back of
      half a sample, and bounds gaps by the host's wakeup jitter. Done when: a mock FIFO removes
      what a read takes and the test asserts strict stamp order with a fixed gap bound. Blocked
      by: nothing (a `lemnos-hal` mock change).
- [ ] Phase 5: rich-model slimming (`Arc` in events, optional retention/diagnostics,
      shared probe code).

## Foundation

### Done

- [x] **DMA buffer basics as library code** (dbc5df4, c38ef59): dma-heap allocation, dma-buf
      CPU-access sync, RAII mappings, names and sync-file fences in `lemnos-linux` (feature `dma`),
      over `lemnos-linux-sys`. Host-tested against the system heap; written, untested on board
      (CMA and vendor heaps, the board kernel's fences). `docs/dma.md`.
- [x] **A generic bus and DMA-engine transfer interface** (dbc5df4): `lemnos_bus::transfer::TransferEngine`
      (submit, poll, wait, cancel) over memory or dma-bufs; SPI (`SpidevEngine`) and I2C (`I2cEngine`)
      engines. Written, untested on board. USB bulk is done (`UsbBulkEngine`, feature `usb`, below).
      Planned, not implemented: PCIe DMA, RP1 DMA. `docs/dma.md`.
- [x] **V4L2 memory-to-memory sessions** (5fa1379): `lemnos_linux::v4l2::m2m::M2mSession` (feature
      `v4l2-m2m`): formats, MMAP and dma-buf queues, streaming, poll, drains, events. Host-tested against
      vim2m and vicodec; written, untested on board (hardware 2D and codec devices). `docs/m2m.md`.

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

- [ ] Memory breakdown for a Linux refresh on the BCM2712 board, measured with and
      without eager binds. A consuming app measured ~6-7 MiB PSS for a refresh including
      camera probing; Lemnos's share is unknown. Measured without binds (`examples/bringup_memory.rs`,
      cross-built for aarch64 musl, run on board #1): PSS 648 kB at start, 1344 kB after building the
      facade with the builtin drivers, 1696 kB after one refresh (RSS 1708 kB; refresh 1 ms).
      Eager binds are not measured on the live board: `RuntimeBindPolicy::all()` would bind the
      camera's I2C bus and the USB power GPIO lines.
- [x] The `tokio` facade feature stays (user, 2026-10-30).

## Release (do last)

- [ ] Merge `dev` into `main`.
- [x] Version: 1.0.0 (user, 2026-10-30; replaces the earlier 2.0.0 plan).
- [ ] Tag the release; optionally publish to crates.io (order: hal, linux-sys, core, ...).
- [ ] Consumers pin Lemnos to the tag instead of a `dev` revision.

Consumer-side work (apps and the camera stack) is tracked in those repositories.

## After the release (planned 2026-11-02; not started; the user's go first)

- [x] **One USB transport** (780588c, 3e5fd0f, c39ba67, 811b9c1): `lemnos-usb` is the one USB
      layer. Linux runs on usbdevfs (URBs and ioctls in `lemnos-linux-sys`, the `usbfs` module);
      Windows (WinUSB) and macOS (IOKit) run on nusb 0.2.9, which no consumer imports. Listing,
      stable location (bus id and port chain), hotplug watch, descriptors and endpoint lookup,
      claims (detach on Linux), vendor and standard control, blocking and async bulk and interrupt
      transfers, buffered bulk streams, isochronous transfers on Linux, error kinds, and WinUSB
      reporting on Windows. libusb is gone from the lock (`rusb` removed). Migration notes for the
      flashing tool (from nusb) and the camera stack (from raw usbdevfs): `docs/usb.md`.
      *Gates:* workspace tests and clippy (all features), docs-and-lints, no_std, sizes, and the
      `lemnos linux` matrix entries pass; `cargo check` and clippy pass for Windows and macOS.
      *Untested on hardware:* every Linux runtime path (descriptor reads, claims, transfers,
      URB completion, isochronous streams, reattach after detach) and every Windows and macOS
      runtime path. This host's devices allow no open for this user; the probe covered the
      permission path only. *Follow-up, now done:* USB bulk is a `TransferEngine`
      (`lemnos_linux::transfer::UsbBulkEngine`, feature `usb`). The USB watch on the single hotplug
      watcher and the USB buffers are done below (untested on a device).
- [x] **One hotplug watcher.** Why: the camera stack needs the same device-arrival events.
      Done when: a single watcher serves Lemnos and the camera stack. (8669d26:
      `lemnos_linux::device_watch::DeviceWatcher`, netlink, inotify or rescan; Lemnos's
      `LinuxHotplugWatcher` is an adapter over it; camera-stack migration notes in docs/hotplug.md.)
- [ ] **Accelerator transports.** Why: accelerators sit on PCIe/MMIO and USB. Done when: PCIe and
      MMIO access works through vfio, and USB devices can load firmware and re-enumerate.
- [x] **Scope line.** Lemnos is user-space hardware access. It is not a memory manager, does not
      decide what data moves where, and has no camera or vision code. (8669d26: README, Scope.)
      Open overlaps, not refactored: the camera focus VCM drivers (`lemnos-drivers-vcm`) and their
      board-registry naming; `lemnos-light` (LED rendering) and `lemnos-fusion` (IMU fusion
      algorithms), which compute on data rather than access hardware; `lemnosd`'s frame fan-out
      policy (`clients.rs`, one shared `Rc` per reading); look names with a product prefix in README.md
      and docs/looks.md.

## Layer follow-ups (2026-11-02)

Done, on dev:

- [x] **M2M crop and device query** (6cdc68b): `QueueSetup::crop` (`S_SELECTION`, crop target,
      after `S_FMT` and before `REQBUFS`), `output_crop()` / `capture_crop()`,
      `M2mSession::set_output_crop`, `lemnos_linux::v4l2::query(path)` (read-only `QUERYCAP`) and
      `find_m2m(driver)`. Host-tested only as far as vim2m allows: this kernel's vim2m has no
      selection operation (`ENOTTY`), so the crop's success path is untested here. `docs/m2m.md`.
- [x] **USB for flashing tools** (d4d333a): `UsbDevice::device_descriptor()` (no request: the
      blob's first 18 bytes on Linux, nusb's cached copy elsewhere), `usb_version()` and
      `device_version()` on the listing, zero-length bulk OUT, `BulkWriter::flush_end()` (the ZLP
      on a packet boundary), `submit_bulk_out_zlp` (`URB_ZERO_PACKET`, Linux), and the control
      futures `submit_control_in` / `submit_control_out`. Untested on hardware. `docs/usb.md`.
- [x] **DMA borrowed forms** (92d8047): `DmaBufRef`, `DmaBuf::borrow()`, `wait_fence` and
      `fence_is_signaled` over a borrowed descriptor, and a size query that leaves the caller's
      offset where it was (no dup). `docs/dma.md`.
- [x] **USB bulk as a transfer engine** (`lemnos-linux::transfer::UsbBulkEngine`, feature `usb`):
      asynchronous, the ticket holds the URB; the caller's buffers are copied at submit and at
      settle, so a dropped ticket never writes the caller's memory. Host-tested on the settle
      logic and the error mapping. Untested on hardware. `docs/dma.md`.
- [x] **Product names out of Lemnos** (8d559f2): the built-in looks named for one consumer are
      generic (`detect.*`, `link.*`; CHANGELOG has the mapping), the example board is `sample-board`,
      and the checker self-tests known leaks against its allowlist. Left for the user (allowlisted):
      the lemnosd test fixture copied from a device package, the systemd unit's `Before=` names, and
      the built-in look-writer client-name policy.
- [x] **Flaky PWM test** (b6884fd): the sysfs harness published a channel before its
      attributes existed; it now renames a staged channel into place. Forced window: 5 of 5 failed;
      fixed: 200 of 200 under load.
- [x] **Scope overlaps note** (`docs/scope-overlaps.md`): `lemnos-light`, `lemnos-fusion` and the
      client queue in `lemnosd`. Options and a recommendation; nothing moved.

Done (2026-11-02, the follow-up above):

- [x] **USB watch on the single hotplug watcher (Linux).** The watcher core is the crate
      `lemnos-uevent` (below `lemnos-linux` and `lemnos-usb`, which both use it), and `lemnos-usb`'s
      `watch()` on Linux is a `DeviceWatcher` filtered to `usb` devices. nusb's watch stays for
      Windows and macOS. Untested on a device (host tests cover the devpath rule only).
- [x] **USB buffers from the DMA basics.** `UsbDevice::allocate_buffer` returns a usbdevfs mapping
      (`USBDEVFS_CAP_MMAP`, page-aligned, used by the host controller directly) and falls back to
      a heap vector when the kernel has no mapping or the mapping fails. Streams allocate their
      slots once and recycle them; `IsoReader` reuses its buffers and packet lengths. **Untested on
      a device:** written from the usbdevfs interface, never run against hardware, and not opened on
      this host. Host tests cover the page rounding, the heap buffer and the packet lengths. The
      completion path still builds one packet table per isochronous transfer.
