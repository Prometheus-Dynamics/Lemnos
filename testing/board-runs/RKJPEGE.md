# RKJPEGE: the encoder's read bound is board-blocked (K1b probe)

Status: partly done. The PC part is done: the job builder refuses a source plane that, from its
offset, reaches past its buffer for the encoded picture (the picture rounded up to the MCU). Commit
`baeac12a` (RKJPEGE) added the two tests in `drivers/rkjpege/tests/builder.rs`
(`a_chroma_plane_that_reaches_past_its_buffer_is_refused`,
`a_luma_plane_placed_past_the_start_of_its_buffer_is_checked_at_its_offset`), each failing with its
own term removed. What stays parked is the hardware reach and the ECS buffer, below. Untested on
board.

## What is missing

`drivers/rkjpege/src/job.rs` leaves the ECS length buffer unplaced, and it claims no reach for the
encoder beyond the encoded picture. Two hardware facts that decide the rest of the bound are not in
any source the repository or the vendor tree holds:

1. How far the encoder reads past the encoded picture (for example the frame's padding rows).
   The vendor HAL (`hal_jpege_vpu720.c`, in the K1b reference tree) programs the picture as
   MCU-aligned (`pic_wd8_m1`, `pic_hd8_m1` from `MPP_ALIGN(width|height, mcu)`), and places chroma
   at `hor_stride * ver_stride`, where `ver_stride` is the caller frame's stride (at least the
   encoded height). It does not state the block's read reach.
2. The extent of the ECS length buffer (`reg021_adr_ecs_len`) when `out_mode` is set. The vendor
   frame-by-frame path leaves the register unset.

A bound from a guessed margin would be an invented fact, so none is written.

## The probe

On the CM5 or the ROCK board with the K1b encoder, under the board-run lock:

- Encode one picture whose encoded size is not a multiple of 64 bytes per row in the source, with
  each source buffer filled with a sentinel and a guard region of 4096 bytes after the encoded
  picture (the plane's last row plus the stride). Read back the guard region and report which bytes
  the block changed (none is the expected answer if the bound is the picture).
- Repeat with `out_mode` set, placing `reg021_adr_ecs_len` in a dma-buf of known size and a guard
  after it. Report the largest offset written, in bytes, against the number of ECS segments.
- Record the kernel, the module build, the picture size and the sentinel scheme.

The result sets the two numbers in `job.rs`: the source extent past the picture, and the ECS
buffer size. Until then the job refuses nothing beyond what it refuses now.

## Owner

K1b board session (`testing/board-runs/K1b.md`, "Not checked here"). Not on the PC.

## The ZERO 3E (RK3566) cannot decide this (2026-11-18)

It has no VPU720-class encoder, so the K1b probe does not apply there. Still parked for the CM5 or
the ROCK board. Details: see [`BOARD-USB0.md`](BOARD-USB0.md).
