# SILENCE: a sensor that stops sending behind its driver is seen silent and restarted

Status: sim half built and run on the PC (`chain_silence`, below). The board step is written here and
has not run. Untested on board.

## What the check does

The F2i silence check: on the reference board, with the camera chain streaming, the sensor is made to
stop sending without its driver's knowledge. The driver's state does not change, so only its silence
watch can act. The check passes when the watch sees the receiver's frame starts stop and restarts the
sensor, and frames come out again after the restart.

Steps, in order:

1. Start the chain on the reference board (`cam0-sensor`, `cam0-receiver`, `cam0-fe`, `cam0-isp`) and
   wait until the sensor reports that it streams. This is a wait on the sensor's state, not on time.
   This wait is the pre-cut health wait the row names: it must hold before the cut, or the cut is taken
   before the sensor streams and the check measures nothing.
2. Take one frame, so the chain is live before the cut.
3. Cut: write the sensor's mode select to 0 over the sensor's bus, from outside the driver, with the
   driver's state left as it is. The driver still believes it streams.
4. Wait for the restart: the sensor's mode select reads 1 again. It must come no sooner than the
   driver's silence bound (`SILENCE_MIN_NS` in `drivers/ov9782/src/lib.rs`, 250 ms plus the margin the
   driver states) after the cut, and no later than the bound times four.
5. Take one more frame after the restart, and check it names the job posted after the restart.

Pass: every step holds. Fail: name the step that did not hold, with the numbers.

## Sim half (run on the PC)

`testing/crates/camera-proof/tests/chain_silence.rs`, the test
`a_sensor_that_stops_behind_its_driver_is_seen_silent_and_restarted`. It runs the chain's in-process
daemon with the real drivers (`ov9782` with its silence watch, the sim receiver, the sim front end).
The receiver is configured with the sensor's mode link and started before the sensor. After three
steps the stream runs, `lemnos_camera_sim::silence_sensor` writes the sensor's mode select to 0 on
its bus (a register write from outside the driver), and the sensor's state is unchanged. The test
waits, in real time (the watch judges by the host's clock), for the watch to restart the sensor
(mode select back to streaming). It checks the restart came after `SILENCE_MARGIN_NS` and then that
the sensor streams again.

Run: `a_sensor_that_stops_behind_its_driver_is_seen_silent_and_restarted ... ok` (1.03 s).

Fail-before: with `silence_sensor` made a no-op (planted in `testing/crates/camera-sim/src/lib.rs`),
the test fails at "the sensor is silent right after the cut". The plant was reverted.

## The board step (not built)

The board step needs a way to write the sensor's mode select over its bus, behind the driver. The tree
has no such path yet: the raw bus (`raw-bus/1`) claims only devices the board declares raw, and the
sensor is not one of them. The step is therefore written here, not run. Writing it needs one of: a
board-declared raw path to the sensor's bus, or a debug write through the daemon's bus client, decided
in the same change.

## Owner and bound

Run by the board session that owns the F2 camera runs, under the bench lock. Not run in this pass.
