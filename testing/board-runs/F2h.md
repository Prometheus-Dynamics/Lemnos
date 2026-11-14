# F2h: the sensor's frame period, and the rate check

Decided: the board run of 2026-11-14 (`F2h3.md`) confirmed B1, `(N - 1) x HTS + G`, and the contract and driver now
state it. The text below is the reasoning and the step that led there, kept as it was written.

Not run on a board: untested on board. Everything below comes from files already on disk (the other team's
raw per-frame runs, read-only, under `h1-raw-run-3/rep/*.csv` with the JSON summaries beside them) and from the
datasheet and two reference drivers. No sensor register was read on a board.

## The measurement, recomputed from the per-frame CSVs

Six runs, 2000 frames each, 1280x800 RAW8 (sys clock 96 MHz, line time = HTS / 96 MHz): gated clock (HTS 765)
g60, g120, gmax; continuous clock (HTS 728) c60, c120, cmax. The frame length in lines is the one the driver
programs for the run; it is read from the run's own achieved rate (`achieved_backend_fps`), which is
`1e15 / (line time ps x lines)` on the contract's nominal line:

| run | clock | lines (VTS) | contract nominal (ns) | mean (ns), (last - first) / (N - 1) | excess (ns) | excess (lines) |
|---|---|---|---|---|---|---|
| gmax | gated | 910 | 7 251 562.5 | 7 258 825.1 | +7 262.6 | 0.911 |
| g120 | gated | 1046 | 8 335 312.5 | 8 342 584.5 | +7 272.0 | 0.913 |
| g60 | gated | 2092 | 16 670 625.0 | 16 677 972.1 | +7 347.1 | 0.922 |
| cmax | continuous | 910 | 6 900 833.3 | 6 908 477.8 | +7 644.5 | 1.008 |
| c120 | continuous | 1099 | 8 334 083.3 | 8 341 740.5 | +7 657.2 | 1.010 |
| c60 | continuous | 2198 | 16 668 166.7 | 16 675 898.0 | +7 731.3 | 1.020 |

Every run is one cluster (a 1 us spread around the mode); no frame is missing (ids are consecutive). The
excess is taken against lines x line time, not against 1e9 / (a rounded rate), so it is the same measurement as
the other team's table with a different nominal.

## What the datasheet says (OV9782 datasheet, v1.23, read in place; section numbers only)

- Section 2.8.1 (PLL, figure 2-11, table 2-9): the system clock comes from XVCLK through PLL2. The example for
  1 MP at 120 fps, 2-lane, 10-bit (SYS_CLK 80 MHz) is the reset register set: HTS 0x02D8 = 728, VTS 0x038E = 910.
- Table 7-8 (timing control): HTS at 0x380C/D ("Total Horizontal Timing Size"), VTS at 0x380E/F ("Total
  Vertical Timing Size"), both with no unit given in the table. Fractional VTS 0x3828/29 defaults to 0. Auto VTS
  0x382A[3] defaults to 0. The global-transfer HTS, `hts_global_tx`, is at 0x382C/D (default 0x05B0 = 1456), and
  it is used only when `r_tc_hts_blank` (0x382E[2]) is 1 ("Separate HTS for global transfer"). The default of
  0x382E is 0x01, so bit 2 is 0 and the table says "Uniform HTS".
- Table 5-3 and table 7-8 (exposure): maximum exposure is frame length - 25 rows, and the frame length is the
  VTS register. No formula for the frame period is given.
- Section 4.8.3 (tExp_Dly = 16388 x tXVCLK + 11 tRow) and the FSIN trigger timing apply only to trigger mode, not
  to free-running streaming.
- Table 2-1 notes: the formats have a minimum of four dummy lines and columns, and zero row overhead readout.

The datasheet gives no frame-period formula, no fixed per-frame term, and no +1 on VTS.

## What the reference driver does (mainline ov9282, and the Raspberry Pi tree's ov9282, read-only)

The mainline driver writes VTS (0x380E/F) as `vblank + height` and HTS (0x380C/D) as `(width + hblank) / 2`, and
takes its row time as `(width + hblank) / pixel rate` (`ov9282_exposure_to_us`). The Raspberry Pi tree's ov9282
writes VTS the same way (`lpfr = vblank + height`). Neither has a +1 or a fixed per-frame term, so the frame they
imply is VTS x row time, the same arithmetic as the contract. It does not explain the measurement.

## The line time is right; a per-frame term is missing

The mean is a telescoping difference, so the slope of the mean against VTS is the line time and the intercept
is the per-frame term:

- Gated: the three means (910, 1046, 2092 lines) lie on a line of slope 7 968.82 ns per line (nominal
  7 968.75, +9.0 ppm) and intercept 7 197.3 ns (0.903 gated lines, 690.9 sysclk).
- Continuous: the three means (910, 1099, 2198 lines) lie on a line of slope 7 583.40 ns (nominal 7 583.33,
  +8.9 ppm) and intercept 7 583.1 ns (1.000 continuous lines, 728.0 sysclk).

So the line time is right in both modes to about 9 ppm, and the excess is a constant per frame, not a fraction of
VTS. The two intercepts plus their line lengths are equal: 690.9 + 765 = 1 455.9 sysclk (gated) and 728.0 + 728 =
1 456.0 sysclk (continuous). A per-frame term that is the same in both modes, once the mode's own line is added, is
what gives "about one line in continuous, 0.90 of a line in gated".

## Candidates, tested against the six means

Residual = measured mean - model, in ns and in ppm of the model. The gated/continuous line times are the exact
values (765 and 728 sysclk at 96 MHz).

| model | gmax | g120 | g60 | cmax | c120 | c60 | verdict |
|---|---|---|---|---|---|---|---|
| M0: VTS x line (the contract) | +1 002 ppm | +872 | +441 | +1 108 | +919 | +464 | ruled out |
| A: (VTS + 1) x line | -706 ns (-97 ppm) | -697 ns (-84) | -622 ns (-37) | +61 ns (+9) | +74 ns (+9) | +148 ns (+9) | ruled out: fits continuous, misses gated by a systematic -0.09 gated line |
| B1: (VTS - 1) x HTS + 1456 sysclk, nominal clock | +9 | +9 | +9 | +9 | +9 | +9 | fits, with a common +9 ppm |
| B1 with the clock 9 ppm slow | -0.1 | -0.1 | -0.1 | -0.2 | -0.1 | -0.1 | fits all six to 0.2 ppm |

B1 is a per-frame line of 1456 sysclk in place of one HTS line. 1456 = 0x05B0, the datasheet default of
`hts_global_tx`. The service's own measured rate for each run (its `measured_fps_milli`, from the status frames,
not the reader) matches B1 and not the contract:

| run | service measured (milli-fps) | contract achieved | B1 predicted |
|---|---|---|---|
| g60 | 59 958 / 59 959 | 59 986 | 59 960 |
| g120 | 119 865 / 119 866 | 119 972 | 119 868 |
| gmax | 137 763 | 137 901 | 137 765 |
| c60 | 59 966 | 59 995 | 59 967 |
| c120 | 119 879 | 119 989 | 119 880 |
| cmax | 144 749 / 144 750 | 144 910 | 144 751 |

Ruled out:
- A pure clock error with no per-frame term (M0 times a ppm factor): the excess does not scale with VTS (it is
  +7.26 us at 910 lines and +7.35 us at 2092).
- A line time that differs by mode: the slopes match the nominal line time in both modes to 9 ppm.
- A constant receiver or stamp latency in ns: the offset differs by mode by exactly the HTS difference (385 ns),
  so the per-frame term is in the sensor's line units, not a fixed time.
- A constant offset in pixel clocks (200 MHz at RAW8) or a fixed count of sysclk per frame that is the same in
  both modes: the two modes' offsets differ by about 380 ns, so they are not the same count.

## Established or not

NOT established from the register definitions. B1 reproduces all six means and the service's own rates to about
0.2 ppm once a common clock term of 9 ppm is included (that term is fitted, from the same six means). Its constant,
1456 sysclk, is the sum of the two fitted intercepts and matches the datasheet default of a real register. But the datasheet says that register is used only when 0x382E[2] is 1, and that bit's default is 0
("uniform HTS"). So either the board's 0x382E[2] is 1 (not the default), or the extra line comes from something the
datasheet does not describe. The board readback below decides it. The 9 ppm is a clock term (the 24 MHz input's
error, which the PLL multiplies), to be read with a frequency counter on XVCLK.

The contract and the driver are unchanged. Their arithmetic (the line time, `readout_ps`, the rate for a
frame length) is right for the line. The frame period they imply is VTS x HTS; under B1 the measured period is
that plus 1456 - HTS sysclk per frame (691 sysclk gated, 728 continuous), which the contract does not model. The
rate check below fails on that, as it should until it is decided.

## The board step (for whoever holds the board; not run here)

Prerequisites: the board lock; the F2 camera board file in its gated-clock form (`link` to the receiver, the
`continuous_clock` config unset) for the gated runs, and in its continuous form (`continuous_clock = true`) for
the continuous runs, with `lemnosd` built from this PR. The lemnosd socket is the default. Mode 1 is 1280x800 RAW8 (`MODES[1]` in `drivers/ov9782/src/chip.rs`), the mode every number above
was taken in.

Register access. The sensor's I2C bus (i2c-10) is shared by every device on it (`docs/concepts.md`), so a
`raw-bus` device on the same bus can read the sensor's registers without any new method. Add two devices to
the board file for this session only (not to the product file). The read device has no `writes`, so it
cannot write; the write device is used once, in step 3:

```toml
[[devices]]
id = "cam0-regs-read"
driver = "raw-bus"
uses = { i2c = "i2c-10" }
config = { allow_i2c = "0x60" }

[[devices]]
id = "cam0-regs-write"
driver = "raw-bus"
uses = { i2c = "i2c-10" }
config = { writes = true, allow_i2c = "0x60" }
```

`i2c_transfer` takes the 7-bit address (96 = 0x60), the bytes to write (the 2-byte register address, then
the value for a write), the number of bytes to read, and the reply buffer size. A read is
`lemnos-ctl call cam0-regs-read i2c_transfer 96 <reg> <n> 16`. If the bind fails because the bus is held,
stop there and report the bind error: the shared-bus rule is what the readback relies on.

Step 1: readback, read-only.

```sh
for r in 382e 382c 382d 380c 380d 380e 380f 3828 3829 382a 0300 0301 0302 0303 0304 0305 0306 030b 030c 030d 030e 030f; do
  echo "$r: $(lemnos-ctl call cam0-regs-read i2c_transfer 96 $r 1 16)"
done | tee readback.txt
```

Bit 2 of 0x382E is `r_tc_hts_blank`: 1 means the global transfer uses `hts_global_tx`. Its value sets the
`--bit2` argument of the judge below.

Step 2: the frame length runs, each one with its own log. Gated first (`board-gated.toml`), then continuous
(`board-cc.toml`), each run stopping the stream when done:

```sh
for n in 2082 2091 2092 2093 2102; do
  lemnos-rate-proof --sensor cam0-sensor --mode 1 --asked-milli 60000 --lines $n --frames 5000 > gated-$n.log
done
# continuous clock: the same loop with cont- prefix and 2188 2197 2198 2199 2208
```

Each log's output includes `mean interval <ns> ns over <k> intervals at <N> lines, line time <ps> ps`. The
`rate proof: FAILED` line is expected unless B1 holds: the judge, not that line, decides.

Expected for each candidate (the judge prints the same numbers):

| candidate | gated intercept | continuous intercept | slope | residual per run |
|---|---|---|---|---|
| B1: (N - 1) x HTS + 1456 sysclk | 691 sysclk = 7 197.9 ns | 728 sysclk = 7 583.3 ns | HTS x T (7 968.75 / 7 583.33 ns) | about +9 ppm (one common clock term) |
| A: (N + 1) x HTS | 765 sysclk = 7 968.75 ns | 728 sysclk = 7 583.3 ns | HTS x T | gated -97 to -37 ppm |
| M0: N x HTS (the contract) | 0 | 0 | HTS x T | +440 to +1 000 ppm |

The continuous intercepts of B1 and A coincide (1456 = 2 x 728), so only the gated runs separate them.

Step 3: the one write. The tool stops the stream after each run, so the write happens in standby. Before it:
`cp gated-2092.log write-before.log`. Then write 0x382C/D to 0x0600 (1536), run the same frame once more, and
restore the reset values:

```sh
lemnos-ctl call cam0-regs-write i2c_transfer 96 382c06 0 16    # 0x382C = 0x06
lemnos-ctl call cam0-regs-write i2c_transfer 96 382d00 0 16    # 0x382D = 0x00: hts_global_tx = 1536
lemnos-rate-proof --sensor cam0-sensor --mode 1 --asked-milli 60000 --lines 2092 --frames 5000 > write-after.log
lemnos-ctl call cam0-regs-write i2c_transfer 96 382c05 0 16    # restore 0x382C = 0x05
lemnos-ctl call cam0-regs-write i2c_transfer 96 382db0 0 16    # restore 0x382D = 0xB0 (1456)
```

Under B1 (with bit 2 = 1) the mean moves by +833 ns (80 sysclk, one global line). Under A or M0 it does not
move. If bit 2 reads 0, the datasheet says the global line is not used, so the move should be 0: a move of
+833 ns then means the default is not what the datasheet says, and the judge's `--bit2 0` check fails it.

Step 4: the judge, read-only, on this directory's logs:

```sh
python3 testing/board-runs/F2h-judge.py <logdir> --bit2 <bit 2 of 0x382E: 0 or 1>
```

It prints `PASS` or `FAIL` per check, and exits 0 only when every check passes: the gated intercept matches
B1 and nothing else (within 150 ns), the continuous intercept matches A or B1, the write moved the gated mean
by +833 ns within 100 ns, and bit 2 is 1. Only then is B1 the mechanism. The judge was checked on synthetic logs
made from B1 (all checks pass) and from A (the gated check fails).

Step 5, read-only: a frequency counter on XVCLK (the 24 MHz input, pin 38 of the sensor). A 9 ppm error there
is the common clock term in every residual above. Without it, the 9 ppm stays a measured term, not a fact.

## The check (in the tool)

`lemnos-rate-proof --asked-milli 60000` (and 120000) takes the frame length nearest the asked rate on the nominal
line, runs at least 5000 frames, and compares the measured rate from the timestamps with the nominal rate. It fails
beyond 200 ppm (0.02 %), on a dropped frame or a gap, or on fewer than 5000 frames. It prints the per-frame
histogram, the run lengths of the short/long sequence, the autocorrelation at lags 1 to 8, and the first 200
intervals. `--lines N` sets the frame length instead (within the mode's range), for step 2 above. Expect a rate
difference of about 1 000 ppm at 910 lines and about 440 ppm at 2092 lines (the table above) until the cause is
decided: the check fails on that, which is the open finding, not a pass.

## Frame end: what the receiver gives

The receiver module (`kernel/lemnos_rp1_csi2.c`) takes the frame-end acknowledge interrupt (`IRQ_FE_ACK`) and reads
`ktime_get_ns()` in its interrupt handler (`lrc_isr`). A frame-end time is available to the module. An end stamp on
the ring entry needs a new field in `FrameSlot` (`engine/wire`): a wire change, not built here.

## What the sensor contract reports

The contract reports the nominal values (line time, `readout_ps`, `fps_milli_of`, `frame_length_for_fps_milli`).
Nothing is fitted. The measured frame period is about 0.04 % to 0.11 % above the contract's nominal, and the
candidate B1 is recorded above with its evidence; it is not in the contract until the board readback decides it.
