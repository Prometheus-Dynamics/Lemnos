# TODO

Tracks Lemnos work driven by the apps that integrate it and by the camera stack that consumes its
foundation ([docs/foundation.md](docs/foundation.md)). See [CHANGELOG.md](CHANGELOG.md) for the
user-facing summary of finished work. The version stays on the 2.0 line; releases are the user's
call.

Boards: two BCM2712 boards, #1 and #2, each with a USB webcam and a Coral USB accelerator.

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
- [x] **22. RP1 DMA engine** (060d755, feature `rp1-dma`, off by default): a `TransferEngine` on
      the DesignWare AXI DMAC, channels 3-7 only (0-2 are the kernel's PIO). #1 read-only: COMPVER
      0x3130332a, enabled, channels 3-7 idle. No transfer run. PCIe DMA: skipped (no reachable
      device; `TransferEngine` is the shape).
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

### Left, with blockers

- [ ] 21: a pending sync-file fence on hardware: needs a v3d job with an out-syncobj.
- [ ] 21: SPI transfer engine with data: no known-safe SPI device on either board.
- [ ] 21: a full stateless HEVC decode on rpi-hevc-dec: needs an HEVC header parser for the
      SPS/PPS/slice controls.
- [ ] 21: USB bulk with a payload on the Coral: needs a known-safe command for its protocol.
- [ ] 22: an RP1 DMA transfer on a board: needs verified RP1-view bus addresses for a dma-heap
      buffer.
- [ ] 23: M2M crop success path: no device on either board supports `VIDIOC_S_SELECTION`.
- [ ] 16: the BMI088 in the validator: the validator must open the PIO I2C bus.
- [ ] 17: `finish_refresh`'s work (now `prepare_refresh_commit` ~10 KB and
      `rebind_tracked_devices` ~12 KB) and the `String`-keyed descriptor maps: the latter are
      public `lemnos-core` types (an API break).
- [ ] 8: rows marked "TRM, not checked" in `docs/io-blocks.md`, and the RV1106 beyond its MCU
      core: need the TRMs and datasheets.
- [ ] 15: not built: the `Composite` trait, a listing in `lemnos-ctl list`, persisted tuning, live
      curve controls, the watchdog hook.

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
- RP1 DMA: channels 3-7 only; `cancel` waits for completion (the abort register is not wired).
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

## Open (carried)

- [ ] `embedded_hal_async::digital::Wait` on GPIO lines (`AsyncGpioLine`): needs a hardware check.
- [ ] Accelerator transports over PCIe and MMIO (vfio): not in this round's scope (USB only).

## Done earlier

The compact model (phases 1-4: `lemnos-device`, `lemnos-lite`, the generic L3 adapter, board
definitions, `lemnosd`, `lemnos-ipc`, `lemnos-light`, `lemnos-drivers-ws2812`); the foundation
(`lemnos-hal`, `lemnos-linux-sys`, no_std sensor drivers, regulators and clocks, GPIO character
devices); lemnosd on the BCM2712 board; the IMU on the RP1 PIO I2C bus (100 Hz 1.9 %, 200 Hz 2.9 %,
400 Hz 6.0 % of a core on #1); riscv64 and armv7 builds; DMA buffers, the transfer interface and
V4L2 M2M sessions; one USB layer (`lemnos-usb`) and one hotplug watcher (`lemnos-uevent`); the
`tokio` facade feature kept (user, 2026-10-30). Details are in [CHANGELOG.md](CHANGELOG.md).

Consumer-side work (apps and the camera stack) is tracked in those repositories.
