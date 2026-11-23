# BOOT-FAT-LABEL: the boot FAT's volume label

Untested on board. The host side is checked (`cargo test -p lemnos-ctl --test boot_partition`, the Pi
firmware-layout check on every Pi-form image, `blkid`, `fsck.fat -n` and `mdir` on a built image).

## The check

Each on a bench board, with an image from `lemnos-ctl boot-partition` built at this change, written
only by the approved flashing path:

1. CM5 (Pi firmware form): the board boots the default slot from the labelled image. On the board,
   `blkid` on the boot partition shows `LABEL="LEMNOS-BOOT"` (or the board file's `boot_label`), and
   `/dev/disk/by-label/LEMNOS-BOOT` names it.
2. A U-Boot form board (RK3566 or ZERO 3E): U-Boot loads `boot.cmd` and the bay's files from the
   labelled image and the slot boots; slot-init reads the bays as before.
3. `lemnos-ctl call slots info` and `list` read the same as on an unlabelled image.

## Results

Not run yet.
