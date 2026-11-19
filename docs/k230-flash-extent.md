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
- File size: 67,996 bytes. The original card image was 65,376; this step's code (the extent checks, the guard,
  the paint and the high-water line) adds the rest.
- The image's own end (the statics) is `0x803709d0`, so 63,024 bytes are spare below `0x80380000`. The stack and its
  guard band are below the load address, not in that spare (see "The stack" above).
- The stack reserve is 16 KiB, moved below the code and not cut from the image. It was 64 KiB in the image before the
  guard. The deepest mode's frames, summed down the image's direct-call graph, are an estimate of 3.5 KiB (the card
  write piece; the report 1 KiB, the probe 0.7 KiB). The measured figure is the `stack:` line, not this estimate.
- The probe, report and stage modes are all still in the image: nothing was removed.

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

## The stack: layout, guard, high-water mark

**Layout, from the linked image (`check.sh` asserts the order on every build).** The stack is below the code:

| Region | Before (004f374c + paint, ec1714dd) | After (this change) |
|---|---|---|
| code | `0x80360000`..`0x8036f40a` | `0x80360000`..`0x8036f128` |
| read-only data | `0x8036f410`..`0x80370cd0` | `0x8036f130`..`0x8036f99c` |
| statics (`.bss`) | `0x80370cd0`..`0x80370d00` | `0x803709a0`..`0x803709d0` |
| stack reserve | `0x80370d00`..`0x80374d00` (16 KiB, above the statics) | `0x8035c000`..`0x80360000` (16 KiB, below the code) |
| guard band | none | `0x8035bf00`..`0x8035c000` (256 bytes of canary) |
| below the guard | the statics and read-only data | `0x80300000`..`0x8035bf00`: no image byte |

Before: a stack that grew down out of its reserve met 48 bytes of statics, then 6.3 KB of read-only data, then the code.
After: it meets the guard band, then RAM below the load address that the image never uses. It cannot reach the code,
the read-only data, the statics, or the job area at `0x80380000` (the stack grows down from `0x80360000`).

**The guard.** The entry paints the guard band with `0x5ca1ab1e` and the reserve with `0xa5a5a5a5`, before the first
call. The guard is checked at every end that prints, at every verified sector of a piece, and immediately before every
card write command (`CMD24`, in the SD host, before the command is sent). A changed word prints `stack overflow`, and
the SoC resets at once; the run does not go on.

**Limit of the guard, stated plainly.** The guard is a check, not a barrier. A write is protected only if no overflow
happens between the last check and the write. The window is the code that runs between those two points. A hardware
guard would close it; see the PMP note below.

**PMP (not used).** A PMP region below the stack would fault an overflow at the store itself. Whether the K230's C908
implements PMP, and in which mode the boot ROM hands over, are not on this machine: there is no vendor SDK or datasheet
in the repository or on disk, so nothing can be cited. The board run that comes first can read `pmpcfg0` and `pmpaddr0`
in M-mode (the trap capture in `testing/board-runs/K230-USB.md` shows the stage in machine mode). Until that is read,
no PMP is configured.

**The high-water mark.** Each end prints `stack: <used> of 16384 bytes`: `used` is from the top of the reserve down to
the lowest word a run has written. The host records every console line, so this line is the figure. The job's reply
has no spare word for it.

**Measured.** No figure yet. The real image reads and writes the K230's registers and cannot run off-board; the QEMU in
this environment has no model of them. The paint, guard and scan logic is tested on the host
(`lemnos_k230_usbboot_stage::stack`, and `the_stack_guard_runs_before_every_write_command_and_never_before_a_read`).
The static estimate is 3.5 KiB for the deepest mode (the card write piece), counting direct calls only.

**Rule for the record:** a figure above 50% of the reserve (above 8192 bytes of 16384) means the reserve must be raised,
and a size cut made elsewhere to keep the image below `0x80380000`. The first board use of this image records the
`stack:` line from a whole-image flash with refusals and a hash, under `testing/board-runs/`.
