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
