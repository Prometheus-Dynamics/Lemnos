# BENCHREF board run: bench.sh --real against dev's figures (Raze #2, 2026-11-17)

Rev: origin/next 8642b8e8 (lemnosd, lemnos-ctl, aarch64 musl, service profile, built on the HDD target).
Board: Raze #2, serial a317bcbee5226d57, boot 0f80a3d6-6aa1-4814-ba3d-800bfff5dd06 (unchanged).
Harness: testing/bench/board-side.sh board, driven by hand with the same upload, unit stop list (lemnos*), restore and
failsafe as bench.sh --real. bench.sh itself was not used: it refuses a rotating-disk target and output dir, and the
build target is on the HDD by the coordinator's ruling. The report is testing/bench/report.py in board mode, against
testing/bench/reference.tsv, with dev's figures printed beside each row.

Hypothesis (before the run, RAZE/bench-run/hypothesis.md): the reachable rows are measured, the rest stay not measured,
and the restore matches the baseline.

## Restore (checked)
- Failsafe: bench-failsafe.timer armed at 1800 s, cancelled after the restore. Stopped: lemnos-orion, lemnosd. Restored:
  both active, `systemctl --failed` empty, helios-engine active. The stop order was wrong (lemnosd stopped first,
  orion started first on restore); both came up and the state matches the baseline. Noted as a harness fault.
- The USB-A power was not switched off (usb.cycle: "switch off 0").

## Rows
- Measured: lemnosd.check ok; lemnosd.start up; lemnosd.devices (list); threads.lemnosd 13; mem.pss_kb 2868;
  power.readings (values only; see below).
- Not measured, harness faults on this image:
  - imu delivered/misses/gap (200 Hz run): board has no python3, so stamp.py cannot run.
  - cpu.idle_pct, cpu.50hz_pct, cpu.200hz_pct: board-side.sh line 87, `N: unbound variable` under `set -u`.
  - usb.cycle: cycle incomplete (devices 2, down no, back no).
  - Binary size of the other service crates: not built (only lemnosd, lemnos-ctl).
  - Steady-state allocations, light, fan, camera: no step in this harness (their own board-run records).
- power.readings: values are ~1e-9 (bus volts, current) because ina2xx is `degraded: chip not verified` on this bench
  run. Not a reading; not a figure.

## Devices on the board at the run (lemnos-ctl list)
imu bmi088 degraded (accelerometer: waiting for its first power-up step); power0 ina2xx degraded (chip not verified);
fan, cpu-thermal, fan-control ok; usb-c-power and usb-a-power retrying (no GPIO line gpiochip0:16 / :20); ring and
ring-pixels off, failed (disabled). The IMU and power degradation on a bench run with `lemnosd --board` (not the M41
reduced file) is recorded, not investigated here.

## Against dev
dev's figures are in the report (TODO.md at ed43ff3). No row here is a pass or a fail against them: the timings were not
measured, so nothing can be compared.

## Left as found
Board: services active, failsafe stopped, remote run dirs removed (the .kept copy removed), usb1 profile deleted.
Records: RAZE/bench-run/ on the HDD scratch (run.log, board-rows.tsv, board-side.err, report.txt, after.txt).
