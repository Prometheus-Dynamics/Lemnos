# MAG-GATE board run: magnetometer plausibility on Raze #1

Status: untested on board. The unit tests are in `drivers/orientation` (the filter's
`fusion/orientation/tests.rs` and the driver's `tests.rs`); they run the gate through `read` and `health`
on the simulated clock.

Why this run. On Raze #1 the magnetometer read about 360 µT at every sample of the B10 run
(`testing/board-runs/B10.md`: 0.000201, -0.000237, -0.000181 T, a total of about 360 µT, where the Earth's
field is 25 to 65 µT). Something magnetic sits near the chip. Before this change the orientation took its
heading from that field with health `ok`. The gate ignores a field whose total is outside 15 to 80 µT, and
says so in health: `magnetometer ignored: field magnitude implausible`.

Known risk, to check on the first board that trips it. The orientation calibrates nothing; it uses what the
sensor drivers report. A board whose magnetometer carries a hard-iron offset can report a total above 80 µT
with no magnet near it (dev's calibrator cites a 140 µT offset giving 90 to 190 µT), and on such a board the
gate ignores the magnetometer for as long as the offset stays: the heading is the gyroscope's and health says
so. Its heading from the uncorrected field would have been wrong anyway. The nearest data on record: B9
(`testing/board-runs/B9.md`, check 2) read 65.1 to 67.6 µT still, above the expected 25 to 65 µT range,
with an unexplained step noise of up to 3.5 µT (only 4 of 19 steps had every axis within 1 µT). That is
inside 80 µT, but with little margin. If this board trips the gate with no magnet near it, record the raw
axes and the total over 10 s still, and treat it as an offset to calibrate in the sensor driver, not as a
reason to widen the bound.

Setup: the board as in B10 (bmi088 on the PIO bus, bmm150 on `i2c-1` at 0x10), the orientation device in
nine-axis mode:

```toml
[[devices]]
id = "orientation"
driver = "orientation"
uses = { accel = "imu", gyro = "imu", mag = "mag" }
config = { sample_rate_hz = 100, mode = "9axis" }
```

Build the image on the NVMe or SSD, never on the HDD. Take the board lock named in `AGENTS.md`. Each
check reads the orientation device with `lemnos-ctl watch orientation roll --period-ms 20 --count 250` and
its health with `lemnos-ctl show orientation`, and records both with the board's time.

1. With the source still in place (the 360 µT field), bind and read for 10 s.
   Pass: the health is `Degraded("magnetometer ignored: field magnitude implausible")` at every health
   sample after the first read, and the magnetometer's field read is 350 to 370 µT (the value B10 recorded).
2. Still in the 360 µT field, turn the board by a measured angle (about 30 degrees, by hand) about its
   vertical axis, and hold it for 10 s.
   Pass: the yaw changes by the measured angle, within 3 degrees. With the field ignored the heading is
   the gyroscope's, so it turns with the board and is not pulled toward the field's heading. This check
   needs hands and a known angle: record the angle as the bench compass shows it.
3. Hands action: move the magnet and any steel clear of the chip. Read for 10 s.
   Pass: within 1 s of the field returning to the 25 to 65 µT range, the health returns to `ok` and stays
   `ok`, and the yaw follows the field's heading again (it moves toward the compass heading of the board).
4. Record the time of the move in check 3, and the first health sample that reads `ok` again.

Restore: `lemnosd` and `lemnos-orion` active, `systemctl --failed` 0, the same boot id, and the board's
units as they were before the run. Remove any scratch directory the run made.

Result: not run. Checks 1 and 3 need the source on the board; check 2 needs hands and a compass.
