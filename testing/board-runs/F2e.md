# F2e board run: the raw camera path, as root and as the lemnos user

Run on the board 2026-11-13: Raspberry Pi Compute Module 5 Rev 1.0 (Raze #2), kernel `7.2.9-v8-16k`.
The board's product services were stopped for the run and put back afterwards (six services active,
the stock capture driver, sensor and back end bound again, `lemnos` unloaded, heap permissions
restored). Both board locks were held from 01:22 to 01:28 (board clock) and released.

## What ran, and at which commit

Every number here was measured with the binaries of commit `0aa30510` (the raw path set kept on
the board at `/tmp/lemnos-run/raw-0aa30510/`). Their hashes were checked before and after the run:

| File | sha256 |
|---|---|
| `lemnosd` | `458f9df7308087cdb9a004ab3b39f57727c3622d3a56fbce02e4e16aa44e32d0` |
| `lemnos.ko` | `15e862ac8f0b723466ef782805c7f69b3c9d4faaf0b2e6b523cd6c671e717161` |
| `lemnos-camera-proof` | `c999d8e609cd465f5fb7a556dd7a150067644e8c0787916ab18001bce1068656` |
| `raze-raw-board.toml` | `fe5fe199ffdb3e4baa4949b9b37a9d87d53bbacff21ba32bdaed605533a077af` |

The run did not use a newer build. The `lemnos-sensor-proof` binary (for the mode trace) is from
the F2d set, sha256 `e6b0f35dd37a43b386df740148c6b35c86dc5ed0a0b31769bc6258eb51be1f98`.

## Check 1: the privilege drop on the raw path (CONFIRMED)

The same 9000-frame command, `lemnos-camera-proof --sensor cam0-sensor --mode 0 --pattern 1
--frame-length 910 --frames 9000`, run once with `lemnosd` as root and once with
`lemnosd --user lemnos`:

| | As root | As `lemnos` (uid 900) |
|---|---|---|
| Frames | 9000 | 9000 |
| Frame rate from timestamps | 114.802 fps | 114.802 fps |
| Sequence gaps / receiver `dropped` / `errors` | 0 / 0 / 0 | 0 / 0 / 0 |
| Frame start intervals, median / max | 8710 / 8715 µs | 8710 / 8718 µs |
| Timestamp to client, median / p99 / max | 8492 / 9013 / 12722 µs | 8492 / 9014 / 9612 µs |
| Content | 0 all-zero frames; the bars are identical in every frame (pattern 1) | the same |

The daemon as `lemnos` showed `Uid` and `Gid` 900, `CapPrm`, `CapEff` and `CapBnd` 0000000000020000
(`CAP_SYS_RAWIO` only) and `NoNewPrivs` 1.

The privilege drop needed the three nodes below to be readable and writable by group `lemnos`. They
were changed for the run and put back afterwards (`root:root`, mode 600 each; the module's two nodes
go away on `rmmod`, and the heap is restored by hand):

- `/dev/lemnos` and `/dev/lemnos-rp1-csi2-1f00110000.csi`: `chgrp lemnos`, `chmod 660`
- `/dev/dma_heap/linux,cma`: `chgrp lemnos`, `chmod 660`, then `chgrp root`, `chmod 600`

The sensor (`/dev/i2c-10`, group `i2c`) and the power switch (`/dev/gpiochip15`, group `gpio`) worked as
`lemnos` through the supplementary groups already on the board.

The record of the "before" owners was not written (`stat` is not on the board); the owners above are
those the runbook records.

## Check 2: the sensor's modes (PARTLY RUN)

`lemnos-sensor-proof trace --bus i2c-10` (the sensor on the bus, no daemon) selects modes 0 to 5,
starts, posts 30 value sets, sets flips and test patterns (0 to 7 accepted, 8 refused with `-22`),
and stops. All returned `Ok` except test pattern 8. 338 register writes to 104 registers.

The trace does not print a mode's geometry, so **the stream sizes each mode offers are NOT checked
on the board** by this run. The earlier run's `sensor-info-modes.txt` lists modes 0 and 1 only.

## Check 3: the LED ring's pixel order (NOT ESTABLISHED)

The stock `ws2812-pio-rp1` driver writes the caller's bytes into a buffer of 4 bytes per pixel
(`W, B, R, G` in memory). The PIO shifts the top 24 bits out, so the wire order is G, R, B (source
reading of `drivers/misc/ws2812-pio-rp1.c` in the board's kernel tree; not measured).

Test: all LEDs off, then pixel 0 red (`00 00 ff 00`), then green (`00 00 00 ff`), then all off. Each
capture used a fresh daemon and 30 frames of the scene pattern. The frames show a light at the
same place for both colours: block y 128 to 160, x 384 to 416 rises by about 29 in each (dark mean in
that block 83). Inside that block, the four Bayer sites rise by 29.0, 30.5, 34.5, 37.2 for red and
29.2, 28.5, 36.3, 36.2 for green. The two colours do not separate, so the colour order is not
decided. The pixel count (16 by Q2h) is not checked here.

The LEDs were left off. The frames are in `board-2026-11-13/led-f2e/` on the PC.

## Failure found: a client that stops with an error leaves the sensor started

The first LED attempt used one daemon for three captures. The first capture stopped with an error
after streaming had started (its `--save` directory did not exist: `No such file or directory`).
The next capture on the same daemon failed at `sensor.select_mode failed: Status(-16)` (EBUSY), so the
sensor was still started after the client's error. The daemon was stopped by PID. Not yet fixed; it is
the same state as the receiver-unbind failure in the F2d record. Fix: the sensor goes to Stopped when
its stream ends, however it ends, and a daemon that finds the sensor started at probe copes with it.

## Restore and state

- Stopped by PID: the daemons started for the run (`lemnosd` as root, `lemnosd --user lemnos`, the
  LED captures). The holder that kept the locks was released with its release file.
- `rmmod lemnos` ran; the stock `rp1-cfe`, `ov9282` on `10-0060`, `reg-fixed-voltage` on `cam0_reg` and
  `pispbe` are bound again. The `bind` of `10-0060` answered busy (the sensor's own probe had bound it);
  this is correct.
- `driver_override` is empty on both blocks. `/dev/dma_heap/linux,cma` is `root:root` mode 600.
- The six services are active. The stock receiver's interrupts were 474 per second after the restore.
- Both locks are free. `/tmp/lemnos-run/raw-0aa30510/` still verifies against its checksums.

## Not run

- The sensor's stream sizes per mode (above). The lit-scene exposure and gain checks (the scene is dark).
- The front end and back end with the links, the full chain, the receiver and back-end unbind cycles,
  the sensor's start timing, and the 640x400 resample: these are in the F2d record and were not repeated.
- The LED ring's pixel count and wire order as the product uses them.
