# GROW: the data partition grows on a first boot

Status: **untested on board.** Nothing here has run on a board. The QEMU checks in
`testing/qemu/run-grow-live.py` (MBR and GPT) are the only evidence so far, and they are NVMe-backed QEMU.

## What the board check must show

A card or eMMC built with the image builder's data partition (4 KiB blocks, `bytes_per_inode` 16384, the
filesystem grown online through meta_bg) is first booted with `grow_data` on (the default) and:

1. the first boot grows partition 2 to the end of the card (MBR or GPT, whichever the card carries), with one
   `grow-init`-style line of `info`: `grow_state` goes `pending`, `resizing`, then `none`;
2. `e2fsck -fn` run on the partition after the boot (a clean power-off first) exits 0;
3. a second boot finds nothing to do: `grow_state` `none`, no baseline record left under `board/slots/grow`;
4. the probe on the watchdog keeps its cadence through the resize (no watchdog reset, the tick never blocks).

Power-cut point: a power cut mid-resize on the card is the one check the QEMU run cannot make on a real
device's write behaviour. The GPT order is safe only if the card writes one sector whole on a cut. That is
not measured, and it is what this board check must confirm before the grow is called safe on hardware.

## Record

Write the result to this file under `## Result` with: the board, the card's size and vendor, the table kind,
`info` before and after, the `e2fsck` exit, the boot count, and the resize time from the driver clock.
Do not record a result that was not run. If it was not run, leave the status line as it is.

## Result

Not run.
