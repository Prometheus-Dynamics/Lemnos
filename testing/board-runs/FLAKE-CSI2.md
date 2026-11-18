# FLAKE-CSI2 board run: the receiver's line counter during STOP

Status: untested on board. Nothing here has been run on hardware. The QEMU fake's line period (50 us)
and the driver's window (at least 1.6 ms) are the only evidence so far, and the fake is ours.

What it checks: the assumption in kernel/rs/rp1_csi2.rs (`lrc_line_us`, the line time). While
the channel writes, `CSI2_CH_DEBUG` (low 16 bits: the line count) moves at least once per line time. A
stop waits for the counter to hold still for three line times (at least 1.6 ms), counted in reads
200 to 400 us apart, so a line longer than the window's share of that would let the stop return with a
line still due. The window is worked out at START from the stride and the link; this run checks the real
counter against it.

Run in a local session after this PR merges, on the CM5 (Raze #2) with an OV9782 on `cam0` (i2c-10,
1280x800 RAW10 over 2 lanes at 800 Mbit/s). Line time for that mode is 2000 bytes x 8 / (2 x 800) =
10 us. Take `/run/lock/board-gpu.lock`. Module loads, unbinds and rebinds are allowed; report each.
Build on NVMe or SSD, never on the Github HDD. Report CM5 timings only.

## Steps

1. Start the receiver with a frame stream (as in D5.md, "Getting the block") and let frames run.
2. From a small reader (not the driver), poll `CSI2_CH_DEBUG(0)` (offset `0x038`, channel 0 at
   `0x028` base for CTRL) every 5 us for 50 ms while the stream runs. Record the line count and the
   time of each change. Expect a change about every 10 us during a frame and none in vertical blanking.
3. Start a frame, then issue STOP. Record the time of FORCE (CSI2_CH_CTRL write) and the time of the
   last counter change before the counter stays still for the rest of the poll.
4. Pass: the last change after FORCE is no more than one line time (10 us here) after FORCE, and no
   change follows it. Fail: any change more than 1 line time after FORCE, or a counter that moves
   again after STOP returns.
5. Repeat with the slowest link and widest line the mode allows (for example a 4096 px RAW12 line on
   one lane at 80 Mbit/s, a 615 us line, a 1845 us window), if a sensor mode can be driven to it. Record
   the result as the window's margin.

Report: the sample times, the line count changes, the per-line period seen, and the STOP timings.
If step 4 fails, the window's factor (kernel/rs/rp1_csi2.rs) is wrong for this hardware: stop and
report before changing it.
