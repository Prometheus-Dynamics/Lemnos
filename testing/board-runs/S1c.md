# S1c: hwmon-fan puts its fan at full duty on release

Status: untested on board. The driver's release and drop now write full duty (`pwm1` 255) and leave
the fan in manual mode, instead of restoring `pwm1`, `pwm1_enable` and the thermal governor's state.
Covered by unit tests over a fake sysfs tree (`drivers/hwmon-fan/src/tests.rs`) and by the host tests
(`engine/host/tests/host_owned.rs`, `host_owned_drivers.rs`). Nothing here has written a real hwmon node.

This check replaces the release step of `B12.md` on the reference board, on the CM5 (Raze #2), under
`/run/lock/board-gpu.lock`. Board file: an `hwmon-fan` device on the board's fan, by `name` or `hwmon`.

1. Take over. Start the daemon, drive the fan to 50 % with the controller, and read `pwm1`,
   `pwm1_enable` and the thermal zone's cooling state (`cur_state`). Pass: `pwm1_enable` reads 1 and
   `pwm1` reads the duty set (128 for 50 %).
2. Release. Release the fan (disable the controller, or stop the driver through the daemon).
   Pass: `pwm1` reads 255 and `pwm1_enable` still reads 1. The thermal governor does not change
   `pwm1` afterwards while the driver is down.
3. Drop. Stop the daemon with the fan taken. Pass: `pwm1` reads 255 and `pwm1_enable` reads 1.
4. Never taken. Start the daemon with the fan not driven, then stop it. Pass: `pwm1` and
   `pwm1_enable` are exactly as the kernel left them.

Record each value read, and the time the daemon took to stop, under the check's number.
