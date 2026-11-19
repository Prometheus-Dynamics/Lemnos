# FLASH-EXTENT: change to the K230 card flash job (for the external host's owner)

Branch `pr/FLASH-EXTENT` (on top of `pr/CARD-STAGE-BOUNDS`, b40dea13). Held: not to be landed until the host is ready.

## What changes in the job

Every piece of a card flash (`k230-card piece`, `k230-card flash`, and any host that loads the stage in `piece` mode)
now declares the flash's **extent**: the sectors the whole run may touch. It is carried in the job header:

- word 13: the extent's first sector (u32);
- word 14: the extent's sector count (u32);
- word 6, bit 1 (`EXTENT_FLAG`, value `0x2`): set when words 13 and 14 are declared. Bit 0 (FAST) is unchanged.

The header is still sixteen little-endian words, and word 15 is still the CRC-32 of words 0 to 14. Nothing else in the
header moved. The header is 64 bytes at the job address `0x80380000`, as before.

The stage keeps nothing between pieces: each piece is its own boot-ROM load and the stage resets after it. So the
declaration is repeated in every job. Send the same extent in every piece of one run. The extent is the whole image's
range (for a whole-card flash: first sector 0, count = image sectors), and each piece must lie inside it.

Rules the stage now applies, in this order, before any card write:

1. no declared extent (flag bit 1 clear) is refused;
2. an extent with no sectors is refused;
3. an extent past the card's size in the header (`card blocks`) is refused, before the card is touched;
4. a write, zero or write-test piece that is not wholly inside the extent is refused whole (a straddling piece too);
5. the extent past the card's own size (read from the card's CSD) is refused after the card's identification reads, before any write.

A hash piece reads and writes nothing, so it is not held to the extent. It still needs a declaration.

Extent numbers are 32 bits, so an extent names sectors below 2^32 (2 TiB). A larger card cannot be declared whole, and
its job is refused.

## The new refusal statuses

The stage prints the refusal as a console line, then `lemnos stage: end`, then resets to the ROM. Exact texts:

- `piece refused: no declared extent`
- `piece refused: the declared extent is empty`
- `piece refused: the declared extent runs past the card`
- `piece refused: the piece is not wholly inside the declared extent`

The existing refusals are unchanged. A refused piece writes nothing, and the ROM comes back as `K230`.

What is NOT applied on the flash path: the bench's protected-sector list (sectors 0 to 33, 2048, 3072, the backup GPT,
the last 33 sectors) and the bench's declared scratch range. A real flash writes the GPT and the boot headers, so the
flash path is bounded by its extent only. The bench's bounds are a different type in the stage crate
(`bounds::Declared`), and the flash extent is `extent::Extent`; neither can be given to the other's check.

## What an old host sees against the new stage

An old host sends flag bit 1 clear and words 13 and 14 zero. The new stage refuses it with
`piece refused: no declared extent`, prints `lemnos stage: end`, and resets. It writes nothing. The old host's line
reader counts a line containing `refused` as trouble, so it stops with `the stage reported: [...]`, and does not
write the next piece. The ROM still comes back. So an old host fails clearly against the new stage; it does not write.

A new host against an old stage: the old stage ignores words 13 and 14 and the flag, and runs the piece as before, with
no extent check. Do not run a new host against an old stage image for a flash.

## The image

- Load address `0x80360000` and job address `0x80380000`: unchanged.
- The image's file size: 66,256 bytes (was 65,376; +880, the extent checks and their refusal texts).
- The footprint's end: `0x80374300` (was `0x8037ff90`), so 23,808 bytes are spare below `0x80380000` (was 112).
- The one cut that made it fit: the stack reserve in `drivers/k230-usbboot-image/image.ld` goes from 64 KiB to 16 KiB.
  The deepest mode's frames, summed down the image's direct-call graph, are 3.5 KiB (the card write piece); the report
  is 1 KiB and the probe 0.7 KiB. The image's indirect calls are not counted, and they are small formatting calls.
  Nothing else was removed. The probe, report and stage modes are all still in the image.

## Host side in this repository

`k230-card piece` takes `--extent FIRST,COUNT` (required). `k230-card flash` declares the sectors `--from` to `--to`
in every piece. A host that wants to drive `piece` directly needs the same field.

## Tests (for the record)

- `testing/crates/k230-usbboot-stage/tests/flash_extent.rs` (six tests): on the code before this change, five refused
  cases were accepted (`a_piece_with_no_declared_extent_is_refused_and_writes_nothing`,
  `a_piece_wholly_or_partly_outside_the_declared_extent_is_refused_whole`,
  `the_write_test_outside_the_declared_extent_is_refused_and_writes_nothing`,
  `an_extent_that_runs_past_the_card_end_is_refused_before_any_write`, `an_empty_extent_is_refused`); the in-extent whole-image
  flash passed both before and after (`a_whole_image_flash_inside_its_declared_extent_passes_and_reads_back_equal`).
- The same six are green after the change.
