# I5c board run: slots on raw NAND, over UBI

Status: untested on board. The UBI path has run only against a fake UBI device: sysfs
directories and plain files standing in for volumes. No `UBI_IOCMKVOL`, `UBI_IOCVOLUP`,
`UBI_IOCRNVOL`, `UBI_IOCRMVOL` or `UBI_IOCVOLCRBLK` has been issued. U-Boot has never run
`packaging/uboot/boot-ubi.cmd`, and `lemnos-slot-init` has never booted from a `ubiblock` device.

Run in a local session on a board whose storage is raw NAND with UBI, booting through U-Boot.
None of the reference boards in this repository's runs is one, so this waits for such a board.
Take `/run/lock/board-gpu.lock` if the run shares the build machine with other board runs. Build
on NVMe or SSD, never on the Github HDD. Restarts, slot changes and writes to the NAND are
allowed; report each. Keep a serial console attached for U-Boot's output and `slot-init: ...`.

## Setup

1. Make a UBI layout on the NAND (`ubiformat`, `ubiattach`, `ubimkvol`):
   - a dynamic volume `data` and a dynamic volume `boot`, both formatted as UBIFS;
   - nothing else; the driver makes the rest.
2. U-Boot with `CONFIG_CMD_UBI`, `CONFIG_CMD_UBIFS`, sysboot (`CONFIG_CMD_SYSBOOT`) and
   `env import`/`env export`. Its default environment sets `ubi_mtd` to the MTD partition
   holding UBI. Install `boot.scr` built from `packaging/uboot/boot-ubi.cmd` where the board's
   boot flow runs it.
3. Two slot bundles as `docs/slots.md` says. Each `boot/cmdline.txt` carries `ubi.mtd=<n>`, and
   each initramfs is built with `lemnos-slot-init` (`I5d.md`).
4. The board file:

   ```toml
   [[devices]]
   id = "slots"
   driver = "system-slots"
   config = { data = "/data", boot = "/boot", ubi = "ubi0", record = "ubi", boot_args = "os" }
   ```

## Checks

1. **An install.** Install bundle A. Expect, while it streams, a volume `slot-new-<n>` in
   `ubinfo -a`, and then only `slot-A`, whose `data_bytes` is the manifest's `image.size`. Check
   that `cmp` of `/dev/ubi0_<N>` (its first `image.size` bytes) with the bundle's
   `rootfs.erofs` shows them equal.
2. **Refused and cancelled.** Install a bundle with a wrong `image.sha256`, and cancel a second
   one midway. Expect no `slot-new-*` volume afterwards, and the free erase block count back
   where it was.
3. **The record.** `set_default` A. Expect a static volume `boot-record` reading
   `default=A\ntrial=\ntrial_armed=0\n`, and no `boot-record-new`.
4. **A first boot.** Restart. Expect U-Boot to read the record and sysboot
   `/slots/A/extlinux/extlinux.conf`. Expect `slot-init` to mount `ubi0:data` and `ubi0:boot`,
   make `/dev/ubiblock0_<N>`, and start A with `/` on `dm-0`.
5. **A trial.** Install B and `trial` it. Expect `trial_armed=1` in the record, then U-Boot
   writing it back as `0` before B's kernel starts, and B booting. `confirm`, and expect B as
   the default.
6. **Fallback.** Corrupt one byte of `slot-A`'s data area. Do it through a copy, a
   `ubiupdatevol` and the same size, and keep the original. Make A the default and restart.
   Expect the read to fail through dm-verity, or `slot-init` to refuse it. Expect `A.meta` with
   the reason, and `boot-record` with `trial=B`, `trial_armed=1` and the default still A. Then a
   restart that boots B. Restore A.
7. **Power cut during the record's write.** Optional, on a board with a power switch: cut power
   during a `set_trial`. Expect either the old or the new record after the restart, never an
   unreadable one from the driver's side.

## Results

Not run yet.
