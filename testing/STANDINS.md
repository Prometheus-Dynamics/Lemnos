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
| `UnixStream::pair` as FunctionFS endpoint files | `drivers/usb-gadget/src/tests.rs` (`lending_rig`, line 1085), `drivers/usb-gadget/src/serve_tests.rs` (line 282) | FunctionFS endpoint files on `dummy_hcd` (kernel gadget) | None on the host. A real-failing case is specified here: with the gadget unbound, the endpoint must report the disconnect (`ESHUTDOWN`) where the pair reports EOF or `EPIPE`. It belongs in the QEMU gadget check (`testing/crates/gadget-check`, run through `testing/qemu/run.sh`) on `dummy_hcd`. | not built (needs a kernel with `CONFIG_USB_DUMMY_HCD`, `testing/qemu/build-kernel.sh`; not run) |
| Regular file or in-memory image as a block device | `drivers/slot-store/src/bays_tests.rs` (line 121, in-memory image), `drivers/system-slots/src/offline_tests.rs` (line 491, in-memory copy of the boot record) | A block device node (`S_IFBLK`) | `drivers/system-flash/src/target.rs`, `a_block_target_must_be_a_block_device_and_a_missing_one_is_said_so`: a regular file is refused as a block target. `service/ctl/tests/boot_add.rs`, `a_block_device_a_directory_and_a_file_that_is_not_fat16_are_refused_and_nothing_is_written`: a block node is refused by its file type. | real-failing case exists for the refusal; the in-memory image is not refused by the block path and is not assessed for the write path |
| `engine/hal` mock blocks (`MockUsbDevice`, `MockUsbHost`, `MockUsbFunction`, `MockRegisters`, `MockDma`, and the rest of `mock_blocks.rs`) | Driver and backend tests across the tree | The Linux backend (`backends/linux`, `backends/linux-sys`) on the real kernel | `MockUsbDevice` isochronous errors pinned to the backend's errno mapping (`EINVAL` to `InvalidInput`, `ENODEV` to `Unavailable`), with a fail-before in commit `a88326c0` | real-failing case exists for the USB errors; the other mock blocks not assessed |
| `testing/crates/standin` (`StandIn`, `FakeSys`): a real `lemnosd` for a consumer's tests | Consumer tests through `lemnos-standin` | The daemon (`service/lemnosd`) | `testing/crates/standin/tests/consumer_shapes.rs` runs the consumer shapes against the real service | real-object test (the stand-in is the real daemon) |
| `camera-sim` `SimReceiver` (BUSY latch) and the Nack sensor | Chain proof tests | The receiver driver's refusal, and the sensor's bus NACK | `chain_latch_beside` pins the latch's refusal (`Status(-16)`, BUSY); `chain_sensor_nack` removes the sensor from its bus and pins the refusal by name. Both in `a88326c0`. | real-failing case exists |
| Sim clock `FileClock` | Chain proof tests | The clock file the proof reads | `chain_clock_refusals.rs`: a short clock file, and a clock that goes backwards, are refused by name (`c8e2115b`) | real-failing case exists |

## Not assessed in this pass

These doubles are listed by file so the audit can continue. Each still needs a real-failing case or a
stated reason:

- `backends/linux/src/pio_i2c/bus.rs` (`Mock`), `backends/linux/src/gadget/tests.rs` (`FakeWire`)
- `backends/linux-sys/src/usbfs/fake.rs` (`FakeUsbfs`)
- `backends/sim/src/{board,clock,hints,i2c,spi,stream}.rs` (`SimBoard`, `SimClock`, `SimHints`, `SimI2c`, `SimSpi`, `SimFrameSource`)
- `drivers/*/tests` and `drivers/*/src/tests*` mocks: `a733-di`, `a733-video`, `coral-loader`, `flash-stage`, `g2d`, `hw-watchdog`, `indicator`, `k230-capture`, `k230-capture-sys`, `k230-gsdma`, `m2m-decode`, `named-files`, `nonai2d`, `pisp`, `rkaiisp`, `rkcif`, `rp1-csi2`, `slot-store`, `status-light`, `system-flash`, `system-slots`, `uvc`, `ws2812`
- `contracts/*/tests/contract.rs` (`Mock`, `SimNpu`)
- `engine/capi/tests/common/mod.rs` (`StandIn`), `engine/client/tests/common/mod.rs` (`FakeHost`), `engine/core/src/tests.rs` (`Fake`), `engine/host/tests/stream_*.rs` (`Standins`), `engine/host-static/src/slot.rs` (`Standing`)
- `service/lemnosd/tests/common/mod.rs` (`StandIn`), `service/lemnosd/src/supervisor.rs` (`Fake`), `service/orion/tests/*` (`Fake`, `FakeAsk`), `service/slot-init/src/{boot/tests,dog}.rs`, `service/good-boot/src/tests.rs`
- `testing/crates/fake-devices`, `testing/crates/camera-proof/tests/sim*`, `testing/crates/k230-usbboot*`, `testing/crates/maskrom-rehearsal`

The `UnixStream::pair` uses outside the FunctionFS rig (`backends/linux-sys/src/poll.rs`,
`backends/linux-sys/src/scm.rs`, `drivers/rp1-csi2/src/tests.rs` (the doorbell),
`service/lemnosd/src/helper.rs`, `engine/host/tests/wait_descriptor.rs`) are not assessed.
