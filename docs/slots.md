# Slots: the board's operating system images

A board can hold several operating system images side by side. Each image is a *slot*. One slot
is the default, the one every ordinary boot starts. Another can be booted once on *trial*: it
becomes the default only when it is confirmed, and a board that restarts before then boots the
default again. A board may have only one slot; a trial then falls back to it.

Two drivers do this work. `system-slots` serves `system.slots/1` and `system-reboot` serves
`system.reboot/1`. They carry bytes and keep files. Neither looks inside an image.

## Where a slot lives

| What | Where |
| --- | --- |
| The root filesystem, an EROFS image | `<data>/slots/<id>.erofs`, a file on the ext4 data partition |
| The slot's manifest and state | `<data>/slots/<id>.json` and `<data>/slots/<id>.meta` |
| The slot's boot files, kept by content | `<data>/slots/<id>.boot` |
| Kernel, device trees, initramfs, `cmdline.txt` | `<boot>/slots/<id>/` on the FAT boot partition |
| Pointers: default, previous, trial, fallbacks | `<data>/board/slots/` |
| Bundles sent as files, waiting to install | `<data>/slots/.incoming/` |

The boot partition holds a directory only for the slots that may boot next: the running,
default, previous and trial slots. The other slots' boot files stay on the data partition and go
back onto the boot partition when a pointer names that slot again. A directory goes onto the boot
partition before a pointer names it. Old directories come off only after the pointers are
written. A power cut at any point therefore leaves either the old pointers with their directories
or the new pointers with theirs.

The slot's initramfs opens the image through dm-verity. The root hash it checks against comes
from the bundle's manifest, and the slot's kernel command line carries it:
`<boot_args>.slot=<id> <boot_args>.roothash=<hex> <boot_args>.data=PARTUUID=<uuid> panic=5`.

## A bundle

A bundle is a tar holding:

- `slot.json`, the manifest (below).
- `slot.sig`, optional. It is an Ed25519 signature over the manifest's exact bytes.
- `rootfs.erofs`, the image with its dm-verity hash tree appended.
- `boot/`, the slot's boot files.

The driver checks every byte against the manifest as it arrives. A bundle that differs from its
manifest is refused, and nothing it wrote is left behind.

### The manifest, schema 3

| Key | Format |
| --- | --- |
| `schema` | `3`. Any other schema is refused with a sentence saying so |
| `id` | the slot id, `[A-Za-z0-9._-]` |
| `product` | the operating system's product name, lowercase `[a-z0-9-]` |
| `version` | the operating system's version, text, not empty |
| `variant` | optional text |
| `board` | the board the slot is built for, lowercase `[a-z0-9-]` |
| `kernel`, `display_name` | optional text, shown by `list` |
| `image.size` | `rootfs.erofs`'s size in bytes, hash tree included |
| `image.sha256` | `rootfs.erofs`'s SHA-256, 64 lowercase hex digits |
| `verity.root_hash` | 64 lowercase hex digits |
| `verity.salt` | lowercase hex, an even number of digits up to 512; `""` for no salt |
| `verity.data_blocks` | how many 4096-byte data blocks the tree covers |
| `verity.hash_offset` | the byte offset in `rootfs.erofs` where the hash tree starts |
| `boot.files` | each file's path under `boot/` to its SHA-256 |

A manifest with a `family` key is refused. That key belonged to schema 2.

**The board.** The driver installs a slot only when its `board` equals the board file's
`[board] id`. Otherwise the job ends `NOT_SUPPORTED`, and `list`'s `error` names both boards. A
slot already on disk that names another board, or no board, is still listed. `trial` and
`set_default` refuse it with `NOT_SUPPORTED`.

**dm-verity.** These are fixed by convention and aren't in the manifest:

- format version 1;
- SHA-256;
- 4096-byte data blocks and hash blocks;
- no verity superblock.

The hash tree comes straight after the data. `verity.hash_offset` must equal
`verity.data_blocks * 4096`. `image.size` must equal `hash_offset` plus the tree's size. The
tree's size is every level, each `ceil(blocks below / 128)` hash blocks, down to one block. A
manifest whose numbers don't fit is refused at install. The EROFS superblock is never read for
any of this.

An image builder makes a conforming image like this:

```text
mkfs.erofs -b 4096 rootfs.erofs <rootfs dir>  # 4096-byte blocks: the size is a multiple of 4096
DATA=$(stat -c %s rootfs.erofs)
veritysetup format --no-superblock --format=1 --hash=sha256 \
  --data-block-size=4096 --hash-block-size=4096 --data-blocks=$((DATA / 4096)) \
  rootfs.erofs tree.bin                         # prints "Salt:" and "Root hash:"
cat tree.bin >> rootfs.erofs
```

Its outputs go into the manifest as follows:

- `Root hash` goes to `verity.root_hash`.
- `Salt` goes to `verity.salt`.
- `DATA / 4096` goes to `verity.data_blocks`.
- `DATA` goes to `verity.hash_offset`.
- The final file's size and SHA-256 go to `image.size` and `image.sha256`.

`lemnos-slot-init` builds the table from the manifest and these conventions alone:

```text
0 <data_blocks * 8> verity 1 <loop> <loop> 4096 4096 <data_blocks> <hash_offset / 4096> sha256 <root_hash> <salt or ->
```

It refuses a slot whose values don't fit.

**What is trusted at boot, and why.** The root hash is the only value that has to be trusted when
the board boots.

- **The root hash** reaches the initramfs on the kernel command line
  (`<boot_args>.roothash=`). That line is in the slot's boot directory, next to the kernel and
  initramfs, so it is exactly as trustworthy as the kernel and initramfs: as much as the board's
  boot chain makes them.
- **The other values** are read at boot from `slots/<id>.json` on the data partition, which
  nothing verifies at boot. Every one of them is bound by the root hash:
  - The salt goes into every hash.
  - The block count sets the tree's shape, and the root covers that shape.
  - The offset only says where to read the tree.

  So a wrong value makes dm-verity fail. It can never make a different filesystem pass.
  `slot-init` also refuses a manifest whose `root_hash` differs from the command line's.
- **The fixed parameters** (algorithm, block sizes, format) are in `slot-init` itself.
- **The image's superblock** sits inside the image. It is covered by `image.sha256` at install,
  but nothing checks it at boot, so nothing at boot relies on it.

Signing is a board setting. The `keys` file lists the keys the board trusts, one per line: 64 hex
digits, a space, then the key's owner. A signature that is present is always checked. `list`
shows each slot as one of these:

- `"signed":"trusted"`, with the key's owner as `"signer"`;
- `"untrusted"`;
- `"unsigned"`.

With `require_signed = true`, only bundles signed by a trusted key install. The setting is off by
default.

## Two ways to send a bundle

- **Streamed.** `install(size, pin, keep_old)` starts a long job. Its starter then sends the
  bytes in order with `write(offset, bytes)`.
- **As a file.** The sender first asks `accept(size)`. The answer is `ENOSPC` when the bundle and
  the reserve don't fit, and `BUSY` while another install runs. If it fits, the sender writes the
  bundle into `<data>/slots/.incoming/<name>` and calls `install_file(name, pin, keep_old)`, a
  long job. The driver reads the file a little on each tick.

  The name is one path component of `[A-Za-z0-9._-]`. The file is opened without following
  links and must be a regular file. The driver deletes the file when the job ends, whatever the
  outcome. It empties `.incoming` when it starts, and leaves it mode 0770 with group `lemnos`, so
  a sender in that group can write there and nothing else can.

Both ways feed the same unpacker and the same checks. A job's progress is the bytes taken of the
size. Its result is the new slot's id. One install runs at a time.

## Actions

| Method | What it does |
| --- | --- |
| `list` | Every slot (with its `product`, `version` and `board`), the pointers, free space, the reserve, the number kept, the last error |
| `install`, `write` | A streamed install (a long job) |
| `accept`, `install_file` | An install from a file in `.incoming` (a long job) |
| `set_default(id, force)` | The default from the next boot. The slot must have booted well, unless `force` |
| `trial(id, keep_default, keep_old, force)` | Restarts into `id` once |
| `confirm` | A running trial becomes the default. A trial that isn't running is recorded as failed |
| `remove(id)` | Removes a slot. The running, default, trial and pinned slots are refused |
| `pin(id, pinned)` | Keeps a slot from housekeeping and from `remove` |
| `set_keep(n)` | How many slots housekeeping keeps; 0 keeps every slot |
| `reboot` (`system.reboot/1`) | Restarts the board |

Housekeeping runs on `confirm` and keeps `keep` slots. It never removes the running, default,
trial or staged slot, and those count toward `keep`. After them it keeps the previous slot, then
the newest. A slot that has failed is kept only while there is room left under `keep`. Pinned
slots don't count and are never removed. `keep_old` on an install or a trial makes its confirm
remove nothing. No backup slot is forced: `remove` may take the previous slot, and a trial still
falls back to the default.

## Boot records

The `record` setting chooses how the boot loader is told which slot to start.

### `tryboot`, the default

This uses the board firmware's one-shot boot:

- `config.txt` includes `default.txt`, and `tryboot.txt` includes `trial.txt`. Each of those is
  one line, `os_prefix=slots/<id>/`.
- A restart with the argument `0 tryboot` reads `tryboot.txt` once.
- The initramfs knows a trial boot from `/proc/device-tree/chosen/bootloader/tryboot`.

### `uboot`

This uses U-Boot's standard boot flow. There is no saved U-Boot environment and no boot count.

- At install, each slot gets `slots/<id>/extlinux/extlinux.conf`:
  - `kernel` is the slot's `Image`, `Image.gz`, `zImage`, `vmlinuz*` or `kernel*.img`.
  - `initrd` is its `initramfs*` or `initrd*`.
  - `fdt` names a single top-level device tree. With several device trees, or a `dtbs/`
    directory, it is `fdtdir`.
  - `append` is the slot's command line.
- The record is `boot-record.env` at the boot partition's root, with three lines: `default=`,
  `trial=` and `trial_armed=0|1`. The driver writes it atomically: a temporary file, an fsync,
  a rename, then an fsync of the directory. A trial writes `trial_armed=1`.
- `packaging/uboot/boot.cmd` is a fixed script, built with
  `mkimage -T script -d boot.cmd boot.scr`. It loads the record and then does the following:
  1. If a trial is armed, it first writes the record back with `trial_armed=0` and then boots
     the trial's `extlinux.conf`. The trial gets one try: if it doesn't come up, the next boot
     finds the record disarmed.
  2. Otherwise it boots the default's `extlinux.conf`.
  3. If that fails or there is no record, it boots `/extlinux/extlinux.conf`. The driver keeps
     that file as a copy of the default's.

  It needs U-Boot built with `sysboot` (extlinux), `env import -t` and `env export -t`,
  `test`, and write support for the boot partition's filesystem (`save`; FAT write).

The tests check the extlinux files, the record's round trip, the atomic writes, and a decision
table that the script's text is checked against. **The script itself has never been run by
U-Boot.** The first board run with `record = "uboot"` is its first real test.

### `file`

This is for a loader with one fixed kernel that can't pick a slot's kernel.

- The record is one `boot-record` file with a `default=` line and a `trial=` line.
- The loader always starts the kernel and initramfs at the boot partition's root. The driver
  keeps those as a copy of the default slot's.
- The slot's initramfs (`slot-init`) picks the slot from the record, and clears `trial=` when it
  starts a trial.

What this gives up: every slot runs the default's kernel. A slot whose image needs a different
kernel can't boot until it is the default. Even then the root copy changes only on `confirm` or
`set_default`, so a trial of a new kernel isn't possible.

### Raw NAND

On raw NAND, the same contract works over UBI volumes, and the record is a tiny static UBI
volume. That backend is a later PR. It isn't in the tree yet.

## Settings

```toml
[[devices]]
id = "slots"
driver = "system-slots"
config = { data = "/data", boot = "/boot/firmware", record = "tryboot", keys = "/etc/lemnos/slot-keys", boot_args = "os", require_signed = false, reserve_mb = 2048, keep = 3 }

[[devices]]
id = "reboot"
driver = "system-reboot"
```

The driver's crate documentation, `drivers/system-slots/src/lib.rs`, lists every setting.

## Proof

```sh slow
cargo test -q -p lemnos-driver-system-slots -p lemnos-contract-system-slots -p lemnos-contract-system-reboot
```

Every test runs over temporary directories that stand in for the two partitions, with a record
fake that writes down what it was told. None of them touches a disk or the boot configuration.
`testing/board-runs/I5.md` is the board run. Until it runs, these drivers are untested on board.
