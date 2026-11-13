# F2h: the sensor's frame period, and the rate check

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

## The first board step (decides between the candidates; read-only except the sensor's own run)

1. Read back over SCCB (read-only): 0x382E (bit 2 = `r_tc_hts_blank`), 0x382C and 0x382D (`hts_global_tx`), 0x380C
   to 0x380F (HTS, VTS), 0x3828 and 0x3829 (fractional VTS), 0x382A (`vts_auto_en`), and the PLL registers
   0x0300 to 0x0306 and 0x030B to 0x030F. This shows whether bit 2 is set on the board and the actual sysclk.
2. Set VTS by `lemnos-rate-proof --asked-milli 60000 --lines N` at N = 2092 - 10, 2092 - 1, 2092, 2092 + 1, 2092 + 10
   (gated), and at N = 2198 - 10, 2198 - 1, 2198, 2198 + 1, 2198 + 10 (continuous). Five means per mode give the slope
   (= line time: 7 968.75 and 7 583.33 ns, to 9 ppm) and the intercept directly. An intercept of 1456 sysclk less
   HTS (691 gated, 728 continuous) is B1; an intercept of one line (765 gated, 728 continuous) is A; 0 is M0.
3. If bit 2 is 1 on the board: change 0x382C/D to 1456 + 80 (0x0600) with `r_tc_hts_blank` set, and see the
   intercept move by 80 sysclk (833 ns) per frame. If it does not move, B1 is not the mechanism.
4. Clock: a frequency counter on XVCLK (the 24 MHz input). A 9 ppm error there confirms the common slope.

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
