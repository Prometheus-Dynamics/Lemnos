# W1b: raw NAND first flash (untested on board)

Status: untested on board. The code is checked on a temp-file target and on the driver's job path,
and in QEMU against the kernel's own NAND simulator, MTD and UBI (see "QEMU", below). No real MTD
device was touched.

## QEMU: the kernel's NAND simulator, MTD and UBI

`testing/qemu/run.sh` loads `nandsim` with 128 MiB of 128 KiB erase blocks and 2 KiB pages, six
partitions, injected bad blocks, one weak erase block and bit flips (`testing/qemu/init.c`). Then
`lemnos-nand-check` (`testing/crates/nand-check`) runs the driver's NAND writer (the code an `mtd`
target's job runs) against that chip, and checks the result with the kernel's UBI and MTD:

- Geometry: sysfs and `MEMGETINFO` agree; `MEMGETBADBLOCK` returns exactly the injected bad blocks.
- UBI on partition A: the kernel attaches and formats the blank chip, a volume is written through
  its node and read back, and A's good erase blocks are read through MTD into an image.
- The driver writes that image to partition B (two bad blocks). The driver's own verifier reads it
  back. Each good block's erase count is the count it held plus one. The bad blocks are untouched.
  The kernel attaches B and the volume reads back the bytes written to A.
- Refusal: an image that needs more good blocks than partition D has (61 good for 62 needed) is
  refused before any erase; D's bytes are unchanged.
- Raw: a raw image that is not whole erase blocks is written to E and reads back; `MEMERASE` on a
  good block leaves it all 0xFF.
- ECC: the injected flips are corrected and counted; the driver's count equals the kernel's.
- `mark_bad`: a weak erase block fails the write; with `mark_bad` off the block stays good, with it
  on `MEMSETBADBLOCK` marks it and the kernel reports it bad.
- Negative run (`lemnos-nand-check --corrupt`, one changed byte in every image block): the kernel
  refuses to attach B ("both volume tables are corrupted"), and the check fails.

Found and fixed by this run: `MEMSETBADBLOCK` was defined as ioctl 14 (the kernel's is 12), so the
driver's `mark_bad` could never succeed; the unit test pinned the wrong value. And the kernel's
character device returns the data of an uncorrectable read and counts it in `failed`, which the
driver did not look at; a read the ECC could not correct is now an error (`EBADMSG`).

What QEMU does not prove: the ECC's behaviour on real NAND (the simulator's bit flips are one per
page at most, and the uncorrectable path is covered by a unit test of the counter mapping, not by a
guest run); real bad-block tables; timing; the kernel's UBI on a chip with real wear.

## What to run on a board (a spare NAND partition, never the running root)

1. Build a UBI image for the partition's page and sub-page size and the same erase block (`ubinize`
   or the kernel's documented layout), and sign it or leave it unsigned.
2. Board file: `kind = "mtd"`, `path = "/dev/mtdN"`, `image = "ubi"`, `allow_first_flash = true` on a
   flasher image only. Start the flasher, stream the image through the bridge.
3. Check `info`: `erase_size`, `bad_blocks`, `needed` and `available`, then `state` `written`, then
   `verify` to `verified`.
4. Attach the partition with the kernel's UBI attach (`/dev/ubi_ctrl`, `UBI_IOCATT`) and read each
   volume back. Expected: attach succeeds, and the volumes read the image's data.
5. Refusals: an image for another page size fails before any erase (`info` names both offsets);
   a partition with a volume attached from the running root is refused.

Record the CM5 timings and the board's `corrected_bits` separately from this check.

## Not run

- A real NAND chip: none of the above has run on one. Real ECC behaviour, the bad-block table a chip
  ships with, and timing are still open.
- The uncorrectable-read path in the guest: the driver's mapping of the ECC counters is unit-tested;
  no guest run injected an uncorrectable page.
