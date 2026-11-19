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

**PMP (read, through a trap vector of our own).** The entry reads `pmpcfg0` and `pmpaddr0` and prints either
`pmp: cfg0=0x.. addr0=0x..` or `pmp: trapped mcause=0x.. (no PMP)`. A read of a PMP CSR on a core without PMP is an
illegal-instruction trap, and the boot ROM's trap handler is not known (its one observed trap printed
`CPU Exception: NO.2` and the board did not answer identify again; testing/board-runs/K230-USB.md). So the reads run in
a window of their own (`drivers/k230-usbboot-sys/src/pmp.rs`):
1. `mstatus.MIE` is saved and cleared, so no interrupt can reach the vector;
2. `mtvec` is saved and pointed at `lemnos_pmp_handler`, which records the first `mcause`, steps `mepc` past the
   4-byte read, and returns;
3. the two reads run; 4. `mtvec` is restored, then `mstatus.MIE`.
Nothing else runs in the window: no stack, no card or USB I/O. The handler touches only `t0` to `t3`.

The mode: the window writes `mtvec` and `mstatus`, which only machine mode may do. No instruction tells machine mode
from supervisor mode without trapping, so the entry relies on the handover being machine mode (the trap capture's mode
in testing/board-runs/K230-USB.md). That is a contract, not a check.

**Rehearsal (qemu-system-riscv64 10.2.2, virt, rv64, from reset in machine mode).** `drivers/k230-usbboot-image/pmp-rehearsal/run.sh`
builds the probe's own code into a bare-metal program and runs four cases; each case also prints `mtvec` before and
after the probe, and passes only if they are equal (the harness sets its own vector first, so the equality is a test):
- `pmp-true`: `pmp: cfg0=0x00000000 addr0=0x00000000`, `mtvec` restored, `identify: K230`. PASS.
- `pmp-false`: `pmp: trapped mcause=0x00000002 (no PMP)`, `mtvec` restored, `identify: K230`. PASS.
- `fail-naive` (the reads with no vector, the fail-before): no identify; the run times out. CAUGHT.
- `fail-broken` (the vector without the `mepc` step): the trapping read runs again; no identify. CAUGHT.

**Status: rehearsed on qemu only. Untested on the board.** The board's trap behaviour, the ROM's handler and the
machine mode at the entry are not exercised by the rehearsal.

**Below the guard (the RAM under the image).** The entry paints `0x80340000`..`0x8035bf00` (113,408 bytes, from 128 KiB below the load address up to the
guard) with `0x3c3c3c3c`. Each end scans it and prints `below-guard: unchanged`, or
`below-guard: changed at 0x<addr>, <count> words` (the first changed word, and the words changed). The span is painted
because the image's own stack, code and data do not reach it, so any change there is another agent's write. The span's
bottom is a guess at the largest block that the entry can paint quickly; `0x80340000` is not a number from a source.

What the sources say about that RAM (SDK facts, file:line in `git show <rev>:<path>`, the Linux SDK at `fc6cc21`, the U-Boot
overlay under `buildroot-overlay/boot/uboot/u-boot-2022.10-overlay`):
- `arch/riscv/cpu/k230/platform.h:47-54`: `USER_ALLOC_ADDR 0x80230000`, `PATCH_CODE_ADDR 0x80250000`,
  `FIRMWARE_CODE_ADDR 0x80300000`, `FIRMWARE_CODE_MAX_LEN 512 KiB` (so the firmware window ends at `0x80380000`),
  `SRAM_START_ADDRESS 0x80000000`.
- `configs/k230_canmv_defconfig:9`: `CONFIG_SPL_TEXT_BASE=0x80300000`; `:43`: `CONFIG_SPL_BSS_START_ADDR=0x80380000`.
  So the SPL's own code and data occupy the window from `0x80300000` (the SPL is not running in our download mode).
- `arch/riscv/cpu/k230/unzip.c:37-41` and `:145-150`: the U-Boot SDMA ping-pong buffers are in SRAM at `0x80200000`
  (write side, four 128 KiB slots, to `0x80280000`) and `0x80280000` (read side, two slots, to `0x80300000`). These
  are U-Boot's, not the boot ROM's, and they end below the span.
- The k230_sdk clone at `7e302f7` contains no U-Boot, SPL or boot ROM (only `src/big/mpp` and `src/common/cdk`).
  The K230-USB record's `src/little/uboot` paths are not in that clone; its U-Boot is the Linux SDK's overlay above.
- The record (`testing/board-runs/K230-USB.md:1383`): GAHBCFG read `0` as the ROM left it, so the USB core was in slave
  mode at handover: the ROM's USB path was not using DMA on that run.

**UNKNOWN:** the boot ROM's stack location and size, the addresses of its USB buffers, and whether the ROM or its USB
path writes anywhere below the guard after handover. No source and no board record gives those. The span scan is the
measurement that answers the last question on the board.

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
