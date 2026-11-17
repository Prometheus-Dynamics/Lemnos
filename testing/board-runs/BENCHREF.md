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

## Reruns (2026-11-17): the harness faults, found and fixed

Three board reruns on the same boot (0f80a3d6-6aa1-4814-ba3d-800bfff5dd06), each with the failsafe armed and cancelled
after the restore, each restoring the units in the right order (lemnos-orion stopped first; lemnosd started first). The
last one is the reference for the numbers below. Records: `RAZE/bench-run/` on the HDD scratch (run2.log, run3.log,
board-rows2.tsv, board-logs2.tar, after2.txt).

Harness faults found on the board, all fixed in commits 90d2dd0c and 66be8986:
- `date +%s%N`: the board's date prints a literal `N`, so every timing in board-side.sh failed under `set -u`
  (`N: unbound variable`). The clock is now `/proc/uptime` in ms (10 ms resolution).
- `python3`: not on the board. The 200 Hz stamping is now `lemnos-bench-stamp`, a static Rust binary
  (`testing/crates/bench-stamp`, tests `counts_delivered_misses_and_repeats` and `no_readings_is_not_measured`).
- `getconf` and `timeout`: not on the board. USER_HZ is 100; the bounds are a kill of our own PID.
- Restore order: lemnos-orion is stopped before lemnosd, and lemnosd is started first.

PC rehearsal: `bash -n` and a `set -u` run of board-side.sh in board mode, on a same-revision stand-in (the host
lemnosd with `--sim`), with the board's missing commands hidden from PATH. It runs to the end with no errors. On the
stand-in the IMU and power rows and the USB result are not measured, as expected without hardware.

## Rows on the board (rerun 3)

- Measured: lemnosd.check ok; lemnosd.start up; lemnosd.devices; threads.lemnosd 13; mem.pss_kb 2868.
- CPU, measured but NOT comparable with dev: cpu.idle 0.03 %, cpu.50hz 0.00 %, cpu.200hz 0.04 %. The IMU was not running
  during the 50 Hz and 200 Hz windows (see below), so no 50 Hz or 200 Hz load was applied. Dev's figures are idle 0.7-0.8 %,
  50 Hz 1.4-1.5 %, 200 Hz 3.1-3.2 % (TODO.md at ed43ff3). These are not a pass or a fail.
- IMU 200 Hz (imu.delivered, misses, largest gap, repeats): not measured. `lemnos-ctl watch` gave no readings, because
  the IMU reads `running degraded (accelerometer: waiting for its first power-up step)` at the device list.
- The daemon log (`lemnosd.log`, kept) shows `imu: running ok 37 ms after bind` and `power0: running ok 3 ms after bind`
  at bind, then the list shows them degraded. The state changes after bind; this is open (next item).
- power.readings: values only (~4.5e-9 V, the chip is not verified), not a reading.
- usb.cycle: NOT measured, a real board result. `lemnos-ctl off usb-a-power` succeeded, the sysfs USB device count stayed
  at 2 for 15 s (the Creative webcam on bus 3 and the 1a6e:089a device on bus 1 were not removed), then `on` succeeded.
  The harness itself now times the wait correctly. Cause on the board is not yet known: the port power may not switch,
  or the kernel does not remove the devices. Needs a gpio read (no gpioget on the image) or a check of the port's line.
- Restore: both units active, failed 0, helios-engine active, failsafe stopped.

## Not measured, still open for BENCHREF
- IMU 200 Hz row: needs the IMU running (see above).
- CPU at 50 Hz and 200 Hz: needs the IMU subscriber, so it waits on the IMU.
- usb.cycle: needs the port-power cause.
