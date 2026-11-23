# READ-FLOOR: the daemon reads a subscribed reading at a period, never in a loop

Status: **untested on board.** The measurement that found it is below; the same measurement on a
build with this change is the check.

## What was measured (Raze #2, Lemnos bbb3c420, 2026-11-23, by Atlas)
Three 60 s windows from /proc (per-thread utime+stime and voluntary switches). lemnosd used 50.2 %
of a core with no session, 47.6 % with a session idle, 41.4 % during pushes. In the first window:
`lemnosd-readers` 24.9 % and 38,272 wakes/s; four lanes at 3.2 to 3.3 % and about 14,000 wakes/s
each; one at 6.8 % and 11,616/s; one at 1.6 % and 6,402/s; three at 0.5 to 0.6 % and 519 to 623/s;
`pio-i2c-tx` 1,245/s. Files: `cpu-2026-11-23/report.txt`, `w1.txt`, `w2.txt`, `w3.txt` in Atlas's
session directory (kept off the repository).

## Cause
lemnos-orion subscribed to every value it publishes with a period of 1 us, on change. lemnosd
granted `max(asked, what one read costs)`, so each reading was read again the moment its last read
ended. On change only stopped the value being sent. Each read is a call on the device's lane.

## The change
- lemnosd: no reading is read faster than `[host] read_floor_ms` (10 by default, 1 to 60000).
- lemnos-orion: asks at `LEMNOS_ORION_PERIOD_MS` (250 by default, 1 to 60000).

## Steps
1. Build lemnosd and lemnos-orion at the READ-FLOOR head; boot the board; leave it idle 5 minutes.
2. Repeat the three 60 s windows as before (no session, a session idle, during pushes).
3. Record per thread: CPU % and voluntary switches a second. Expected at idle: `lemnosd-readers`
   at most 4 reads a second for each readable method of each device, so a few hundred wakes a
   second in all and well under 1 % of a core; no lane above that; `pio-i2c-tx` at the I2C devices'
   own rate (4 a second for each readable method of the IMU, the magnetometer and the power
   monitor, plus what their drivers' own ticks read).
4. `lemnos-ctl check --board <file>` prints `read floor 10 ms`.
5. Atlas's readings page still shows every value it showed, each at most 250 ms old.

## Which readings on this board have a fast path
- Streams today: `cam0-sensor` values, `cam0-receiver` frames, `cam0-fe` and `cam0-isp`.
- No stream: `imu`, `magnetometer`, `power`, `cpu-thermal`, `orientation`. A subscription to one of
  their readings gives at most 100 values a second at the default floor.
- The IMU's fast series is `read_batch` (every sample its FIFO held, each stamped), a call, not held
  to the floor. A consumer that had 1 kHz samples keeps them that way, not by subscribing.
- A board that needs a faster subscription sets `read_floor_ms` down to 1.

## Other things that poll, found by looking for the same shape (rates from the code, not measured)
Nothing else in the daemon, the bridge or `lemnos-ctl` asks for the fastest period, and no driver's
tick rearms itself at once without work having moved. What polls on a timer:
- `usb-gadget`: every 20 ms while up, to read endpoint 0 events (50 a second, always); every 1 ms
  while a consumer has its data stream open and nothing moves (1,000 a second). Both could wait on
  the function's descriptor. Not changed here.
- Job-serving drivers with no doorbell from their stream (`pisp-fe`, `pisp-be`, `ov9782`'s values
  ring, and the other boards' image and codec blocks): every 1 ms, only while streaming or with a
  job open. `rp1-csi2` waits on its doorbell and falls back to 33 ms. Not changed here.
- lemnos-orion: its file receivers and its named offers look every 50 ms, its job waits every 50 ms,
  its lease every 200 ms, its link watcher subscribes at 100 ms. Tens of wakes a second in all.
- lemnosd's event thread sleeps until the next driver tick is due, never less than 1 ms.

## Results
(none yet)
