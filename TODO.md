# TODO

Tracks Lemnos work driven by the apps that integrate it and by the camera stack that consumes its
foundation ([docs/foundation.md](docs/foundation.md)). See [CHANGELOG.md](CHANGELOG.md) for the
user-facing summary of finished work. The version stays on the 2.0 line; releases are the user's
call.

Boards: two BCM2712 boards, #1 and #2, each with a USB webcam and a Coral USB accelerator.

## Decided, starts when the user says

Everything planned, in order. Execution is paused until the user starts it.

- [ ] **1. `lemnos-abi`.** Generation-counted handles, status codes, a `catch_unwind` macro, recycled
      slabs, buffer and fence descriptors, a version handshake. Every dependency uses it instead of
      inventing its own.
- [ ] **2. C surface and client wrapper.** Lemnos's small versioned C surface and a static Rust
      client wrapper: the first core, surface and wrapper set.
- [ ] **3. `lemnos-ipc` written spec.**
- [ ] **4. Single owner of hardware blocks.** Resume branch `lemnos-r5-v4l2` (df1dc81, local only,
      worktree `/mnt/nvme-wd/lemnos-r5-v4l2`): V4L2, subdev and media, then RGA, VGLite, PiSP, and
      the K230 and rkisp device layers. `crates/rga` and `crates/vglite` come over from the graph
      runtime, and the generic parts of the camera stack's `crates/kernel` move here.
- [ ] **5. Kernel module and Rust user-space drivers.** One small module of our own, its sources in
      Lemnos: claim a block, DMA buffers and device addresses, dma-buf export, interrupt forwarding,
      register mapping. The drivers are Rust in user space. It is a fast path, never a requirement:
      stock V4L2 stays as the fallback, and other boards, the simulator and other kernels stay on
      stock interfaces. No patches on other people's code; our own modules are fine.
  - [ ] The PiSP back end is the first block. First task: answer IOMMU coverage,
        interrupt-to-user-space latency, clocks, power and reset, and the GPL position of porting
        from stock drivers.
  - [ ] Then the receiver, the sensor over i2c-dev, and a mock backend for the simulator.
  - [ ] When the module lands, reword "Lemnos is user-space hardware access" in
        [docs/dma.md](docs/dma.md) and "no kernel modules" in
        [docs/system-service.md](docs/system-service.md). The latter argues against chip drivers as
        modules, which still holds, but its recommendation reads wider than that.
- [ ] **6. GPU**, per the agreed seam note (kept outside this repository, because it names the
      products). Mesa stays, and `lemnosd` coordinates rather than enforces: an advisory lease
      first, `lemnos-gpu` only when a consumer needs it.
- [ ] **7. Shared and static builds.** Both modes, with core, surface and wrapper for each
      dependency. The workspace's LTO test informs how shared gets built.
- [x] **8. `a_polled_400_hz_imu_delivers_each_sample_once` was intermittent.** Fixed: its rate floor
      is now 90 % of 400 for the time the service's threads had a CPU (their run-queue delay in
      `/proc/self/task/*/schedstat`), not a share set by a 10 ms wakeup probe's p99. Measured on two
      cores with 0-16 busy loops: a plain thread sleeping on the same 2.5 ms grid hit 99.8-100 % of
      it, but the service's loop thread waited 60-74 % of the time for a CPU (89 preemptions in
      2.5 s, about 54 us of work per wakeup in a debug build) and delivered 156-335 a second. 12 of
      12 runs pass at 0-16 busy loops; a 5 ms schedule (200 a second) still fails with no load
      (floor 358).
  - [ ] Open: under CPU contention lemnosd's loop is preempted where a lighter thread is not, so
        it loses samples. Not measured on a board or in a release build. Options: less work per
        wakeup, or a scheduling class for lemnosd in its unit.

## Round 4 (the user's list, 2026-11-03)

- [x] **Orion protocol 1** (351b7eb, 00aa534): the bridge pinned to Orion a89ebd5. Live on #1
      (2026-11-04, image dev.912): 9 devices published as `lemnos.device` resources, all healthy;
      `read` matches `lemnos-ctl`, `calibration.status`, `set` and `restore` succeed, a bad control
      is rejected.
- [x] **15. Composites** (6d66b53, 7aac6a3): the `Composite` trait in `lemnos-device`; the fan
      controller is a device slot listed by `lemnos-ctl list` with status, reason, channels and
      controls; client tuning persists in `<state>/fan/<id>.tuning`; the watchdog is pinged only
      while every enabled controller has stepped lately. #1 fan: listing, live tuning and
      persistence tested, then restored exactly.
- [x] **16. The BMI088 in the validator** (e28ae66): the bus targets take a `pio-i2c:sda=N,scl=N`
      selector (facade feature `rp1`). Passes on #1.
- [x] **17. String-keyed maps and probe sharing** (37065b5, 8c1cec0, 1e71ab7, ba46fb7, 153a405,
      ccafd32, 4b2d0e5, c7174aa): `SmallMap<MapKey, V>` (sorted vector, `Cow<'static, str>` keys)
      replaces the `BTreeMap`s in descriptors and runtime bookkeeping (an API break, serde shape
      unchanged); the probes share their descriptor builders. linux-lite 560 928 -> 511 664 B
      (-8.8 %), linux-lite-builtin 720 584 -> 662 032 B; lemnosd +17 KB from the composite work.
- [x] **21. Pending fence** (0735212, #2): 1024 TFU copy jobs (4 MiB), the out-syncobj's sync
      file reads pending straight after the submits and signals 4.9-5.0 ms later; all bytes match
      in five runs. The TFU `icfg` (32) and one-row-per-job shape came from a sweep.
- [x] **21. HEVC decode** (9f0c0d8, 0ce8289, 78222b0, #1 rpi-hevc-dec): `lemnos-hevc`, a no_std
      parser (IDR path only), and the stateless HEVC controls (`v4l2-hevc`). One 320x240 IDR
      decodes and matches the host VA-API decode byte for byte (3 runs); request run 181-192 us.
- [x] **22. Raw RP1 DMA dropped** (the user, 2026-11-03): the `rp1-dma` engine, its register
      layout and the planned PCIe DMA engine are removed. The ruling covers this memory-copy engine
      only.

## Round 3 (the user's ruling, 2026-11-03)

Every item below was ruled on by the user. Shas are on `dev`; "host" means this workstation.

- [x] **3. USB layer on a real device, both boards, against the webcam.** Listing, descriptors,
      claims, control (UVC probe, GET_LEN), interrupt, isochronous UVC streams in both reaping
      modes, usbdevfs mappings (`CAP_MMAP` on both), `next_block_until`, `driver()`/`speed()`,
      reattach. #1: 29.4 fps of 30, 0 packet errors; 60 s run 1800 frames, 473 664 packets, 0
      errors, ~1 % CPU, ~485 xHCI IRQ/s. #2: 24.5 fps of 25, 0 errors, monitored (throttled 0x0).
      Fixes: claim check before the kernel, release through `USBDEVFS_IOCTL`, usbfs memory cap with
      heap fallback, the reaper fails a hung-up descriptor instead of spinning (7a96740..a4512c9,
      f41ac9c..92e21a7). Reattach: a release does not rebind the UVC streaming interface; rebinding
      uvcvideo on the control interface does. `docs/usb.md`.
- [x] **4. lemnosd under real load** (#1): FIFO at 400 Hz, a full-rate subscriber, camera stack
      running: 23 872 of 24 000 readings, wakeup p50 55 / p99 79 / max 446 us, 1.72 % of a core;
      receive age p50 11.0 / p99 20.8 ms (batched by design) (399b41e).
- [x] **8. IO blocks as chip support** (untested on board): `lemnos-soc` (no_std data crate) with
      the K230D, RK3588S, RK3576, RK3566, RK3562 and RV1106 blocks marked used / unused / impossible,
      and register layouts for the Rockchip PWM v3, timer and the CLINT; per-chip tables in
      `docs/io-blocks.md` (6d839e4, 700348b). Source: mainline device trees and drivers.
- [x] **10. IMU FIFO vs polled on the PIO bus, matched rates** (#1), lemnosd CPU and timing:
      200 Hz polled 2.55 %, 0 gaps, age p50 0.56 ms; 200 Hz FIFO 1.40 %, 66 gaps, age p50 7.2 ms;
      400 Hz FIFO 1.72 %, 128 gaps, age p50 11.0 ms; 400 Hz polled: see the clamp below.
      FIFO trades latency and gaps for CPU (a 48-frame drain takes ~16 ms on the PIO bus).
- [x] **11. Scheduler with real buses** (#1): per-slot read cost over IPC (`Request::Stats`,
      `lemnos-ctl stats`, 191c646). IMU 100 Hz beside the INA238 and BMM150: 6001 readings, grid
      deviation mean 16.9 / p99 118 / max 614 us, 1.62 % of a core; read means IMU 563 us,
      magnetometer 1082 us, power monitor 2543 us.
- [x] **12. A mock FIFO that drains** (bd9543d): `lemnos-hal` mock `with_fifo` /
      `with_fifo_stream`; the batched-IMU test asserts strict stamp order and a 25 ms gap bound.
- [x] **13. IIO buffered reads** (2e79517, untested on board: neither board has IIO):
      `IioBuffer` (`KernelDevice::buffered`), scan elements, trigger, the timestamp channel on
      CLOCK_BOOTTIME, poll-based reads; host-tested on a fake sysfs tree.
- [x] **14. Phase 5 rich-model slimming:** `Arc` in inventory events (cacdb6c), retained-event
      byte accounting behind `retention-bytes` (9e22a4f), a shared absent-root probe helper
      (05d85e1). With item 17: linux-lite 574 480 -> 562 704 B, linux-lite-builtin -11 744 B,
      facade -1 792 B.
- [x] **15. Composite devices: the fan controller** (148bcd1, 3c76a2c, 50533a8, slimmed in 77089e3):
      `lemnos_device::fan_control` (no_std), a `[[composites]]` board entry, hosted by lemnosd.
      #1 real fan: two curves settled at pwm1 111 and 189; restored exactly (pwm1 179,
      pwm1_enable 1, cooling state 0). Cost: lemnosd ~+16 KB, lemnos-ctl ~+9.8 KB.
      `docs/composite-devices.md`.
- [x] **16. Board validator on the sensor crates** (0e00029, 1e04b49, 1ad7a6e): #2 BMM150 and
      INA238 pass (4.253 V, 2.953 W, 38.6 °C). The BMI088 is skipped: the validator opens `i2c-N`
      only and the IMU is on the PIO bus.
- [x] **17. Slimmer small-Linux runtime:** device-keyed runtime state in a sorted vector
      (baa0076); the nightly `-Z build-std` recipe documented (f30d2fd, `docs/compact-model.md`,
      *Constrained images*): empty main 278 856 -> 12 232 B, sensors 319 056 -> 43 712 B,
      linux-lite 562 704 -> 232 608 B.
- [x] **18. `SetDutyCycle` on sysfs PWM** (5341463): `lemnos_linux::hal::SysfsPwm`, feature `pwm`.
      Untested on board.
- [x] **19. Memory of a refresh with eager binds** (#1, `bringup_memory`, cd15641): PSS start
      616 kB, facade 1212 kB, refresh without binds ~1892 kB, with eager binds 2136 kB (+248 kB;
      36 tried, 20 bound). Refresh 2 ms without binds, 110 ms with. The camera's I2C bus
      (`linux.i2c.bus.10`) is excluded; the USB power lines are not discovered without the board
      file.
- [x] **20. Accelerator transport, USB only:** a generic DFU loader (`UsbDevice::dfu_download`,
      `wait_for_device`), blocks of the device's `wTransferSize`. Coral 1a6e:089a -> 18d1:9302 on
      both boards: 43 blocks of 256 B, download and reset 1.22 / 1.23 s, re-enumerated after
      1.52 / 1.53 s. The first attempt on #1 used 4096-byte blocks and wedged the device until a
      port power cycle.
- [x] **21. USB and DMA on hardware.** Coral bulk endpoints after the load (both boards):
      `UsbBulkEngine` claim, zero-length OUT 56-147 us, IN timeouts cancel cleanly. DMA heaps on #2
      (1 MiB: system 60 us, CMA 162-165 us; CMA pool 64 MiB), now capped before the kernel
      (019d090). Fences: a signalled v3d syncobj exports and imports. I2C engine: BMM150 chip id in
      435 us (#2). SPI: zero-length only. M2M: stateless request API (bb4eb7e, e24e50a), host
      decode through vicodec; on #1 rpi-hevc-dec takes a request, a control and an OUTPUT buffer.
- [x] **24. Isochronous packet tables reused:** the pool was already in c546b44; a host test now
      proves steady-state reuse.
- [x] **26. Product names out of Lemnos** (569ad29, 47b3b07): look writers are a board-file
      `look_writers` list (default `lemnos-ctl`, `orion:*`); the lemnosd fixture is a generic
      `sample-board.toml`; the unit's ordering is a drop-in example. Their allowlist entries are
      gone.
- [x] **Scope overlaps decided** (user, 2026-11-03; `docs/scope-overlaps.md`):
      1B, the named looks move out (c87819a, a1414a7, 97d9d03, ef4ef15, 7fdfb1d): built-ins are
      `status.*` only, the rest are example look files and presets under `examples/looks/`, boards
      get `default_look`, and the 21 retired ws2812 look keys are refused with an error naming
      where each now lives. 2A: `lemnos-fusion` stays. 3A: the client queue stays in lemnosd; its
      drop classes are documented in `lemnos-ipc`.
- [x] **Polled IMU never faster than the device, never a repeated sample** (found in item 10:
      a 2 ms poll read a 400 Hz device at 500 Hz; e9ab879): lemnosd clamps a polled
      sensor to its output period (from the board config) and skips a reading equal to the last
      one within that period. #1 at 400 Hz: 399.90 Hz, 6 gaps, grid p99 64 us, 4.41 % of a core,
      receive age p50 559 us. Firmware images unchanged.

### Decisions (made during the round)

- The validator drops the BMI055 (no crate support, no board carries one).
- INA238 range: 16.38 A at 10 mOhm (16.384 A is exactly the 163.84 mV limit and is refused).
- Failure records stay on (they feed `DeviceStatus::Faulted`); `failures()` stays a `BTreeMap`.
- `Arc` in `InventoryEvent`, `InventoryDiff` and `ChangedDevice` is an API break; the serde shape
  is unchanged.
- `retention-bytes` is on by default; with it off only the event-count limit applies.
- The fan controller is a concrete type in a `[[composites]]` entry that reuses the device table
  shape (roles in `config`); it is not a device and is not listed by `lemnos-ctl`.
- lemnosd hands a fan back after a failed write and waits 10 s.
- Stats are counted per read on the slot and read on request (three integer updates per read).
- The mock FIFO fills on the device clock, not from a feeder thread.
- IIO: boottime clock (monotonic converted per record, others refused); a device without a
  timestamp element is refused; a running buffer is `Busy`.
- DMA heaps: CMA requests are capped by `CmaFree`, system by a quarter of `MemAvailable`;
  `DmaHeap::with_capacity` sets a lower cap.
- USB: transfers on an unclaimed interface are refused before the kernel; driver rebind after a
  release goes through sysfs, for UVC through the control interface.
- USB port power (the user, 2026-11-03): a wedged USB device is recovered by cycling its port's
  `gpio-power-switch` through lemnosd. Both boards: `usb-c-power` (RP1 GPIO16) feeds the Coral
  (bus 1 port 1), `usb-a-power` (RP1 GPIO20) the webcam (bus 3 port 1). Cycles this round: #1
  usb-c once (Coral recovery) and usb-a once (mapping); #2 each once (mapping).
- Polled sensors: repeats are filtered by value within one output period (a still device is
  still delivered once per period). A sensor-clock index (BMI088 SENSORTIME) was tried and
  dropped: its rate did not match the samples, and measuring it needs the IMU bus free of
  lemnosd.
- Looks: the retired ws2812 look keys are refused, not ignored (the user: nothing unreleased gets
  compat). A light with no owner, or a look name nothing defines, shows `default_look`.
- `finish_refresh` stays at about 15 KB: split into its steps it showed no duplicated code.
- HEVC: frame-based decode mode (the only one the driver takes); the driver's NV12 is in 128-byte
  columns; `bit_size` covers the slice data from `data_byte_offset`, not the whole buffer.

## Done earlier

The compact model (phases 1-4: `lemnos-device`, `lemnos-lite`, the generic L3 adapter, board
definitions, `lemnosd`, `lemnos-ipc`, `lemnos-light`, `lemnos-drivers-ws2812`); the foundation
(`lemnos-hal`, `lemnos-linux-sys`, no_std sensor drivers, regulators and clocks, GPIO character
devices); lemnosd on the BCM2712 board; the IMU on the RP1 PIO I2C bus (100 Hz 1.9 %, 200 Hz 2.9 %,
400 Hz 6.0 % of a core on #1); riscv64 and armv7 builds; DMA buffers, the transfer interface and
V4L2 M2M sessions; `AsyncGpioLine` (host-tested, untested on board: no line gives real edges
without rewiring); one USB layer (`lemnos-usb`) and one hotplug watcher (`lemnos-uevent`); the
`tokio` facade feature kept (user, 2026-10-30). Details are in [CHANGELOG.md](CHANGELOG.md).

Consumer-side work (apps and the camera stack) is tracked in those repositories.
