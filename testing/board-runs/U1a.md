# U1a board run: the PiSP back end's missing blocks

Not run on the board yet. Untested on board.

What changed since D17: the back end's YCbCr conversion (`ycbcr`, `ycbcr_inverse`), the output
colour space conversions (`csc`), debin, demosaic, false colour, sharpening, saturation control,
tone mapping, chromatic aberration (CAC), input decompression and the HOG enable have builders with
the libpisp defaults (`drivers/pisp/src/be/defaults.rs`). A Bayer-to-NV12 job built with
`BackEnd::simple_bayer` and no matrix from the caller now carries full-range BT.601 on output 0;
before, its colours were wrong.

## Setup

As in D17: the board lock, the CM5, builds on NVMe or SSD. Run `lemnos-isp-proof` with the colour
bars frame that the earlier runs used (`--input`), and `--save` to keep the first output.

## Checks

1. Default NV12 colours. Build the configuration with `simple_bayer(..., NV12, None)` (no matrix
   from the caller). Expect the eight colour bars' luma as before and Cb, Cr by eighths within 2 of
   the values the reference frame gives with the matrix passed explicitly
   (`Encoding::Jpeg.forward()`). Expected: equal. Record the Cb and Cr values in this file.
2. Full-range BT.601 against limited range. Set `Encoding::Smpte170m` on output 0 with
   `set_csc_encoding`. Expect black bars to sit at 16 and white at 235 in luma (within 2).
3. YCbCr round trip. Enable `set_ycbcr_encoding(Some(Encoding::Jpeg))` with CSC off and output 0
   as RGB888. Expect the bars to come back at the input's values within 2.
4. Demosaic defaults. Enable `set_demosaic(Some(DEMOSAIC_DEFAULT))` (the default `simple_bayer`
   already does). Expect no error from the block and the same luma as check 1 within 4.
5. Debin. Enable `set_debin(Some(DEBIN_DEFAULT))` with an output of half the input's size.
   Expect the output to be the input's average within 2 per pixel of a software average of 2x2
   blocks (the reference the FE proof uses for its halving).
6. Sharpen. Enable `set_sharpen(Some(SHARPEN_DEFAULT))`. Expect no error and the edges of the bars
   to overshoot by a few levels; no other change to the flat areas.
7. False colour. Enable `set_false_colour(Some(FALSE_COLOUR_DEFAULT))`. Expect no error and the
   flat areas unchanged.
8. Saturation control, tone mapping, CAC. Each enabled with a neutral table (zero shifts, a linear
   tone map, a zero CAC table) on its own. Expect no error, no dropped job, and the luma within 2.
   A tone map of all zeros is refused before any job is sent.
9. Input decompression. A compressed (8-bit, mode 1) input with `set_input_decompress(Some(..))`
   runs; without it the configuration is refused before a job is sent.
10. HOG. Enabled, it is refused before any job is sent.

Each check: the job completes (`counters` shows `jobs` up and `dropped` and `errors` unchanged),
and the figure named above. A check that the block stalls is a stop: do not post more jobs on the
back end until it is rebooted (the back end has no reset from user space).

## Results

(none yet)
