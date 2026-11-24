# SPIN-SWEEP: waits bounded by a count, swept on 2026-11-24

Why: three install tests in drivers/system-slots bounded a wait on the flush worker by a count of
ticks, and twice the count ran out first under the gate's load (t2, t3). The coordinator asked for the whole
tree to be swept in the same batch.

How: a scanner listed every `for _ in 0..N`, `while` and `loop` whose body polls something (tick,
poll, try_recv, exists, sleep, ...) and is bounded by a count, with no wall-clock deadline in the
loop: 152 loops. Each was read and classed: CONCURRENT (its exit waits on another thread, a process,
the kernel or the disk), DETERMINISTIC (simulated time or state the same thread steps), or
NOT-A-WAIT (a fixed repeat by design). 138 were DETERMINISTIC or NOT-A-WAIT.

CONCURRENT, and what was done:

| Loop | Waited on | Was | Now |
|---|---|---|---|
| drivers/system-slots/src/tests.rs `through_a_host_an_install_is_a_job…` | flush worker | 1000 ticks, no wait | 30 s deadline (SPEED-1, a8a07682) |
| drivers/system-slots/src/tests.rs `run_file_job` | flush worker | 10 000 ticks, no wait | 30 s deadline (SPEED-1, a8a07682) |
| drivers/system-slots/src/tests.rs `the_call_that_takes_the_last_byte…` | flush worker | 1000 ticks, no wait | 30 s deadline (SPEED-1, 99076eb9) |
| drivers/system-slots/src/tests.rs `install_slowly` | flush worker | 100 000 ticks, no wait | 30 s deadline (SPEED-1, 99076eb9) |
| engine/client/tests/tokio.rs `the_async_form_comes_back_after_a_restart` | fake host's threads | 200 tries, a Disconnected try returns at once | 30 s deadline, 5 ms between Disconnected tries |
| testing/crates/k230-usbboot/tests/b2.rs `SlowTree::release` | reader thread on a FIFO | 200 × 10 ms, then gave up silently | deadline per call; the product string's release must succeed in 30 s (asserted) |
| testing/crates/lend-check `wait_frame` | producer in the guest | 1000 × 2 ms | 10 s deadline |
| testing/crates/crash-init (state disk) | kernel disk scan | 200 × 100 ms | 20 s deadline |
| testing/crates/flasher-check `wait_for` | kernel disk scan | 400 × 50 ms | 20 s deadline |
| testing/crates/grow-live-init (/dev/nvme0n1p2) | kernel disk scan | 300 × 100 ms | 30 s deadline |
| testing/crates/nand-check `wait_for_headers` | UBI writing headers | 100 × 100 ms | 10 s deadline |
| testing/crates/uboot-grant-check `wait_for` | kernel disk scan | 200 × 50 ms | 10 s deadline |
| service/slot-init/src/sys.rs `vcio`, `ubi_block` | devtmpfs | 50 × 20 ms | 1 s deadline (`NODE_WAIT`), unchanged budget: it is boot behaviour |
| kernel/rs/vip.rs:640 | hardware reset | 10 tries, each 20 ms by the wall clock | left as it is: product driver code, already wall-clock per try |

A sleep loop's count times its sleep is a floor on its wall time, never less, so those were not
false reds waiting to happen like the spins were. They are converted anyway so that every wait in
the tree reads as a time.

## The PWM red (t4, t6): a stand-in that was not like the kernel, and a second, wider scan

t4 and t6 went red on `backends/linux/src/pwm.rs` `an_absent_channel_is_exported_then_granted`:
NotFound at the test's unwrap, 0.05 s and 0.13 s in. The cause was the test's stand-in for the
kernel's export. It made `pwmN` empty and wrote the attributes after, so the grant, finding the
directory, could reach `chown` of an attribute not yet written. The kernel's export makes the
directory with its attributes in it. The stand-in now builds it under another name and renames it
into place. With 50 ms planted between the directory and its files, the old stand-in fails as t4 and
t6 did (same line, same error, 0.05 s in) and the new one passes.

After t4 I named the wait's count bound (50 looks, 2 ms apart, about 100 ms) as the cause, and a
planted 300 ms delay failed the old code by that bound. That was not the failure the gate saw (it
failed well inside 100 ms), and t6 went red again with the clock bound in place. The count bound was
still a wait on another party bounded by a count, so it stays converted (both export waits: 1 s by
the clock, `EXPORT_BOUND`). The first scan missed it because its bound was a named constant
(`tries == EXPORT_TRIES`).

The second scan also matched counts held in named constants and in variables (`0..count`,
`tries < X`, `left -= …`): 26 more loops.

| Loop | Class | Done |
|---|---|---|
| service/orion/src/source.rs `describe_when_free` | waits on the daemon (BUSY), 500 × 2 ms | by the clock, 1 s (`BUSY_BOUND`); its test checks that a lasting BUSY is asked again and given up at the bound |
| backends/linux/src/gadget.rs `serve_stages` | waits on another thread's ep0 file operation (EAGAIN), 20 × 1 ms | by the clock, 500 ms (`STAGE_BUSY_BOUND`) |
| service/slot-init/src/boot.rs:938, :1238 | counts derived from a time (`env.wait / 100 ms`), through `ops.sleep`, which tests simulate | left: already a time, stepped by the simulated sleep |
| service/slot-init/src/sys.rs:261 | a timed wait that feeds the watchdog | not a count |
| drivers/slot-store/src/bays.rs:1033 | a step budget | not a wait |
| drivers/rp1-csi2/tests/frames.rs:697 | simulated time, with a simulated-time bound | deterministic |
| shm/ring-doorbell/tests/doorbell.rs:153 | the producer | not a wait |
| testing/crates/watchdog-init, watchdog-user | timed feeds in a guest, by design | not a wait |
| the 0..ms, 0..seconds, 0..count and 0..CONST helpers in tests (hw-watchdog, indicator, k230-vpu, rk-iep, rk-mpp-codec, rp1-csi2, usb-gadget, host, host-static, sensor-power-up, bmi088-batch, test_block) | simulated time, or fixed repeats | deterministic |

## Product code changed, and why each bound is right on a board

Test-only conversions: the k230-usbboot FIFO release is in `testing/crates/k230-usbboot/tests/b2.rs`
(a test's stand-in sysfs; neither the daemon nor the flasher runs it), and the client reconnect is
in `engine/client/tests/tokio.rs`. Only there does a missed bound assert.

In product code, every converted wait ends in the same named error it ended in before; no wait
asserts or panics.

| Product wait | Old bound | New bound | On timeout | Why it is right on a board |
|---|---|---|---|---|
| backends/linux/src/pwm.rs `SysfsPwm::open`, `grant` (export) | 50 looks, 2 ms apart (at least about 100 ms) | 1 s by the clock (`EXPORT_BOUND`) | `io::ErrorKind::NotFound`, as before: the same error reaches the same callers | The kernel's `export` write creates `pwmN` before it returns, so the first look finds it and the bound is never reached. The bound applies only to a channel the chip refuses to make, where the wait is spent once per attempt under the driver's capped backoff. |
| service/orion/src/source.rs `describe_when_free` (BUSY re-ask) | 500 asks, 2 ms apart (at least about 1 s) | 1 s by the clock (`BUSY_BOUND`) | the last `Refused { BUSY }`, as before: the same answer reaches the same caller | The same 1 s budget. A daemon that answers BUSY is doing a short exclusive step. A loaded board slows the asks themselves, so a count stretched the budget unevenly; a time keeps it at 1 s. |
| backends/linux/src/gadget.rs `serve_stages` (ep0 EAGAIN) | 20 tries, 1 ms apart (at least about 20 ms) | 500 ms by the clock (`STAGE_BUSY_BOUND`) | the `EAGAIN` itself, as before: the same error reaches the same caller | EAGAIN on ep0 means another file operation of the daemon's own holds the FunctionFS lock for a moment. On a board under camera load that thread can be off the CPU for longer than 20 ms, and the stage then failed a setup the host had sent correctly. 500 ms is still well inside the host's control-transfer timeout (USB 2.0: 5 s for the data stage), so the host never sees a slower answer than a failure would give it. |
| service/slot-init/src/sys.rs `vcio`, `ubi_block` (devtmpfs node) | 50 looks, 20 ms apart (at least about 1 s) | 1 s by the clock (`NODE_WAIT`) | `vcio`: the path, as before (its caller's handling of a missing node is unchanged); `ubi_block`: `Err("<node> didn't appear")`, as before | The budget is unchanged at 1 s. devtmpfs makes the node within milliseconds of the ioctl or the driver's probe, and slot-init is the only process running at that point of the boot. |

## The t7 red: daemon calls unwrapped inside a wait

Three tests in `service/lemnosd/tests/controller_on_fakes.rs` failed in t7 at load 169. They had
passed in every earlier train, so the code under test was not the cause:

- `case_3` got `Status(-19)` (`NO_DEVICE`) at its first wait: it asked for the controller's state
  before the controller had bound.
- Two others got `Status(-110)` (`TIMED_OUT`) about 8.5 s in: under that load one `state()` call
  went past its call bound.

Each wait called `control.state().unwrap()` inside the loop. A "not yet" answer panicked the
test where it should have meant look again. The three `fan-control-*` entries left in TMPDIR
were a consequence: the panics skipped the scratch directory's removal.

The fixes:

- `state()` now treats `NO_DEVICE` and `TIMED_OUT` as "not yet". It keeps asking for up to 20 s,
  20 ms apart, and fails the test on any other status.
- The scratch directory is a guard that removes itself on drop, panic or not.
- The same pattern, a daemon `list()` unwrapped inside a wait, was in `late_events.rs` and
  `service.rs`. Both now treat `Error::TimedOut` as "not yet" and fail on any other error.
- `keepalive.rs` reads the metadata of a file the test made before the daemon started. It cannot
  be missing, so it is left as it is.

How each fix was proven:

- `TIMED_OUT`: a one-off 5 s delay planted in the fake controller's `state` reproduced the t7
  failure in the old test, the same `Status(-110)` from the same `unwrap`. The new test passes
  with the plant in place.
- `NO_DEVICE`: a planted bind delay did not reproduce it here. The new helper handles it by
  construction.
- The scratch guard: a planted panic left nothing in TMPDIR.

## The t8 red, and the class it belongs to
Two of 7,080 tests failed in t8's gate, the other four lanes green. Both were timing assertions that
a loaded machine broke, each proven by a plant that fails the old test with the gate's signature and
passes the new one:

- `drivers/system-slots` `a_verify_slower_than_the_call_deadline_neither_overruns_a_tick_nor_loops`:
  "a tick took 616 ms" against a 500 ms wall-clock bound. The test shows that no tick waits for a
  2.5 s verify; a tick that waited would take the whole 2.5 s. At load 160 a tick that does not wait
  took 616 ms of wall time, descheduled. The bound is now the verify's delay. Plant: one 700 ms
  stall inside a tick; old "a tick took 700 ms", new passes.
- `service/lemnosd/tests/controller_on_fakes.rs` `the_controller_binds_to_the_fakes_and_follows_its_curve`:
  "the duty fell from 816 to 300 in 0.10 s", with the controller bound twice in its stderr. A call
  past the host's 2 s call deadline is a hung device: the host abandons the instance and binds it
  again. The old instance's drop hands the fan back, and the new controller starts at the curve, so
  the slew cannot hold across it. The board file in these tests sets `call_deadline_ms = 60000`:
  they follow the curve and the slew, not the hang path. Plant: one 2.5 s tick stall during the
  fall; old fails with two binds and "fell from 866 to 300 in 0.10 s", new binds once and passes.
  The product side (a working device declared hung under load) is `HANG-UNDER-LOAD` in TODO.md.
  t7's `NO_DEVICE` in `case_3` was most likely this same hung path, not a call before the bind;
  `state()` reads both again either way.

### Every call unwrapped inside a test's wait (148 sites)

A scan found every wait or poll in test code (a loop that sleeps or yields, or a closure given to a
wait helper) with an `.unwrap()`, `.expect(` or `?` inside it. Each was read and judged by what the
call can return under load: RISK (a transient "not yet": a call timeout, NO_DEVICE or BUSY before a
bind, a path another thread makes, an empty non-blocking read), SAFE (it cannot fail that way), or
UNSURE. 19 RISK, 127 SAFE, 2 UNSURE (both settled SAFE). Each RISK site now takes the transient
answer as not yet, inside the wait's own deadline, and fails on any other error or when the
deadline passes:

| Site | Change |
|---|---|
| `drivers/gpu-lease/tests/connection.rs:50` | TIMED_OUT asked again within the 20 s wait |
| `service/ctl/tests/out_buffers.rs:343` | a client timeout or TIMED_OUT (-110) asked again within the 15 s wait |
| `service/ctl/tests/out_buffers.rs:368` | TIMED_OUT retried beside the refused -16/-19 the loop already retried |
| `service/ctl/tests/show.rs:83` | TIMED_OUT retried beside refused -16 |
| `service/ctl/tests/walk.rs:67` | TIMED_OUT retried beside refused -16/-19 |
| `service/lemnosd/tests/call_outcome.rs:99` | a timed-out `outcome` is still pending; the 10 s loop asks again |
| `service/lemnosd/tests/callers.rs:108` | TIMED_OUT asked again within the loop's deadline |
| `service/lemnosd/tests/jobs.rs:274` | TIMED_OUT asked again within the loop's deadline |
| `service/lemnosd/tests/stall_kept.rs:60` | a timed-out read is no stall yet; the 2 s loop goes on |
| `service/lemnosd/tests/stream_fds.rs:107` | a cycle whose call timed out is dropped (the daemon ends that connection's streams); at least one cycle must complete |
| `service/lemnosd/tests/ticks.rs:117` | TIMED_OUT asked again within the loop's deadline |
| `service/lemnosd/tests/ticks.rs:131` | a timed-out `arm` armed again (it overwrites one slot, so a second arm is the first) |
| `service/orion/tests/description_bounds.rs:72` | a timed-out `list` is still pending within the 20 s wait |
| `service/orion/tests/live_walk.rs:204` | a timed-out run or finish asked again within WAIT (the actions are reads and power on/off) |
| `service/orion/tests/orion_bridge.rs:279` | a timed-out run or finish asked again within WAIT (reads; a repeated `take` by the same caller shows in `held`) |
| `testing/crates/camera-proof/src/bin/lemnos-fe-proof.rs:888` | left: a board proof; `stop` and `start` fail with a named error through `?`, there is no bound to retry within, and a timed-out `start` may have applied |
| `testing/crates/camera-sim/tests/instance.rs:683` | a timed-out `list` asked again within the wait |
| `testing/crates/k230-usbboot/src/usb.rs:265` | NoDevice from a device that left between the listing and the read or open is not yet; polling goes on |
| `testing/crates/restart-lemnosd-check/src/main.rs:49` | a timed-out `list` asked again until BIND_WAIT |

The UNSURE two:

- `testing/crates/stream-compose-check/src/main.rs:257`: SAFE (resolved: the FIRE ioctl returns only EFAULT or ENOTTY)
- `testing/crates/usb-echo/src/lib.rs:169`: SAFE (resolved: at most 2 jobs outstanding against 8 to 16 slots)

The SAFE sites, with the reason each cannot fail by load:

| Site | Call | Why |
|---|---|---|
| `contracts/self-test/tests/runner.rs:267` | runner.tick(...).expect on an Option | In-process runner with an injected now and a scripted bench; it returns Option with no clock or readiness state, so no load-dependent error. |
| `contracts/self-test/tests/runner.rs:422` | runner.tick(...).expect on an Option | Same in-process scripted runner with injected time; None can only mean a bug, not load. |
| `drivers/a733-video/src/module/tests.rs:495` | rig.driver.tick(now).unwrap() | In-process driver on a simulated clock (rig.now) with a mock module; an Err is a driver bug, not timing. |
| `drivers/codec-jobs/tests/host.rs:384` | server.serve(...).unwrap() | Server thread over the in-process host endpoint; publish fails only with InvalidInput for a bad buffer or size and the Hooked endpoint counts calls, not time, so no transient error. |
| `drivers/hw-watchdog/src/tests.rs:941` | dog.tick(at).unwrap() | Fixed count of ticks on a fake watchdog node in a temp tree with explicit times; nothing waits on the device. |
| `drivers/isp600/src/driver_tests.rs:1008` | d.tick(now).unwrap() | In-process driver with simulated time and a mock kernel; no readiness state. |
| `drivers/isp600/src/driver_tests.rs:1081` | streams.statistics.take_frame().unwrap() | Frame is produced synchronously by the in-process d.tick just before; None is a driver bug. |
| `drivers/isp600/src/driver_tests.rs:1295` | d.counters().unwrap() | In-process driver counters; errors only for a missing device, which is deterministic. |
| `drivers/isp600/src/driver_tests.rs:1304` | streams.*.return_buffer(...).expect | In-process ring; errors only if the test returns a buffer it does not hold, a test bug. |
| `drivers/k230-capture/src/tests.rs:544` | d.tick(now).unwrap(); stream.take_frame().expect | In-process driver on a simulated clock with a mock block; the frame is published synchronously by tick. |
| `drivers/k230-capture/src/tests.rs:671` | d.tick(0).unwrap(); stream.return_buffer(...).expect | In-process mock on simulated time; no transient error. |
| `drivers/k230-capture/src/tests.rs:746` | d.tick(now).unwrap() | Backoff loop on simulated time with an injected NO_DEVICE; an Err would be a driver bug. |
| `drivers/k230-capture/src/tests.rs:759` | d.tick(now).unwrap() | Simulated-time loop over an injected failure; no daemon or device timing. |
| `drivers/k230-capture/src/tests.rs:827` | d.tick(t).unwrap(); stream.take_frame().expect | Simulated-time loop over in-process mocks; the frame is published synchronously by tick. |
| `drivers/k230-capture/src/tests.rs:936` | d.tick(now).unwrap(); stream.take_frame().unwrap() | In-process mock on simulated time; no readiness state. |
| `drivers/k230-capture/src/tests.rs:949` | d.tick(now).unwrap(); stream.take_frame().unwrap() | Same in-process simulated-time loop, allocation-counted. |
| `drivers/k230-capture/src/tests.rs:1040` | d.tick(at).unwrap() | Simulated-time loop with a mock block; no transient error. |
| `drivers/k230-capture/src/tests.rs:1049` | d.tick(at).unwrap() | Simulated-time loop; the drop is judged by the driver, not by any wait. |
| `drivers/k230-capture/src/tests.rs:1079` | d.tick(t).unwrap(); stream.take_frame().unwrap(); return_buffer().unwrap() | In-process simulated-time loop; frames are published synchronously. |
| `drivers/k230-capture/src/tests.rs:1089` | d.tick(t).unwrap() | Simulated-time loop that advances the clock itself; no call can time out. |
| `drivers/k230-capture/src/tests.rs:1100` | d.tick(t).unwrap() | Simulated-time loop, same as 1089. |
| `drivers/k230-usbboot-stage/tests/probe.rs:113` | hw.sent.last().unwrap(); telemetry::decode(...).expect | Simulated stage with a stepped clock; last() is None only if nothing was sent, which the busy guard rules out, and decode is local. |
| `drivers/k230-vpu/src/codec_tests.rs:704` | s.take().unwrap() | In-process codec session after a synchronous s.tick() and mock finish; take returns an Option from an in-process queue. |
| `drivers/k230isp/src/driver_tests.rs:745` | streams.statistics.take_frame().unwrap() | Frame is produced by the in-process d.tick just before; None is a driver bug. |
| `drivers/k230isp/src/driver_tests.rs:778` | d.tick(now).unwrap() | Simulated-time loop with an injected failure; no daemon or device timing. |
| `drivers/k230isp/src/frames_tests.rs:939` | rig.config.return_buffer(0).expect | In-process mock ring; errors only if the test returns a buffer it does not hold. |
| `drivers/k230isp/src/frames_tests.rs:1130` | rig.output.take_frame().unwrap(); rig.statistics.take_frame().unwrap() | In-process mock streams after a synchronous tick; no readiness state. |
| `drivers/pisp/src/fe_device/frames_tests.rs:546` | fe.tick(now).unwrap(); frames.ring.pop_entry().unwrap() | In-process front end and ring on simulated time; Err only on a corrupt ring. |
| `drivers/pisp/src/fe_device/frames_tests.rs:713` | fe.tick(now).unwrap(); second.ring.pop_entry().unwrap() | Same in-process simulated-time pattern as 546. |
| `drivers/pisp/src/fe_device/tests.rs:766` | r.fe.tick(now).unwrap() | Simulated clock with fail_make injected by the test; an Err is injected, not timing. |
| `drivers/pisp/tests/be_frames.rs:788` | app.output.ring.pop_entry().unwrap() | In-process ring of a driver that never started; Err only on a corrupt ring. |
| `drivers/pisp/tests/be_frames.rs:825` | host.device("isp").unwrap(); host.stream_stats(...).unwrap() | In-process Host after host.tick(); an unknown id is a test bug, not timing. |
| `drivers/pisp/tests/be_frames.rs:859` | be.tick(now) next.unwrap(); app.output ring pop_entry().unwrap() | In-process backend on simulated time; allocation-counted loop with no waiting. |
| `drivers/pisp/tests/be_frames.rs:862` | be.tick(now) next.unwrap() | Same simulated-time tick as 859. |
| `drivers/pisp/tests/fe_frames.rs:970` | fe.tick(now) next.unwrap(); client.ring.pop_entry().unwrap() | In-process front end on simulated time; Err only on a corrupt ring. |
| `drivers/pisp/tests/fe_frames.rs:974` | fe.tick(now) next.unwrap() | Same simulated-time tick as 970. |
| `drivers/pisp/tests/fe_frames.rs:1028` | host.device("isp").unwrap(); host.stream_stats(...).unwrap() | In-process Host after host.tick(); unknown names are test bugs. |
| `drivers/rga/tests/stream.rs:125` | consumer.pop_entry().unwrap() | shm ring filled by the in-process host on a fake atomic clock advanced by the test; Err only on a corrupt ring. |
| `drivers/rga/tests/stream.rs:183` | consumer.jobs().post(...).unwrap(); consumer.pop_entry().unwrap() | post returns Ok(false) when full (asserted true), so Err is only for an invalid argument; the ring is in-process with a fake clock. |
| `drivers/rga/tests/stream.rs:190` | consumer.pop_entry().unwrap() | Same in-process fake-clock ring as 125. |
| `drivers/rga/tests/stream.rs:270` | consumer.pop_entry().unwrap(); host.stream_detach(...).unwrap() | In-process host and ring on a fake clock; detach errors only for an unknown stream. |
| `drivers/rk-iep/tests/host.rs:382` | driver.tick(now).unwrap(); caller.completion().unwrap() | In-process driver with a mock caller on simulated time; completion is present after the synchronous tick. |
| `drivers/rk-iep/tests/host.rs:521` | driver.tick(now).unwrap(); caller.completion().unwrap() | Same in-process pattern as 382. |
| `drivers/rkcif/src/tests/mod.rs:673` | d.tick(now).unwrap() | Simulated-time backoff loop with an injected failure; no timing. |
| `drivers/rkcif/src/tests/mod.rs:679` | d.tick(now).unwrap() | Same simulated-time loop as 673. |
| `drivers/rkcif/src/tests/mod.rs:757` | d.tick(now).unwrap(); d.link().unwrap(); d.limits().unwrap() | In-process driver on simulated time; errors only for bad state. |
| `drivers/rkcif/src/tests/raw.rs:401` | client.ring.pop_entry().unwrap() | In-process ring fed synchronously by rig.tick(); Err only on a corrupt ring. |
| `drivers/rkcif/src/tests/raw.rs:499` | client.ring.pop_entry().unwrap() | Same in-process pattern as 401. |
| `drivers/rkisp/src/driver_tests.rs:1118` | d.tick(now).unwrap() | Simulated-time backoff loop; no daemon or device timing. |
| `drivers/rkisp/src/driver_tests.rs:1327` | streams.statistics.take_frame().unwrap(); return_buffer().expect | Frame is published by the in-process tick just before; None is a driver bug. |
| `drivers/rp1-csi2/src/tests.rs:784` | d.tick(now).unwrap() | Simulated-time backoff loop with injected failure. |
| `drivers/rp1-csi2/src/tests.rs:790` | d.tick(now).unwrap() | Simulated-time loop, same as 784. |
| `drivers/rp1-csi2/src/tests.rs:876` | d.tick(t).unwrap(); frames.take_frame().expect | Simulated-time loop over in-process mocks; the frame is published synchronously. |
| `drivers/rp1-csi2/src/tests.rs:1519` | d.tick(t).unwrap() | Simulated time advanced by the loop itself; no call can time out. |
| `drivers/rp1-csi2/src/tests.rs:1536` | d.tick(now).unwrap(); frames.take_frame() | In-process simulated-time loop; frame is published synchronously. |
| `drivers/rp1-csi2/src/tests.rs:1612` | d.tick(0).unwrap() | In-process simulated-time loop. |
| `drivers/rp1-csi2/src/tests.rs:1641` | d.tick(0).unwrap(); frames.take_frame().expect; return_buffer().unwrap() | In-process simulated-time loop; frames are published synchronously. |
| `drivers/rp1-csi2/tests/frames.rs:512` | receiver.tick(now).unwrap(); frames.ring.pop_entry().unwrap() | In-process receiver and ring on simulated time with mock hardware. |
| `drivers/rp1-csi2/tests/frames.rs:552` | receiver.tick(now).unwrap(); frames.ring.pop_entry().unwrap() | Same in-process pattern as 512. |
| `drivers/rp1-csi2/tests/frames.rs:659` | receiver.tick(now).unwrap(); pop() | Loop advances simulated time itself; no daemon or device call can time out. |
| `drivers/rp1-csi2/tests/frames.rs:697` | receiver.tick(*now).unwrap() | Simulated-time loop with an explicit give-up time; tick is in-process. |
| `drivers/rp1-csi2/tests/frames.rs:748` | receiver.tick(now).unwrap() | Fixed-count simulated-time loop. |
| `drivers/rp1-csi2/tests/frames.rs:775` | receiver.tick(now).unwrap() | Fixed-count simulated-time loop. |
| `drivers/rp1-csi2/tests/frames.rs:836` | receiver.tick(now).unwrap(); frames.ring.pop_entry().unwrap() | In-process simulated-time loop, same as 512. |
| `drivers/rp1-csi2/tests/frames.rs:870` | receiver.tick(now).unwrap() | Fixed-count simulated-time loop over in-process hardware mock. |
| `drivers/slot-store/src/flush.rs:83` | barrier.poll()? | poll returns Err only when the flush worker is gone or the flush itself failed; the 5 s bound returns Ok(false), so no timeout error reaches the ?. |
| `drivers/sunxi-vin/src/tests/direct.rs:278` | client.ring.pop_entry().unwrap() | In-process ring fed synchronously by rig.tick(); Err only on a corrupt ring. |
| `drivers/sunxi-vin/src/tests/direct.rs:450` | client.ring.pop_entry().unwrap() | Same in-process pattern as 278. |
| `drivers/sunxi-vin/src/tests/direct.rs:525` | client.ring.pop_entry().unwrap() | Same in-process pattern; the slot expect reads the mock node's own state. |
| `drivers/sunxi-vin/src/tests/mod.rs:481` | d.tick(now).unwrap(); stream.take_frame().expect | In-process mock on simulated time; the frame is published synchronously. |
| `drivers/sunxi-vin/src/tests/mod.rs:532` | d.tick(now).unwrap() | Fixed-count simulated-time loop. |
| `drivers/sunxi-vin/src/tests/mod.rs:621` | d.tick(now).unwrap() | Fixed-count simulated-time loop. |
| `drivers/sunxi-vin/src/tests/mod.rs:719` | d.tick(now).unwrap() | Simulated-time backoff loop with injected failure. |
| `drivers/sunxi-vin/src/tests/mod.rs:725` | d.tick(now).unwrap() | Same as 719. |
| `drivers/sunxi-vin/src/tests/mod.rs:833` | d.tick(now).unwrap(); stream.take_frame().unwrap() | In-process simulated-time loop; frames published synchronously. |
| `drivers/sunxi-vin/src/tests/mod.rs:844` | d.tick(now).unwrap(); stream.take_frame().unwrap(); d.counters().unwrap() | In-process simulated-time loop, allocation-counted. |
| `drivers/system-flash/tests/driver.rs:267` | driver.write(size, format) with expect on the non-BUSY arm | Only Status::BUSY is retried and it is the one transient refusal; the other refusals write() returns (NOT_FOUND, NOT_PERMITTED, INVALID_ARGUMENT) come from the fixture, not from load. |
| `drivers/system-slots/src/tests.rs:6712` | board.slots.list(&mut buf).unwrap() | In-process driver step on a simulated board; Err only for a too-small buffer. |
| `drivers/uvc/src/tests/driver.rs:420` | rig.tick().unwrap() | Simulated-time backoff loop with an injected NO_DEVICE; no timing. |
| `drivers/vglite/tests/driver.rs:593` | driver.tick(now).unwrap() | Simulated-time backoff loop with an in-process driver. |
| `engine/capi/tests/common/mod.rs:248` | dma_buf::identity(handle.fd()).unwrap() | fstat on a live fd the thread holds; it cannot fail transiently. |
| `engine/client/tests/client.rs:609` | client.next_value(&mut into, 50 ms).unwrap() | next_value returns Ok(None) on timeout and its only Err is TooSmall for the fixed 8-byte buffer, so no timeout error reaches the unwrap. |
| `engine/client/tests/client.rs:661` | client.next_value(&mut into, 20 ms).unwrap() | Same as 609: Ok(None) on timeout, Err only TooSmall. |
| `engine/client/tests/client.rs:694` | client.next_value(&mut into, 50 ms).unwrap() | Same as 609. |
| `engine/client/tests/client.rs:726` | client.next_value(&mut into, 50 ms).unwrap() | Same as 609. |
| `engine/client/tests/client.rs:785` | client.next_value(&mut into, 100 ms).unwrap() | Same as 609. |
| `engine/host/tests/contain.rs:100` | host.next_retry_ns().unwrap() | In-process host with SimClock; an Option, None is a test bug. |
| `engine/host/tests/contain.rs:237` | host.next_retry_ns().unwrap() | In-process host with SimClock, same as 100. |
| `engine/host/tests/overrun_line.rs:123` | host.device(id).unwrap() | In-process host on SimClock; an unknown id is a test bug. |
| `engine/host/tests/resume.rs:190` | self.host.device("k").unwrap() | In-process host on SimClock. |
| `engine/host/tests/resume_refused.rs:159` | host.device("k").unwrap() | In-process host on SimClock. |
| `engine/host/tests/retry_off.rs:85` | host.device("r").unwrap() | In-process host on SimClock. |
| `engine/host/tests/stream.rs:642` | host.stream_stats(0, "frames").unwrap(); producer.next_buffer().unwrap() | In-process host driven by the test's own tick; errors only for an unknown stream or bad state, and the real-time sleep does not reach the host. |
| `engine/host/tests/stream.rs:770` | host.stream_stats(1, "frames").unwrap() | In-process host; unknown stream only. |
| `engine/host/tests/stream.rs:876` | host.device("e").unwrap() | In-process host on SimClock. |
| `engine/host/tests/stream_hold_restart.rs:134` | host.device("p").unwrap() | In-process host on SimClock. |
| `engine/host/tests/stream_upstream.rs:228` | self.host.device(id).unwrap() | In-process host on SimClock. |
| `engine/host/tests/stream_upstream.rs:425` | rig.host.device("s").unwrap() | In-process host on SimClock. |
| `engine/package/tests/resume.rs:40` | host.device(id).unwrap() | In-process host on SimClock. |
| `service/lemnosd/tests/caller_gone_hand_back.rs:343` | lines.lock().unwrap() | Mutex lock; poisoned only after a panic. |
| `service/lemnosd/tests/keepalive.rs:53` | fs::metadata(&file).unwrap() | The test created the file with fs::write before starting the daemon and the daemon appends in place, so NotFound cannot occur. |
| `service/lemnosd/tests/look_owner_gone.rs:141` | SHOWN.lock().unwrap() | Mutex lock; poisoned only after a panic. |
| `service/lemnosd/tests/notes.rs:97` | lines.lock().unwrap() | Mutex lock; poisoned only after a panic. |
| `service/lemnosd/tests/service.rs:197` | watcher.next_value(&mut into, 5 ms).unwrap() | next_value returns Ok(None) on timeout and its only Err is TooSmall for the fixed 8-byte buffer; not a timeout error. |
| `service/lemnosd/tests/service.rs:350` | client.next_value(&mut into, 30 ms).unwrap() | Same as 197: Ok(None) on timeout, Err only TooSmall. |
| `service/lemnosd/tests/start.rs:57` | lemnos_socket_dir::socket(&dir, "sock").unwrap() | Path built from a fixed scratch dir; the connect is_err() is the loop condition, not an unwrap. |
| `service/lemnosd/tests/stream.rs:201` | host(&running).stream_stats(...).unwrap() | In-process daemon Host via running.service; errors only for an unknown stream. |
| `service/lemnosd/tests/stream.rs:429` | host(&running).stream_stats(...).unwrap() | Same in-process Host accessor as 201. |
| `service/lemnosd/tests/stream_consumer_hold.rs:55` | stream.next_or_end(50 ms).expect("no stop") | Timeout is Next::TimedOut, not Err; the only Err is a doorbell poll failure (EINTR already gives Ok) or HUP/ERR, which needs the service to close its end, and this test keeps the service up. |
| `service/lemnosd/tests/stream_fds.rs:71` | UnixStream::connect(&running.socket).unwrap(); write_all; host.stream_stats().unwrap() | Blocking connect to a listening daemon socket waits on backlog rather than failing transiently; writes go into fresh socket buffers; host stats are in-process. |
| `service/lemnosd/tests/stream_fds.rs:95` | host.stream_stats(0, "frames").unwrap() | In-process daemon Host; unknown stream only. |
| `service/lemnosd/tests/stream_fds.rs:99` | host.stream_stats(0, "frames").unwrap() | In-process daemon Host. |
| `service/lemnosd/tests/stream_fds.rs:121` | host.stream_stats(0, "handles").unwrap() | In-process daemon Host. |
| `service/lemnosd/tests/stream_fds.rs:127` | UnixStream::connect(&running.socket).unwrap(); write_all; host.stream_stats().unwrap() | Same as 71: blocking connect to a live socket, writes into fresh buffers, in-process host. |
| `service/lemnosd/tests/stream_fds.rs:150` | host.stream_stats(0, "handles").unwrap() | In-process daemon Host. |
| `service/lemnosd/tests/stream_fds.rs:154` | host.stream_stats(0, "handles").unwrap() | In-process daemon Host. |
| `service/orion/tests/support/proxy.rs:43` | client.set_nonblocking(false).expect("blocking") | Set on a live accepted socket; accept errors are handled by a sleep and retry. |
| `service/orion/tests/support/proxy.rs:133` | rules.lock().unwrap(); sent.lock().unwrap() | Mutex locks; the read loop ends on Err. |
| `service/restart-helper/src/dbus_tests.rs:244` | w.write_all(...).unwrap() on a socketpair | The client's answer wait is 5 s and the server answers 300 ms after StartUnit, so the peer is alive when it writes; EPIPE needs a client that already gave up. |
| `shm/ring-doorbell/tests/doorbell.rs:153` | writer.publish(...).expect("publish") | ring/src/writer.rs publish errors are NoSuchField and TooLarge only, both deterministic; there is no full-ring error to be transient. |
| `shm/ring-holds/tests/holds.rs:291` | checker join/send expect("checker") | send fails only if the checker thread exited, which means it panicked on a real assertion; not a timing error. |
| `shm/ring-holds/tests/holds.rs:306` | checker join/send expect("checker") | Same as 291. |
| `testing/crates/camera-proof/src/bin/lemnos-chain-proof.rs:1279` | child.try_wait()?; child.kill()?; child.wait()? | OS calls on a child process; the deadline returns TimedOut on purpose, and no daemon or device call is made. |
| `testing/crates/camera-proof/src/bin/lemnos-fe-proof.rs:376` | s.output.next_frame(2 s)?; release()? | next_frame returns Ok(None) on timeout and the loop breaks with a message (a failed run, not a panic); its Err variants are StreamAtLimit, StreamWait or StreamTorn, none from load; release is a local ring op. |
| `testing/crates/camera-proof/src/bin/lemnos-isp-proof.rs:672` | output.next_frame(2 s)?; output.release()? | Same as fe-proof 376: Ok(None) on timeout ends the loop with a break, and the Err variants are not load-dependent; release is local. |
| `testing/crates/camera-proof/tests/chain_clock_refusals.rs:153` | child.try_wait().unwrap(); OpenOptions::open(&clock).unwrap(); write_all_at().unwrap() | OS calls; the clock file is created by the test before the child starts and written in place, so it cannot be missing or half-replaced. |
| `testing/crates/sim-clock/src/lib.rs:149` | self.read_once()? | read_exact_at on an open fd the sim writes in place; not-settled is Ok(None) and only a real I/O error reaches the ?. |

### Test scratch directories (221 sites)

The gate fails on anything a test leaves in TMPDIR, and a directory removed only at the end of a test
is left whenever the test panics first, so every red came with a second one. Every place test code
makes something under TMPDIR was read: 57 already GUARDED (a drop removes it), 80 NOT-TMP (the
`/tmp` socket dirs of `lemnos_socket_dir`, `CARGO_TARGET_TMPDIR`, or a namespace's own `/tmp`), and
84 that leaked on a panic. 83 now hold a guard, mostly `lemnos_scratch::Scratch` (a new test-only
crate, `testing/crates/scratch`, whose drop removes the directory or file). The shm crates take no
Lemnos crate: `shm/ring-buffers` unlinks its file right after creating it, and the `shm/fdpass` site
already did. The one left is `testing/crates/layout-check/src/main.rs:41`, a mode run by hand.

| Site | Was |
|---|---|
| `backends/linux-sys/src/aio.rs:856` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `backends/linux-sys/src/confined.rs:487` | end-of-body remove only (line(s) 488) |
| `backends/linux-sys/src/functionfs/tests.rs:13` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `backends/linux-sys/src/inotify.rs:127` | end-of-body remove only (line(s) 145) |
| `backends/linux-sys/src/mmap.rs:220` | end-of-body remove only (line(s) 246) |
| `backends/linux-sys/src/mmap.rs:252` | end-of-body remove only (line(s) 278) |
| `backends/linux-sys/src/mmap.rs:283` | end-of-body remove only (line(s) 303) |
| `backends/linux-sys/src/module.rs:921` | end-of-body remove only (line(s) 933) |
| `backends/linux-sys/src/usbfs/tests.rs:349` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `backends/linux-sys/src/usbfs/tests.rs:373` | end-of-body remove only (line(s) 388) |
| `backends/linux-sys/src/v4l2/mod.rs:1071` | end-of-body remove only (line(s) 1078) |
| `backends/linux/src/gadget/tests.rs:1976` | end-of-body remove only (line(s) 1977,2010) |
| `backends/linux/src/lib.rs:640` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `backends/linux/src/pio_i2c/mod.rs:124` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `backends/linux/src/pio_out.rs:271` | end-of-body remove only (line(s) 281) |
| `backends/linux/src/pwm.rs:346` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `backends/linux/src/stable.rs:288` | end-of-body remove only (line(s) 289,305) |
| `backends/linux/src/stable.rs:320` | end-of-body remove only (line(s) 321,366) |
| `backends/v4l2/src/node.rs:143` | end-of-body remove only (line(s) 149) |
| `backends/v4l2/src/v4l2.rs:305` | end-of-body remove only (line(s) 316) |
| `drivers/a733-video-sys/src/tests.rs:12` | end-of-body remove only (line(s) 24) |
| `drivers/hw-watchdog/src/tests.rs:249` | end-of-body remove only (line(s) 259) |
| `drivers/k230isp-sys/src/tests.rs:177` | end-of-body remove only (line(s) 205) |
| `drivers/m2m-decode/src/request.rs:118` | end-of-body remove only (line(s) 122) |
| `drivers/pisp/src/fe_hw.rs:474` | end-of-body remove only (line(s) 482) |
| `drivers/rknpu/src/tests.rs:1459` | end-of-body remove only (line(s) 1460,1480) |
| `drivers/rkvpss-sys/src/tests.rs:293` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `drivers/rkvpss/src/tests.rs:296` | end-of-body remove only (line(s) 306) |
| `drivers/rkvpss/src/tests.rs:317` | end-of-body remove only (line(s) 320) |
| `drivers/rpi-firmware-sys/src/lib.rs:268` | end-of-body remove only (line(s) 287) |
| `drivers/rpi-firmware-sys/src/lib.rs:301` | end-of-body remove only (line(s) 310) |
| `drivers/slot-store/src/bootimage_tests.rs:285` | end-of-body remove only (line(s) 286) |
| `drivers/slot-store/src/bootimage_tests.rs:498` | end-of-body remove only (line(s) 499,507) |
| `drivers/slot-store/src/flush.rs:128` | end-of-body remove only (line(s) 142) |
| `drivers/slot-store/src/gpt.rs:190` | end-of-body remove only (line(s) 193) |
| `drivers/slot-store/src/owner.rs:110` | end-of-body remove only (line(s) 111,125) |
| `drivers/slot-store/src/owner.rs:129` | end-of-body remove only (line(s) 130) |
| `drivers/slot-store/src/store.rs:583` | end-of-body remove only (line(s) 584,589,592) |
| `drivers/slot-store/src/twocopy_tests.rs:335` | end-of-body remove only (line(s) 351) |
| `drivers/slot-store/src/uboot_env_tests.rs:833` | end-of-body remove only (line(s) 871) |
| `drivers/system-flash/src/nand.rs:1367` | end-of-body remove only (line(s) 1368,1381) |
| `drivers/system-flash/src/target.rs:282` | end-of-body remove only (line(s) 318) |
| `drivers/system-flash/src/target.rs:367` | end-of-body remove only (line(s) 368,377) |
| `drivers/system-security/tests/orion_bound.rs:118` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `drivers/system-security/tests/orion_note.rs:93` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `drivers/system-security/tests/slots_without_flash.rs:94` | end-of-body remove only (line(s) 124) |
| `drivers/system-security/tests/slots_without_flash.rs:129` | end-of-body remove only (line(s) 145) |
| `drivers/system-slots/src/boot_guard.rs:268` | end-of-body remove only (line(s) 271,273,305) |
| `drivers/system-slots/src/boot_mark.rs:101` | end-of-body remove only (line(s) 122) |
| `drivers/usb-gadget/src/tests.rs:1442` | end-of-body remove only (line(s) 1450) |
| `engine/abi/src/tests.rs:344` | end-of-body remove only (line(s) 354) |
| `engine/hal/src/mock_blocks.rs:2001` | end-of-body remove only (line(s) 2008) |
| `engine/package/src/elf.rs:575` | end-of-body remove only (line(s) 578) |
| `engine/package/tests/common/mod.rs:101` | end-of-body remove only (line(s) 102) |
| `service/ctl/src/boot_dir_tests.rs:149` | end-of-body remove only (line(s) 150) |
| `service/ctl/src/boot_dir_tests.rs:319` | end-of-body remove only (line(s) 323,346) |
| `service/ctl/src/boot_dir_tests.rs:612` | end-of-body remove only (line(s) 613,631) |
| `service/ctl/src/boot_dir_tests.rs:876` | end-of-body remove only (line(s) 877,915) |
| `service/ctl/src/bridge_env_tests.rs:41` | end-of-body remove only (line(s) 54) |
| `service/ctl/src/grow_retry.rs:62` | end-of-body remove only (line(s) 66) |
| `service/ctl/src/lib.rs:1614` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `service/ctl/tests/board_meta.rs:8` | end-of-body remove only (line(s) 26) |
| `service/ctl/tests/firmware_check.rs:29` | end-of-body remove only (line(s) 35) |
| `service/ctl/tests/kernel_cmdline.rs:36` | end-of-body remove only (line(s) 93) |
| `service/ctl/tests/resource_names.rs:10` | end-of-body remove only (line(s) 30) |
| `service/good-boot/src/tests.rs:157` | end-of-body remove only (line(s) 168) |
| `service/lemnosd/src/gadget.rs:441` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `service/lemnosd/src/gadget.rs:486` | end-of-body remove only (line(s) 525) |
| `service/lemnosd/src/gadget.rs:531` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `service/lemnosd/src/helper.rs:1230` | end-of-body remove only (line(s) 1231) |
| `service/lemnosd/tests/caller_gone_hand_back.rs:120` | end-of-body remove only (line(s) 121) |
| `service/lemnosd/tests/packages.rs:11` | end-of-body remove only (line(s) 12) |
| `service/lemnosd/tests/stream_dma.rs:269` | end-of-body remove only (line(s) 279) |
| `service/lemnosd/tests/usb_gadget_file.rs:20` | end-of-body remove only (line(s) 37) |
| `service/slot-init/src/boot/tests.rs:2893` | end-of-body remove only (line(s) 2905) |
| `service/slot-init/src/kernel.rs:396` | end-of-body remove only (line(s) 440) |
| `service/slot-init/src/machine_id.rs:139` | end-of-body remove only (no Drop; the Dir guard in the file is a different test) |
| `service/slot-init/src/sys.rs:362` | helper returns the path; unguarded; removed only at end of caller test(s) |
| `service/way-back/src/tests.rs:99` | end-of-body remove only (line(s) 113) |
| `shm/fdpass/tests/socketpair.rs:17` | end-of-body remove only (line(s) 30) |
| `shm/ring-buffers/tests/unmapped.rs:129` | end-of-body remove only (line(s) 140) |
| `testing/crates/k230-usbboot/src/bin/k230-card.rs:628` | end-of-body remove only (line(s) 637) |
| `testing/crates/k230-usbboot/tests/b2.rs:425` | end-of-body remove only (line(s) 447) |
| `testing/crates/layout-check/src/main.rs:41` | end-of-body remove only (line(s) 74) |

### Starved runs

Every test that fails only when starved is a cause. Each run takes the workspace tests prebuilt in
one nextest archive (built unstarved) and runs them under a systemd unit with `CPUQuota=130%` and
13 test threads: about a tenth of a core per running test, less than a runnable thread gets at load
170, and nothing added to the machine's load. Runs 2 and on are at nice 10.

| Run | Tree | Tests | Failed | Wall | CPU-s |
|---|---|---|---|---|---|
| 1 | t8's stack (49688960), nice 0 | 7,044 | 3 | 1,821 s | 1,242 |
| 2 | the sweep (3621b5fc) | 7,046 | 1 | 1,196 s | 1,163 |
| 3 | the sweep with the camera fix (11cf70de) | 7,046 | 0 | 932 s | 1,131 |

Run 1's failures, each a cause:

- `testing/crates/camera-sim/tests/instance.rs` `exposure_and_gain_scale_the_light_and_the_black_level_is_64`:
  a frame wait got a stall event. The stream reports its producer stalled when a frame comes late,
  and starved it does; the frame comes after. The frame helpers pass over a stall and its resume
  within their wait (the stall tests read the events themselves).
- `backends/linux/src/gadget/tests.rs` `a_gadget_that_was_bound_is_bound_again_only_after_the_gap`:
  "the disable alone took 191 ms" against half of a 300 ms gap. Runs 1 to 3 were of this branch on
  `next` alone; in the stack it lands with, the same test already counts the gap's waits instead of
  timing the disable, so it needs no change here.
- `service/orion/tests/files.rs` `trial_says_whether_the_slot_failed_a_trial_or_is_the_one_running`:
  "no action lemnos.slots.call here". The test stopped lemnosd and started it again at once; the
  starved bridge answered through the new one, then noticed the old one gone, withdrew the slots and
  offered them again, and the next action fell between. The restart now waits for the bridge to
  withdraw them before the new lemnosd starts (with none running nothing can be offered again), then
  for the slots and their pushes as the setup does. Unstarved the bridge never withdraws, so a plant
  at the withdrawal did not reproduce it; the fix is by construction and the starved runs check it.

Run 2's one failure was the camera fix itself: it passed over a stall by asking `next` again, and in
lock-step that grants a second frame, so a control posted for a later frame came `Late`. A passed-over
stall is now waited out with `recv`, which grants nothing. Starved alone at a tenth of a core, the run-2
version failed 1 of 10 with `Late` and the fixed one passed 10 of 10.

Run 3 was clean, and left nothing in its TMPDIR. Runs 1 to 3 were of this branch on `next`; run 4
is of the whole stack the train lands.
