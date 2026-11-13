# F2d board run: the PiSP camera path at 3324fec4, and the reference board's unverified values

Run on Raze #2 (Compute Module 5 Rev 1.0, kernel 7.2.9-v8-16k) on 2026-11-13, with the module built
from 3324fec4 against the board's kernel tree (vermagic `7.2.9-v8-16k SMP preempt mod_unload modversions
aarch64`). Only what is listed here was run. The chain was NOT run end to end.

## Measured (this run)

| Check | Result |
|---|---|
| Raw path as root, 9000 frames, pattern 1, 910 lines, gated clock | 114.802 fps; 0 gaps; 0 dropped; 0 errors. This was as root, before the privilege drop. The privilege-drop regression was NOT run. |
| Front end through the links (sensor → receiver → front end → back end config), 600 frames | 114.803 fps; 0 frame starts without a job; 800 of 800 lines equal to the raw reference; guard bytes zero. Passes. |
| Back end on a captured frame, queued (two in flight), 1000 jobs, with `--checks 1` | Before F2d1: one job dropped per queued run (the proof named one config buffer for every job; the driver refuses a held config buffer). After F2d1: 0 dropped, identities in order, 1190 jobs/s. The checks themselves: not reproduced as passing (see below). |
| Back end paced, 500 jobs at 100 jobs/s, two in flight, after F2d1 | 500 of 500; in order; dropped +0; errors +0. Passes. |
| Back end, sharpen on (default filters) | Luma eighths unchanged; mean absolute luma difference from the unsharpened run 0.018. Changes the output, slightly. |
| Back end, resample to 640x400 | NOT SETTLED. The first board run showed an all-zero luma readout on that frame; the fix in the proof tool is untested on the board. |
| Back-end unbind and rebind during a stream, 10 cycles | 10 of 10 probes finished (10 jobs each); probe output hash 651730e8e82d7885 each time, matching the earlier record. The cut-off in-flight job exits 1 every cycle. |
| Receiver unbind and rebind during a raw stream, 5 cycles | FAILED 5 of 5. After each rebind the probe is refused: `sensor.select_mode` returns BUSY, because the sensor is left in a started state when its receiver is unbound mid-stream. |
| Full chain sensor → receiver → front end → back end with frames and timestamps (`lemnos-chain-proof`) | NOT RUN to completion. The receiver's counters are refused with EBUSY while the front end is linked: the receiver opens its node lazily, and the module allows one open per node. The chain tool reports this as "not checked" and exits 1. |
| Sensor control landing (`lemnos-sensor-proof latch`) with the chain linked | NOT RUN. `receiver.configure` returns EBUSY for the same reason. |
| Sensor start after the daemon starts | The first bind attempts fail with NO_DEVICE (-19) from the chip-ID check in the sensor driver; the sensor reaches running (degraded) after about 7 s (measured 7.1 s once). The cause is not established. |
| Back end with the stock driver for comparison | NOT RUN. |
| Lit scene (gain and exposure brightness) | NOT RUN: the scene is dark (mean about 20 against a black level of 16). |

Corrections to earlier records: the "9000 frames at 114.8 fps after the privilege drop" in the
camera runbook was as root (its section 5 says so); the back end's "100 of 100" unbind figure in the
0aa30510 notes is replaced by the 10-of-10 figure above.

## Reference board values checked on the board (2026-11-13)

Read-only: device tree, sysfs, the product's files, and register reads of known addresses on i2c-1
(`i2ctransfer` reads only).

- Magnetometer: a BMM150 at 0x10 on i2c-1 (chip ID register 0x40 reads 0x32). It is on i2c-1, not the PIO bus.
- Power monitor: an INA238-family chip at 0x40 on i2c-1 (manufacturer ID 0x5449; device ID register 0x3f reads 0x2381).
  The shunt is not readable from the board: the device tree names no `shunt-resistor` and the shunt
  calibration register holds its reset value. The 0.010 ohm in `reference.toml` is from the earlier run
  and the product's manifest, unverified against the schematic.
- Fan: the product's `/cooling_fan` (pwm-fan) takes channel 3 of the RP1 PWM at `pwm@9c000` (phandle 0x62
  = 98, the node that owns pin 45's pwm1 function), period 41566 ns. `reference.toml` said channel 1; corrected to 3.
- LED ring: the product's overlay is `ws2812-pio,gpio=13,num_leds=16`, 24 bits per LED on the wire.
  The GRB pixel order is the product's; this driver takes only the wire width, so the colour order is
  unverified and not readable without lighting the ring.
- Camera: the product's overlay is `ov9782,cam0,clk-continuous`. The driver's sensor modes are
  1280x800, 1280x720 and 640x400 (binned), each RAW10 and RAW8. The stream sizes in the reference file are
  this file's own choices and the product does not state them: unverified.
- Boot: `/boot/firmware` does not exist on the running image. The boot partition is `/dev/mmcblk0p1`
  (vfat, 79 MB, PARTUUID 5056525a-01), not mounted by the running system; it was mounted read-only for
  these reads and unmounted again.
- Base device tree: the running model is "Raspberry Pi Compute Module 5 Rev 1.0", compatible
  `raspberrypi,5-compute-module brcm,bcm2712`. The slot's boot file list names `bcm2712-rpi-cm5-cm5io.dtb`
  (sha256 f773b476d7bb55cd6361d778a34514e319c33cfc97605afe984fca7556fa4643). The other file on the partition,
  `bcm2712-rpi-cm5l-cm5io.dtb`, is 26cccbf4a74038199cd655e6c982b0a1c97f579bb2c9c647076f86b0baa87225.
  The image's `slot.json` gives its source as e1097ec7a1252a3d769d6015eee0e72c6df2ee63 (the image
  build), and the kernel as 7.2.9-v8-16k with no commit. The image does NOT state raspberrypi/linux
  53679a5, so whether the kernel matches it is not established from the board.
- Overlay gate: `lemnos-ctl tree` on the corrected reference file gives an overlay that `dtc` compiles
  and `fdtoverlay` applies to the copied base dtb (`bcm2712-rpi-cm5-cm5io.dtb`, copied to the PC, not
  committed). After applying: `pwm@9c000` has phandle 98 (0x62), status okay; the i2c-1 node is okay.

The goldens (`service/devtree/tests/golden/reference.dts`, `reference-boot.txt`,
`reference-modprobe.conf`) do not change: the overlay names the buses and PWM block but not the fan channel
or the magnetometer address, so regenerating them gives the same text (checked).

## Not run, and why
- Privilege-drop regression: not run this turn; the board was released before it.
- Full chain and the sensor's landing check: blocked by the one-open-per-node limit (above).
- Lit-scene checks: no light on the scene.
- Stock comparison of the back end: not run.
