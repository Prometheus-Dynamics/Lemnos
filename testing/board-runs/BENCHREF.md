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

## Rerun 4 (2026-11-17, rev 8642b8e8 + the harness commits to 732cb53e): the load rows are measured

Records: `RAZE/bench-run/` on the HDD scratch (run4.log, board-rows2.tsv, board-logs-all.tar: the whole remote run
directory, with board-side.sh, the IMU watch output and the daemon log).

What was wrong, and is now fixed:
- The IMU watch asked for the item `acceleration`. The IMU's items are `acceleration.x`, `.y`, `.z` (its describe
  output: `value acceleration.x [m/s²]`). The watch failed at once with `no item "acceleration"`, so no IMU row could
  ever be measured. Fixed in 732cb53e.
- The USB power lines in the bench board file were on `gpiochip0`, which this board does not have. The board's
  GPIO chips are 11 to 15; the USB power lines are on `gpiochip15` (the RP1 pinctrl, lines 16 and 20: `gpio-16`,
  `gpio-20` in /sys/kernel/debug/gpio). Fixed in 732cb53e (`bus = "gpiochip15:16"`, `"gpiochip15:20"`).

Rows:
- IMU 200 Hz (`lemnos-ctl watch imu acceleration.x --period-ms 5 --count 9000`): delivered 9000 of 9000, misses 0,
  largest gap 6.17 ms, repeats 273 (readings equal to the one before). Dev: 9000 or 9001 of 9000 delivered, no misses,
  largest gap 5.1-5.2 ms (TODO.md at ed43ff3, item 8). Delivery and misses agree with dev; the largest gap is about 1 ms
  longer on next. Read time is not exported on next, so it is not measured.
- CPU idle (30 s, no subscriber): 0.00 %. Dev: 0.7-0.8 %.
- CPU at 50 Hz (30 s, a subscriber at 20 ms): 0.53 %. Dev: 1.4-1.5 %.
- CPU at 200 Hz (45 s, the 200 Hz IMU run): 1.51 %. Dev: 3.1-3.2 % (5 ms and 1 ms subscribers).
  Next is about half of dev's CPU at each load. The subscribers are the same periods as dev's, but the method is
  /proc utime+stime over 30 or 45 s windows, not dev's own; the difference is recorded, not explained.
- Power readings: values only; the power monitor is degraded on this board (chip not verified), not a reading.
- Threads 13, PSS 2932 kB.

What the startup list showed: at the moment the socket appears (t=0), the IMU reads degraded (gyroscope: waiting
for the chip to reset) and the power monitor may read degraded (chip not verified). Both are `running ok` within
0.1 s and stay ok for the 30 s capture. No flip after bind was found (RAZE/flip/, three captures). The bench rows
above use the state after that start.

USB-A power (usb.cycle): not measured, for the reason below. Step 2 capture (RAZE/flip/usbout/):
- The line `gpio-20` (usb-a-power) is `out hi`, consumer `lemnos`, before the switch.
- `lemnos-ctl off usb-a-power` releases the line (no consumer and no level shown in debugfs) for the whole off window.
- `on` drives it `out hi` again.
- The sysfs USB device count stays at 2 for the whole 3 s off window (the Creative webcam on bus 3 and the 1a6e:089a
  device on bus 1 are not removed).
- VBUS state is not exposed on the board. So whether the port power was cut is NOT verified. Needs a physical check
  or a VBUS probe before this row can say anything.
- usb.cycle in the harness stays `not_measured` (devices did not drop).

Restore: both units active, failed 0, helios-engine active, failsafe stopped and removed; the board's remote run
directory removed. Usb1 profile deleted.

## usb.cycle: the hardware and the wiring work (2026-11-17, read-only of the switch's own method)

Measured on Raze #2 under the failsafe (RAZE/flip/u2/usb2-log.txt):
- `lemnos-ctl call usb-a-power set false` (the power-switch contract's own `set(on)`, method 1): line `gpio-20`
  goes `out lo` (held by `lemnos`), and the sysfs USB device count falls from 2 to 1 within 0.2 s (the Creative
  webcam on bus 3 drops off).
- `lemnos-ctl call usb-a-power set true`: line goes `out hi`, and the count returns to 2 by about 0.4 s.
So the GPIO line, the port power and the devices respond. VBUS is still not exposed on the board; the device count is
the evidence.

Why the earlier `usb.cycle` did not drop the devices: `lemnos-ctl off usb-a-power` is the daemon's device lifecycle,
not a power command. `set_power` (service/lemnosd/src/requests.rs:478) calls `host.off()`, and `Host::off`
(engine/host/src/host.rs:3546) drops the driver instance, which releases the GPIO line. The line is released, not driven
low (debugfs shows no consumer or level during the off window), and the port stays powered. `on` rebinds the driver,
whose `default_on = true` drives the line high again. So `off` never switched the port off.

Not changed here: the fix needs a decision on where the off path lives (daemon routing to the switch's `set`, or a
new lifecycle hook in engine/core, which is an A-series change). Until then, the harness's usb.cycle stays
`not_measured`, and the switch is only reachable through `call ... set`.

## usb.cycle through the fixed build (pr/USB-OFF, ea56e87d): measured (2026-11-17, 12:33 to 12:39 local)

Build under test: `origin/next` 8642b8e8 plus the USB-OFF change (pr/USB-OFF, ea56e87d), aarch64 musl, service profile.
Board: Raze #2, boot 0f80a3d6-6aa1-4814-ba3d-800bfff5dd06 (unchanged). Failsafe armed and cancelled each time.

Direct check (`lemnos-ctl off usb-a-power`, then `on`; RAZE/flip/u5/usb3-log.txt):
- before: `gpio-20` is `out hi`, held by `lemnos`; sysfs USB devices 2.
- `off`: the line is `out lo`, still held by `lemnos`; devices drop to 1 within about 0.4 s of the request.
- `on`: `out hi`, devices back to 2 within about 0.4 s.

Bench row (`board-side.sh` usb.cycle, the same run as the rows below):
- usb.cycle: measured, devices 2, down after 50 ms, back after 490 ms.

Other rows in this run (same build): imu 9000 of 9000 delivered, 0 misses, largest gap 5.68 ms, 274 repeats;
cpu idle 0.00 %, 50 Hz 0.53 %, 200 Hz 1.51 %; threads 13, PSS 2936 kB. The CPU load rows are still lower than dev's,
recorded as they were (see the rerun 4 section).

Restore: lemnosd and lemnos-orion active, `systemctl --failed` 0, helios-engine active, failsafe stopped, the remote run
directory removed.
