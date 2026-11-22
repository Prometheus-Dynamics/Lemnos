# FLASH-BCM2712: the first flash of the reference board's eMMC from a RAM flasher

Status: **untested on board.** The guest proof below ran on the PC in an arm64 QEMU guest with an eMMC. No board has
been flashed this way.

## What is built

- `drivers/system-flash`: the target may be named by its storage controller (`controller`, a device-tree path)
  instead of `path`. The target is the one whole disk under it (`lemnos_slot_store::disk::disk_under`, the slot
  driver's lookup): the eMMC's hardware boot areas are set aside, so the target is the user area. It is looked up at
  each call, so a disk that registers after the start is taken; its `/dev` node must be the device sysfs names.
- `drivers/slot-store`: `disk::disk_under`, and the reference board's own block listing as a shared fixture
  (`disk::fixture::emmc_board`, `testing` feature), which system-flash's tests use too.
- `service/orion` (the bridge): a committed image's answer detail is fields, `raw_bytes=<n> outcome=<0|1|2|none>`.
- `testing/boards/bcm2712-flasher.toml`: the reference board's flasher board file: `flash` on
  `controller = "/soc@107c000000/mmc@fff000"` with `allow_first_flash`, the ring as its indicator, the gadget for the
  bridge's link, the watchdog as keep-alive, `reboot`.
- `testing/flasher`: the arm64 guest run (`run.sh`, `build-kernel.sh`, `kernel.config`) and its guest
  (`testing/crates/flasher-check`).

## Proof on the PC

- Unit tests: `drivers/system-flash/tests/controller.rs` on the board's listing (the user area written, both boot
  areas untouched; a late disk named, then taken; the settings refused by name), `drivers/slot-store` `disk::` tests,
  `service/orion` `a_committed_image_s_detail_is_its_fields_for_every_outcome` and `tests/flash_stream.rs`.
- QEMU (`testing/flasher/run.sh`, kernel 7.3.5 from `build-kernel.sh`): QEMU's `emmc` on `sdhci-pci` with two
  1 MiB boot areas, all 0xa5 before the run. The real lemnosd, the bridge's `Streamer`, an 8 MiB MBR image as xz.
  Result: `flasher: passed (10 checks)`: the stream committed with `raw_bytes=8388608 outcome=1`; the verify job read
  back 8388608; `/dev/mmcblk0` holds the image (SHA-256 read back); both boot areas byte-identical; `mmcblk0p1` read
  from the new table (12288 sectors). Host side: `emmc: boot areas unchanged (2097152 bytes); user area holds the image
  (8388608 bytes); the rest unchanged`.

## The board step (for the board session's sheet)

This writes the board's eMMC: a persistent write. It needs the user's go for the run, and a board whose eMMC may be
erased (its own image is written back by this step).

Needs: the flasher image (a RAM-only initramfs with lemnosd, the bridge and the recovery build, its board file this
rev's `testing/boards/bcm2712-flasher.toml` with the board's own id), booted over RPIBOOT; the board's own signed
image as `image.img.xz` with its transport manifest.

1. Boot the flasher over RPIBOOT. Record: `lemnos-ctl list` (`flash` ok), `info` of `flash` (`writable` true; its
   target the eMMC's user area, not a boot area), the daemon's probe-to-bind time (`testing/board-runs/WDT-CHAIN.md`).
2. Push `image.img.xz.json`, then `image.img.xz`, to `lemnos/flash/`. Expect the answer `committed` with
   `raw_bytes=<the image's size> outcome=0` (signed by a key in the board's list).
3. `verify`: expect status 0 and the raw size.
4. Restart through `reboot`. Expect the board to boot its new slot from the eMMC.
5. Read back on the booted board: the boot areas' hashes (`mmcblk0boot0`, `mmcblk0boot1`) against their hashes from
   before the flash (recorded in step 1 from the flasher).

Pass: 2 to 5 as expected.
