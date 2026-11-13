# W1b: raw NAND first flash (untested on board)

Status: untested on board. The code is checked on a temp-file target (a stand-in partition with a
given erase size, page size, bad blocks and a fake sysfs tree) and on the driver's job path; no
real MTD device was touched.

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

- nandsim in the QEMU harness: not run. It needs `CONFIG_MTD_NAND_NANDSIM` (built in or a
  modules-on guest module) in `testing/qemu/kernel.config`, which changes the pinned kernel, so a
  separate kernel build is needed. Not done in this PR.
- A real UBI attach of the written partition: not run (needs the board or nandsim).
- `mark_bad` on a real chip: the ioctl path (`MEMSETBADBLOCK`) is compiled and its number is
  tested, but no chip has been marked.
