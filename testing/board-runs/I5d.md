# I5d board run: `lemnos-slot-init` booting a slot on the CM5

Status: untested on board. `lemnos-slot-init` has run only in its tests, over a temporary
directory standing in for `/`, with a fake for every system call. It has never been the `/init`
of a real initramfs, opened a real dm-verity device, or restarted a board.

Run in a local session on the CM5. Take `/run/lock/board-gpu.lock` for the whole run. Build on
NVMe or SSD, never on the Github HDD. Restarts, slot changes and writes to the boot partition are
allowed; report each. Keep a serial console attached: every step logs to the kernel log as
`slot-init: ...`.

## Setup

1. Build `lemnos-slot-init` for `aarch64-unknown-linux-musl` with the `service` profile
   (`docs/building.md`). Make an initramfs (newc cpio) holding it as `/init`, plus
   `/lib/modules/<release>/` with uncompressed `dm-mod`, `dm-bufio` and `dm-verity` and the
   kernel's `modules.dep`, unless those are built in. Also create the empty directories `/proc`,
   `/sys`, `/dev`, `/data`, `/boot` and `/newroot`.
2. Build two slot bundles from two images that differ, as `docs/slots.md` says: a random 64-hex
   salt per image, the tree appended, schema 3 manifests. Put this initramfs in both bundles'
   `boot/`. Install both with the `system-slots` driver (`I5.md`), so the board has a default
   `A` and a previous `B`, each with its boot directory on the boot partition.
3. Before each step, keep a copy of the boot partition's `default.txt`, `trial.txt` and
   `config.txt`, and of `/data/board/slots/`.

## Checks

1. **A plain boot.** Restart. Expect `starting slot A` on the console, `/` mounted from
   `/dev/dm-0` as EROFS read-only, `/data` mounted, and `board/slots/booted` reading `A`.
   `dmsetup table slot` must show the table from `docs/slots.md`, salt included.
2. **A trial.** Use the driver's `trial` to start `B`. Expect `trial-init` and `trial-started`
   to read `B`, and `confirm` to make it the default.
3. **A damaged image falls back.** Flip one byte inside `A`'s data area in `/data/slots/A.erofs`
   (keep a copy) and make sure `A` is the default. Restart. Expect:
   - an EROFS or dm-verity read error, or a refusal at mount;
   - if the failure happens in the initramfs: `A.meta` with `FAILED=yes` and its reason,
     `last-failure.log` on the data partition, `slot-init.log` on the boot partition,
     `trial.txt` naming `B`, and a `0 tryboot` restart that boots `B`;
   - `default.txt` unchanged.

   Then put the byte back.
4. **A root hash that isn't the manifest's.** Edit `A`'s `slots/A/cmdline.txt` on the boot
   partition so `<prefix>.roothash=` is wrong (keep a copy). Restart. Expect the refusal
   `root hash isn't the one on its command line` in `A.meta`, and the fallback to `B` on trial.
   Restore the file.
5. **Three in a row.** Repeat check 4 without confirming `B` in between, until
   `board/slots/fallbacks` reads 3. Expect the next boot to show `no slot can start` and
   `fell back 3 times in a row`, wait 30 s, and restart into `A` again. Restore and confirm.
6. **A cut-short image.** Truncate a copy of `A.erofs` by one block and put it in place. Expect
   `damaged or cut short` and a fallback. Restore it.
7. **A data partition with errors.** Not on this board unless a spare card is used: set the ext4
   error flag with `debugfs -w -R "ssv state 3"` on a spare data partition. Expect a read-only
   `/data` and a boot that still reaches the slot.

## Results

Not run yet.
