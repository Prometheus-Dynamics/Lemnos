# U2b board run: K230 ISP lens shading

Status: untested on board.

The lens shading load (`drivers/k230isp/src/lens.rs`, `lens_shading_writes`) writes 634 registers
through the configuration session. The sources fix the register addresses, field masks, the bank
addresses (0 and 153), the sample packing and the write order. They do not fix two things, and the
board must show them:

1. The mapping of the 17 x 17 samples to the image. The SDK's tool and rkisp1 both take 17 x 17
   tables with 8 sector sizes and gradients per axis, but neither states where each sample sits
   in the frame. Styx's K230 builder (`crates/k230isp/src/params.rs`, `Params::lens_shading`)
   resamples to 9 vertices per axis into a 17 x 18 table, and calls its own layout a guess.
2. Whether the last sample of each row (word 8) is ignored. rkisp1 writes it with a 0 upper half;
   Styx writes an 18th sample there.

Check on the reference board, with the lock named in `AGENTS.md`:

- Load a flat table (every sample the same register value) and read `LSC_STATUS` (at
  `LSC_BASE + 0x68`) before and after a `TABLE_SEL` change to see the active bank.
- Load a table with one sample raised, on bank 0 and bank 1 separately, and see which frame
  pixels change. The change tells the mapping of sample to sector.
- Load a table where only word 8 of each row is non-zero. If the frame changes, the last sample
  is used.
- Count the writes: 4 address pointers, 4 x 153 data words, 8 sector sizes and 8 gradients packed
  two to a register, the select and the control: 634.

Record the mapping, the answer on word 8, and the bank readback in this file.
