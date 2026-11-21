# Test doubles: what each stands for, and its real-failing case

A test double is a stand-in that a test uses where the real object would be needed. The rule the
audit checks is the one in the test-double rule: a double must fail where the real thing fails,
or the test proves nothing about the real thing. Each row names the real object, and the case
that fails the way the real object does. A row with no such case says why none is possible on the
PC, or it is marked as not assessed.

Status words:

- **real-failing case**: a test that the double passes and the real object refuses, with the test
  name and file.
- **real-object test**: the test runs against the real object (skipped by name when it is absent).
- **not built**: the case is named here but not written in this pass.
- **not assessed**: the double is listed, but its real-failing case has not been walked yet.

## The families the audit names

| Double | Where it is used | Real object | Real-failing case | Status |
|---|---|---|---|---|
| Sealed memfd as a dma-buf | 82 test files (`sealed_memfd`, `memfd_create`), e.g. `drivers/g2d/tests/stream.rs`, `drivers/pisp/tests/be_frames.rs` | A dma-buf from `/dev/udmabuf` (kernel) | `backends/linux-sys/src/udmabuf.rs`, `udmabuf_refuses_an_unsealed_memfd_and_takes_a_sealed_one`: the real device refuses an unsealed memfd with `EINVAL` and takes the same memfd once sealed. Skips by name, printed as `SKIPPED`, when `/dev/udmabuf` cannot be opened. | real-object test |
| `UnixStream::pair` as FunctionFS endpoint files | `drivers/usb-gadget/src/tests.rs` (`lending_rig`, line 1085), `drivers/usb-gadget/src/serve_tests.rs` (line 282) | FunctionFS endpoint files on `dummy_hcd` (kernel gadget) | The real-gadget cases in `testing/crates/gadget-check/src/holder.rs`, run in QEMU on `dummy_hcd` by the gadget stanza: 1 (a) the read in flight at a disconnect fails `ESHUTDOWN`; 1 (b) the queued AIO read at a disconnect fails `ESHUTDOWN`; 2b (a) the read in flight at a bus reset fails `ESHUTDOWN`; 4 (a) the read in flight at the UDC unbind fails `ESHUTDOWN`; 4 (b) the queued AIO read at the unbind fails `ESHUTDOWN`; 5 (b) the queued AIO read at the switch-off fails `ESHUTDOWN`. A socketpair cannot produce `ESHUTDOWN`: a peer's close reads as EOF, so the pair does not reproduce these cases. These cases are the evidence; no new case is added. | real-failing evidence exists (the gadget stanza; its QEMU run is recorded in the commit) |
| Regular file or in-memory image as a block device | `drivers/slot-store/src/bays_tests.rs` (line 121, in-memory image), `drivers/system-slots/src/offline_tests.rs` (line 491, in-memory copy of the boot record) | A block device node (`S_IFBLK`) | `drivers/system-flash/src/target.rs`, `a_block_target_must_be_a_block_device_and_a_missing_one_is_said_so`: a regular file is refused as a block target. `service/ctl/tests/boot_add.rs`, `a_block_device_a_directory_and_a_file_that_is_not_fat16_are_refused_and_nothing_is_written`: a block node is refused by its file type. | real-failing case exists for the refusal; the in-memory image is not refused by the block path and is not assessed for the write path |
| `engine/hal` mock blocks (`MockUsbDevice`, `MockUsbHost`, `MockUsbFunction`, `MockRegisters`, `MockDma`, and the rest of `mock_blocks.rs`) | Driver and backend tests across the tree | The Linux backend (`backends/linux`, `backends/linux-sys`) on the real kernel | `MockUsbDevice` isochronous errors pinned to the backend's errno mapping (`EINVAL` to `InvalidInput`, `ENODEV` to `Unavailable`), with a fail-before in commit `a88326c0` | real-failing case exists for the USB errors; the other mock blocks not assessed |
| `testing/crates/standin` (`StandIn`, `FakeSys`): a real `lemnosd` for a consumer's tests | Consumer tests through `lemnos-standin` | The daemon (`service/lemnosd`) | `testing/crates/standin/tests/consumer_shapes.rs` runs the consumer shapes against the real service | real-object test (the stand-in is the real daemon) |
| `camera-sim` `SimReceiver` (BUSY latch) and the Nack sensor | Chain proof tests | The receiver driver's refusal, and the sensor's bus NACK | `chain_latch_beside` pins the latch's refusal (`Status(-16)`, BUSY); `chain_sensor_nack` removes the sensor from its bus and pins the refusal by name. Both in `a88326c0`. | real-failing case exists |
| Sim clock `FileClock` | Chain proof tests | The clock file the proof reads | `chain_clock_refusals.rs`: a short clock file, and a clock that goes backwards, are refused by name (`c8e2115b`) | real-failing case exists |

## Verdicts by family

Each family gets a verdict: **yes** means a test makes the double fail the way the real object
does and asserts the real refusal. **no** names the missing case. Where a family is marked
"not judged", the agent did not read it deeply enough to decide, and it is open.

### Drivers (the families under `drivers/`)

| Family | Files | Real object | Verdict | Evidence, or the missing case |
|---|---|---|---|---|
| A733 DI engine Mock | drivers/a733-di/src/engine/tests.rs | A733 DI block | yes | EINVAL to Refused (tests.rs:158); EIO to Device with backoff 250 ms to 8 s (:184) |
| A733 video Mock | drivers/a733-video/src/block/tests.rs | A733 video block | yes | map EFAULT to Refused (:272); wait EIO to Device (:365) |
| Coral USB MockUsb | drivers/coral-loader/src/mock.rs | USB device | no | Kinds injected directly; missing: ENODEV on transfer and EBUSY on claim, mapped and asserted |
| Flash-stage clock SimClock and Fake | drivers/flash-stage/src/sim/model.rs, wait.rs | board clock | no | The clock cannot fail; missing: a stalled clock (zero step) on the deadline path |
| G2D Recording | drivers/g2d/src/convert/tests.rs | G2D block | no | Recording never fails; missing: an engine EINVAL asserted as a refusal at this layer |
| G2D engine and streams | drivers/g2d/tests, src/engine/tests.rs | G2D block | yes | EINVAL to Refused (engine/tests.rs:98); ETIMEDOUT and EIO to Device (:112, :144) |
| Hardware watchdog Fake | drivers/hw-watchdog/src/tests.rs | watchdog device | yes | Failed feeds retried on backoff (:251); no answer is faulted, not failed (:407); the real backend maps every failure to Failed (lib.rs:197-207) |
| LED pixels FakePixels | drivers/indicator/tests/support/mod.rs; drivers/status-light/tests/support/mod.rs | LED pixel output | no | Failing show returns TIMED_OUT; asserted only by retry gaps and Failed health; missing: the real pixel driver's failed-show status asserted |
| CSI node FakeMipi | drivers/k230-capture/src/module_tests.rs | MIPI receiver | no | Always opens; missing: an absent or refused CSI node (ENOENT, EACCES) refused at bind |
| DMA FakeDma | drivers/k230-capture-sys/src/tests.rs | DMA engine | no | allocate never fails; missing: an ENOMEM allocation refusal asserted |
| GSDMA Mock | drivers/k230-gsdma/src/tests.rs | GSDMA block | yes | Open errno 19 backs off (:582); submit errno 22 to Device (:678). Only the variant is asserted, not the errno |
| m2m decode Mock | drivers/m2m-decode/src/mock.rs | V4L2 decode node | yes | Queue EINVAL to Failed, session reopened after 250 ms (stateless_tests.rs:448) |
| Named files Fake | drivers/named-files/src/tests.rs | files on disk | no | Real temp directory, no failure injection; missing: a file removed or unreadable between index and read |
| 2D engine Recording | drivers/nonai2d/src/convert/tests.rs | 2D engine | no | Always Ok; missing: engine EINVAL asserted as a refusal at this layer |
| PiSP back end Mock | drivers/pisp/src/be_device/tests.rs | PiSP back end | no | Status injected directly; missing: errno 16 or 22 through be_hw.rs:169-172, asserted |
| PiSP front end Mock | drivers/pisp/src/fe_device/tests.rs | PiSP front end | no | Status injected directly; missing: errno 16 or 22 through fe_hw.rs:191-193, asserted |
| RK AIISP Mock | drivers/rkaiisp/src/tests.rs | AIISP block | yes | Queue fail InvalidInput to Refused (:569); open NotFound backs off to the 8 s cap (:577). Injected kinds, not errnos |
| RKCIF Mock | drivers/rkcif/src/tests/mod.rs | RKCIF capture block | no | Injects NO_DEVICE and IO; missing: a real errno (ENODEV, EIO) through the RKCIF map. Judged from targeted reads |
| RP1 CSI-2 Mock | drivers/rp1-csi2/src/tests.rs | RP1 CSI-2 receiver | no | Status injected; missing: real errno to Status for start and check, asserted. Judged from targeted reads |
| UBI volume Fake | drivers/slot-store/src/ubi.rs | UBI volume | no | Refusals are io::Error::other; missing: EBUSY, ENOSPC or EEXIST from make or rename, mapped and asserted |
| NAND FakeNand | drivers/system-flash/src/lib.rs | NAND partition | no | Never fails a write or erase; missing: ENOSPC asserted through a job |
| NAND Fake | drivers/system-flash/src/nand.rs | NAND chip | no | Errno-less errors; missing: an uncorrectable read (EBADMSG) asserted through a job |
| Partition grow Fake | drivers/system-slots/src/grow/tests.rs | partition grow (BLKPG) | yes | BLKPG EBUSY gives a refusal and the next pass converges (:913); ENOSPC (:932). Asserts text and convergence, not the kind |
| pstore FakePstore | drivers/system-slots/src/tests.rs | pstore region | no | Remove fails with a plain error; missing: unlink EPERM or EBUSY mapped and asserted |
| Mount table FakeMounts | drivers/usb-gadget/tests/configfs.rs | FunctionFS mount | no | Always Ok; missing: a refused mount (EBUSY or EPERM) or an unmount failure asserted |
| UVC USB MockUsb | drivers/uvc/src/tests/mock.rs | UVC USB device | no | Kinds injected; missing: unplug mapped from ENODEV to Unavailable, asserted |
| PIO LED Fake | drivers/ws2812/tests/pio.rs | PIO LED driver | no | Busy only; missing: a non-Busy transaction failure (timeout or ENODEV) from the PIO asserted |

### Engine and backends

| Family | Files | Real object | Verdict | Evidence, or the missing case |
|---|---|---|---|---|
| PIO I2C Mock | backends/linux/src/pio_i2c/bus.rs | PIO I2C bus | yes | NACK reported (bus.rs:672); ETIMEDOUT to Timeout, bus recovers (:702) |
| FakeWire | backends/linux/src/gadget/tests.rs | USB gadget wire | yes | Abandoned request to Unavailable (tests.rs:1157); errno table (:1330) |
| FakeUsbfs | backends/linux-sys/src/usbfs/fake.rs | usbfs device | yes | Detach EACCES comes back with its errno (usbfs/tests.rs:137); ENODEV on re-enumerate (:160) |
| SimBoard | backends/sim/src/board.rs | Linux i2c, spi, clock, stream nodes | no | Exclusive BUSY matches EBUSY; missing: an unplugged node asserted as NO_DEVICE (real mapping gives NO_DEVICE, sim gives NOT_FOUND) |
| SimClock | backends/sim/src/clock.rs | host clock | no | The clock is infallible; no failure path exists |
| SimHints | backends/sim/src/hints.rs | uevent hints | yes | Overflow reports Hint::Any (sim tests.rs:358), matching the real dropped events |
| SimI2c | backends/sim/src/i2c.rs | Linux i2c-dev | yes | Dead target gives Nack (sim tests.rs:35); EREMOTEIO and ENXIO map to Nack, pinned (linux i2c.rs:381, :391) |
| SimSpi | backends/sim/src/spi.rs | spidev | no | Never returns an error; missing: a spidev EIO or ENODEV mapped and asserted |
| SimFrameSource | backends/sim/src/stream.rs | kernel frame queue | no | Writers always Ok; missing: a write failing with an ErrorKind that produce() propagates |
| MockRegisters | engine/hal/src/mock_blocks.rs | register block | no | Only out-of-range InvalidInput; missing: a bus error (EIO) on read32 mapped and asserted |
| MockInterrupt | engine/hal/src/mock_blocks.rs | interrupt line | no | Never fails; missing: an enable refused by the kernel, mapped and asserted |
| MockDma | engine/hal/src/mock_blocks.rs | DMA buffer | no | Allocation never fails; missing: ENOMEM mapped and asserted |
| MockClocks | engine/hal/src/mock_blocks.rs | clock framework | no | Rate never refused; missing: EINVAL or EBUSY on set_rate_hz, mapped and asserted |
| MockReset | engine/hal/src/mock_blocks.rs | reset controller | no | Index only; missing: deassert EIO, mapped and asserted |
| MockUart | engine/hal/src/mock_blocks.rs | tty serial port | no | No failure; missing: a read timeout or framing error with its mapped kind |
| MockStream | engine/hal/src/mock_blocks.rs | dma-buf frame queue | no | InvalidInput and Unsupported asserted directly; missing: a kernel queue error through from_errno |
| MockUsbDevice | engine/hal/src/mock_blocks.rs | usbfs device | yes | Re-enumerate leaves the handle dead (mock_blocks.rs:2073); ENODEV to Unavailable and EINVAL to InvalidInput pinned (:2012) |
| MockUsbHost | engine/hal/src/mock_blocks.rs | usbfs enumerate and open | no | Unknown device gives NotFound; missing: an unplugged device's open as Unavailable (ENODEV or ENOENT), checked against the real open path |
| MockUsbFunction | engine/hal/src/mock_blocks.rs | FunctionFS wire | no | The mock recovers on re-enable; the real unbind stays unavailable (gadget/tests.rs:1319). Divergence: the mock must be made to stay dead after unbind |
| MockI2c | engine/hal/src/mock.rs | hal I2C | no | Dead target is_err() only; missing: assert ErrorKind::Nack, matching EREMOTEIO or ENXIO |
| MockI2cTarget, MockFifo, MockDelay | engine/hal/src/mock.rs | hal blocks | not judged | Not read in enough depth |
| MockLine | engine/hal/src/mock_raw.rs | gpio line chardev | yes | Busy asserted (gpio-power-switch tests.rs:73; package resources.rs:133); EBUSY to BUSY pinned (linux lib.rs:1110) |
| MockPwm | engine/hal/src/mock_raw.rs | PWM sysfs | no | Duty above period returns InvalidInput; missing: assert InvalidInput, matching sysfs EINVAL (test asserts is_err only) |
| MockRawSpi | engine/hal/src/mock_raw.rs | spidev raw | no | Successful transfers only; missing: a failed transfer (EIO) asserted with its kind |
| FakeHost | engine/client/tests/common/mod.rs | lemnosd client | no | Hand-written BUSY (-16); missing: the real daemon's subscribe refusal asserted as BUSY |
| Fake (core) | engine/core/src/tests.rs | core registry | no | Maps bus errors, untested; missing: a MockI2c Nack reaching dispatch and asserted as IO |
| Recording (guest) | engine/host/src/guest.rs | host guest SPI | no | Always Ok; missing: a device transaction error surfacing through the guest SPI with its status |
| Standing | engine/host-static/src/slot.rs | slot store | yes | Five failing calls as gone devices give IO and NO_DEVICE (host-static/tests/rebind.rs:300-329) |
| Standins (lend) | engine/host/tests/stream_lend.rs | stream lend | no | Allocators never fail; missing: lend of a non-dma-buf memfd refused, asserted |
| Standins (release) | engine/host/tests/stream_release.rs | stream release | no | Allocators never fail; missing: allocate_planes error propagated and asserted |
| StandIn (C API) | engine/capi/tests/common/mod.rs | C API buffer source | no | Never fails; missing: BufferSource::allocate returning ENOMEM asserted as the C status |
| Fixture (package) | engine/package/tests/common/mod.rs; fixture/src/lib.rs | package driver containment | yes | A panic in bind is contained and retried (package.rs:287-317); matches built-in containment, not an errno |
| Fixture (USB backend) | backends/usb/src/tests.rs | USB backend over usbfs | yes | EACCES to PermissionDenied and ENODEV to Unavailable (tests.rs:604); kind table (:700-716) |

### Services, contracts and testing crates

| Family | Files | Real object | Verdict | Evidence, or the missing case |
|---|---|---|---|---|
| Camera contract Mocks | contracts/camera-frames, camera-isp, camera-receiver, camera-sensor, tests/contract.rs | camera drivers | no | BUSY and NOT_FOUND chosen by the Mock; missing: kernel EBUSY on start or select through the real driver's map (rkcif lib.rs:245) |
| Image and ISP contract Mocks | contracts/image-blend, image-convert, image-scaler, isp-isp600, isp-pisp | image and ISP drivers | no | Failing jobs chosen by the Mock; missing: a hardware job failure with its real status (pisp be_hw.rs:334 BUSY) through the same driver path |
| NPU SimNpu | contracts/npu/tests/contract.rs | NPU block | not judged | run and run_one read; other tests in the file not read |
| lemnosd StandIn | service/lemnosd/tests/common/mod.rs | lemnosd buffer allocator | no | fail hook never set by a test; missing: a buffer allocation refused with the real errno, asserted by a client call |
| Supervisor Fake | service/lemnosd/src/supervisor.rs | forked helper | no | Raw errno 11; missing: fork failing with ENOMEM, mapping asserted |
| Peer named-file Fake | the peer service tests, named_files.rs | peer named-file node | no | Free-string refusals; missing: node open refused with NOT_FOUND reaching the client
| Peer named-offer Fake and FakeAsk | the peer service tests, named_offer.rs | named-file offers | no | Hand-picked NOT_FOUND; missing: the real node-gone status from the offer path, asserted by the client
| Boot Fake | service/slot-init/src/boot/tests.rs | boot partition | no | "refused by the test" and power cut; missing: a real mount or UBI errno through the boot refusal |
| Watchdog Fake (dog) | service/slot-init/src/dog.rs | hardware watchdog | no | ENOTTY mapped to Unsupported; the real driver maps every failure to Failed (drivers/hw-watchdog/src/lib.rs:203, :207). Missing: ENOTTY asserted as Failed |
| Good-boot Fake | service/good-boot/src/tests.rs | boot state marker | no | Clock and script probe, no failing read; missing: marker read I/O error through the real probe, asserted as not good |
| Sensor-proof Recording | testing/crates/camera-proof/src/bin/lemnos-sensor-proof.rs | real I2C bus | no | Wraps the real bus, no failing case; missing: a bus NACK surfaced as Nack through the wrapper |
| Camera-proof sims (sensor) | testing/crates/camera-proof/tests/sim/mod.rs | camera sensor | yes | Sensor NACK: chain_sensor_nack.rs:18-40 refuses by name; pinned to ENXIO and EREMOTEIO (:89-95). The sim's own BUSY and NOT_FOUND are unpinned |
| Camera-proof SimTiming | testing/crates/camera-proof/tests/sim_timing/mod.rs | frame timing | no | No failure mode; missing: the clock moving backwards from the real timing source (the clock file path has it, chain_clock_refusals.rs:83) |
| camera-sim SimBuffers, SimReceiver | testing/crates/camera-sim/src/lib.rs | receiver driver and buffer pool | no | BUSY on take (lib.rs:100), no #[test] here; missing: START refused with EBUSY (rkcif hw.rs:75) asserted through SimReceiver |
| FakeFan | testing/crates/fake-devices/src/lib.rs | hwmon fan | no | Refused writes give IO (controller_on_fakes.rs:225); missing: a sysfs write EIO asserted as IO in hwmon-fan |
| FakeCounter, FakeFifo, FakeThermalZone, FakeProbe | testing/crates/fake-devices/src/lib.rs, probe.rs | IIO sensors, thermal, probe | not judged | FakeCounter's IO is asserted (lemnosd/tests/service.rs:177); the FIFO, thermal and probe failure tests not read |
| K230 USB sims | testing/crates/k230-usbboot/src/b2sim.rs, sim.rs | K230 boot ROM and USB link | no | Pipe Stall and NoDevice are pinned (usb.rs:322); missing: a sim Stall or Timeout asserted through map_error |
| K230 SD sims | testing/crates/k230-usbboot/src/sdcard.rs | SD card | no | Happy path only (:449-497); missing: SimSdhci CMD timeout or CRC error asserted through the SdError mapping |
| K230 stage fakes | testing/crates/k230-usbboot-stage/src/fl1/check.rs, sd.rs | timebase counter, SD card | yes | CounterStuck and TimebaseStuck asserted (check.rs:413-418, :483-491); SD reply per command (sd.rs:237, :370). Spec-level, not a board run |
| K230 b2 Fake | testing/crates/k230-usbboot/tests/b2.rs | USB link open | no | EACCES named in the verdict (b2.rs:318-326); missing: a real open EACCES mapped through the backend and asserted |
| K230 SimTimer | testing/crates/k230-usbboot/tests/stage_report_dry.rs | timer | no | Cannot fail; missing: an interrupted sleep or a clock regression through the real timer |
| Maskrom Stand | testing/crates/maskrom-rehearsal/src/lib.rs | maskrom flasher device | no | Refusals are its own enum; missing: a real USB timeout during send_471 mapped through usb.rs:40 and asserted |
| standin StandIn and FakeSys | testing/crates/standin/src/lib.rs | sysfs tree and lemnosd | not judged | StandIn starts the real Service (lib.rs:167); the missing-device test (tests/consumer_shapes.rs:252) not read in full |

### Count

Of the families above, the verdicts are: yes for the families marked yes, no for the rest, and
not judged for the few rows that were not read deeply enough. The "not judged" rows are open.
