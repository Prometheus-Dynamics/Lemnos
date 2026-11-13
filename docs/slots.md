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
| `verity.salt` | exactly 64 lowercase hex digits (32 bytes), random per image; required |
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
`verity.data_blocks * 4096`. `image.size` must equal `hash_offset` plus the tree's size. A
manifest whose numbers don't fit, or whose salt isn't 64 hex digits, is refused at install and
at boot. The EROFS superblock is never read for
any of this.

The tree's size: with 128 SHA-256 digests to a 4096-byte hash block, each level is
`ceil(n / 128)` blocks, repeated **while the count being hashed is more than 1**, and the levels
are summed. A single data block has no tree at all: its root hash is the hash of that block.
Inside the tree the levels are stored **top level first**, down to the level over the data. That
is the order dm-verity computes each level's start in, from the table's hash start block
(`hash_offset / 4096`), and the order `veritysetup` writes.

These vectors come from a real `veritysetup` 2.8.7 run (`format --no-superblock
--data-block-size=4096 --hash-block-size=4096 --hash=sha256 --salt=<64 hex> --data-blocks=N`).
The driver's tests hold all of them, with a tree built in the test that checks the size, the
top-first order and the root hash:

| data blocks | hash tree bytes | root hash (salt `000102…1f`, block i filled with byte i mod 251) |
|---|---|---|
| 1 | 0 | `4ce3ecf32c133bf6321901b6092219474b6ac91a19d0304621d629e6bb9987dc` |
| 2 | 4096 | `bd780675f50010ed023fd1f07e98f029a6aaf25a8f6ee33ec94b4b4e847e4b2e` |
| 128 | 4096 | `acb37719c99f3ac554ecbbccb8910616915a08e5a67bd3b9778694c60e673da4` |
| 129 | 12288 | `6093a2333523050b628581510028976d3d3e9c62458642a83727e6df641397a4` |
| 16384 | 528384 | `234a6801a6840c2defdf3a7c08060cf9874f5773c6360f4172b08ae891a611d5` |
| 16385 | 540672 | `f1acccee31dcae56f612342ed838ba4c1f375de2d0341ae0cddecd0b28ff8732` |
| 38400 | 1245184 | `a73f8f9623fc3e3298871b6ef9de8ae4ba8867f23f5d8a92b87d97b0b5ddfa18` |

An image builder makes a conforming image like this:

```text
mkfs.erofs -b 4096 rootfs.erofs <rootfs dir>  # 4096-byte blocks: the size is a multiple of 4096
DATA=$(stat -c %s rootfs.erofs)
SALT=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')   # a new random salt per image
veritysetup format --no-superblock --format=1 --hash=sha256 --salt=$SALT \
  --data-block-size=4096 --hash-block-size=4096 --data-blocks=$((DATA / 4096)) \
  rootfs.erofs tree.bin                         # prints "Root hash:"
cat tree.bin >> rootfs.erofs
```

Its outputs go into the manifest as follows:

- `Root hash` goes to `verity.root_hash`.
- `$SALT` goes to `verity.salt`.
- `DATA / 4096` goes to `verity.data_blocks`.
- `DATA` goes to `verity.hash_offset`.
- The final file's size and SHA-256 go to `image.size` and `image.sha256`.

`lemnos-slot-init` builds the table from the manifest and these conventions alone:

```text
0 <data_blocks * 8> verity 1 <loop> <loop> 4096 4096 <data_blocks> <hash_offset / 4096> sha256 <root_hash> <salt>
```

It refuses a slot whose values don't fit.

**What is trusted at boot, and why.** The root hash is the only value that has to be trusted when
the board boots.

- **The root hash** reaches the initramfs on the kernel command line
  (`<boot_args>.roothash=`). That line is in the slot's boot directory, next to the kernel and
  initramfs, so it is exactly as trustworthy as the kernel and initramfs, and no more (see
  [What this protects](#what-this-protects-and-what-it-doesnt)).
- **The other values** are read at boot from `slots/<id>.json` on the data partition, which
  nothing verifies at boot. Every one of them is bound by the root hash:
  - The salt goes into every hash.
  - The block count sets the tree's shape, and the root covers that shape.
  - The offset only says where to read the tree.

  So a wrong value makes dm-verity fail. With the root hash as given, it can never make a different filesystem pass.
  `slot-init` also refuses a manifest whose `root_hash` differs from the command line's.
- **The fixed parameters** (algorithm, block sizes, format) are in `slot-init` itself.
- **The image's superblock** sits inside the image. It is covered by `image.sha256` at install,
  but nothing checks it at boot, so nothing at boot relies on it.

### What this protects, and what it doesn't

The root hash on the kernel command line is only as trustworthy as the boot partition that holds
the kernel, the initramfs and the command line. dm-verity here gives integrity against
corruption, and against tampering with the data partition alone. It is **not** verified boot,
unless the SoC's secure boot verifies the boot partition. When `require_signed` is on, the
signature on `slot.json` protects what gets installed, not what boots.

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

## Over Orion

Orion frames are at most 64 KiB, so a bundle doesn't travel as an action argument. With
`LEMNOS_ORION_FILES=slots=<data>/slots/.incoming` set for `lemnos-orion`
([lemnos-orion](daemon.md#lemnos-orion)), a sender does this:

1. Pushes the bundle with Orion's file delivery to `lemnos/slots/<name>`. The bridge asks the
   driver's `accept` with the size first. A board without room rejects the push before a byte
   is sent. The bridge checks the SHA-256 and only then renames the file into `.incoming/<name>`.
2. Runs `lemnos.slots.call` with `install_file <name> <pin> <keep_old>`. The install's progress
   is the run's progress, its slot id is the run's output, and cancelling the run cancels the
   install. The driver deletes the file when the job ends.

Tested against a real Orion node and comm core on a PC, in-process
(`service/orion/tests/files.rs`). There, pushes of up to 258 MiB were committed.

## Actions

| Method | What it does |
| --- | --- |
| `list` | Every slot (with its `product`, `version` and `board`), the pointers, free space, the reserve, the number kept, the last error |
| `install`, `write` | A streamed install (a long job) |
| `accept`, `install_file` | An install from a file in `.incoming` (a long job) |
| `set_default(id, force)` | The default from the next boot. The slot must have booted well, unless `force` |
| `trial(id, keep_default, keep_old, force)` | Makes `id` the trial, with `trial_attempts` boots allowed to start it, and restarts |
| `confirm` | A running trial becomes the default, recorded in `resolved` as confirmed by the caller. A trial that isn't running is recorded as failed. A trial also confirms itself, see [How a trial resolves](#how-a-trial-resolves) |
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
- A restart with the argument `0 tryboot` reads `tryboot.txt` once. The firmware keeps no
  count, so a trial's attempts are the file `trial-attempts` beside it, `0` to `3`.
- The initramfs knows a trial boot from `/proc/device-tree/chosen/bootloader/tryboot`.
- The default slot's `lemnos-slot-init` takes the attempt. When `trial.txt` names a slot, the
  firmware didn't boot the trial, and `trial-attempts` is above 0, it writes one fewer and
  restarts with `0 tryboot`, which boots the trial. With 0, it removes `trial.txt` and boots the
  default. See "The extra pass on tryboot", below.

### `uboot`

This uses U-Boot's standard boot flow. There is no saved U-Boot environment and no boot count.

- At install, each slot gets `slots/<id>/extlinux/extlinux.conf`:
  - `kernel` is the slot's `Image`, `Image.gz`, `zImage`, `vmlinuz*` or `kernel*.img`.
  - `initrd` is its `initramfs*` or `initrd*`.
  - `fdt` names a single top-level device tree. With several device trees, or a `dtbs/`
    directory, it is `fdtdir`.
  - `append` is the slot's command line.
- The record is `boot-record.env` at the boot partition's root, with three lines: `default=`,
  `trial=` and `trial_attempts=0..3`, the boots that may still start the trial. The driver
  writes it atomically: a temporary file, an fsync, a rename, then an fsync of the directory. A
  trial is written with the board's `trial_attempts`.
- `packaging/uboot/boot.cmd` is a fixed script, built with
  `mkimage -T script -d boot.cmd boot.scr`. It loads the record and then does the following:
  1. If a trial has attempts (1 to 3), it takes one: the record is written back with one fewer
     and only then the trial's `extlinux.conf` is booted. A trial that doesn't reach the daemon
     has used its attempt, and the next boot takes another. At 0, the trial is not started.
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

- The record is one `boot-record` file with a `default=` line, and for a trial a `trial=` line
  and `trial_attempts=`.
- The loader always starts the kernel and initramfs at the boot partition's root. The driver
  keeps those as a copy of the default slot's.
- The slot's initramfs (`slot-init`) picks the slot from the record. A trial with attempts takes
  one and writes the record back before it boots; with none left it clears `trial=` and the
  default starts.

What this gives up: every slot runs the default's kernel. A slot whose image needs a different
kernel can't boot until it is the default. Even then the root copy changes only on `confirm` or
`set_default`, so a trial of a new kernel isn't possible.

### Raw NAND, over UBI

On a board whose storage is raw NAND, the same slots live in UBI volumes. Set `ubi = "ubi0"`
and `record = "ubi"`. The volumes on that UBI device are:

| Volume | Type | What it holds |
|---|---|---|
| `data` | dynamic, UBIFS | the data directory, mounted at `data` |
| `boot` | dynamic, UBIFS | the boot directory, mounted at `boot`: `slots/<id>/` with each slot's kernel and `extlinux.conf` |
| `slot-<id>` | static | the slot's image with its hash tree, exactly `image.size` bytes |
| `slot-new-<n>` | static | an image while install job `n` writes it |
| `boot-record` | static | the U-Boot record, the same three lines as `boot-record.env` |

- **An install** makes `slot-new-<n>` the size of `rootfs.erofs`, writes it whole as the bundle
  streams in (`UBI_IOCVOLUP`), checks it like a file, and renames it to `slot-<id>`. UBI renames
  atomically and removes a volume already called `slot-<id>` in the same step. A refused,
  cancelled or interrupted install removes its work volume. `remove` removes the slot's volume.
  `accept` and the reserve count the UBI device's free erase blocks.
- **The record** is replaced whole: written to `boot-record-new`, then renamed over
  `boot-record`. A power cut leaves the old record or the new one.
- **The loader** runs `packaging/uboot/boot-ubi.cmd`. It is the `uboot` script's decision over
  the volume:
  - `ubi part ${ubi_mtd}` and `ubifsmount ubi0:boot`;
  - `ubi read` the record;
  - for a trial with attempts, `ubi write` it back with one fewer, then sysboot the trial;
  - then the default;
  - then `/extlinux/extlinux.conf`.

  U-Boot's `ubi write` of the record isn't atomic. A power cut in it leaves the volume
  unreadable, and the script then boots the root copy, which is the default's.
- **The kernel** attaches UBI itself: a slot's `boot/cmdline.txt` must carry `ubi.mtd=<n>`.
  `lemnos-slot-init` reads `<boot_args>.data=UBI=ubi0:data`. It mounts the data and boot
  volumes as UBIFS, makes a read-only `ubiblock` device over `slot-<id>`, and opens dm-verity on
  it. A fallback writes its trial into `boot-record` the same atomic way.

The script and the UBI calls have run only against fakes. The tests check the volume names, the
structures and request numbers against `mtd/ubi-user.h`, and the script's text against the
decision table. `testing/board-runs/I5c.md` is the board run.

## Booting a slot: `lemnos-slot-init`

`service/slot-init` builds `lemnos-slot-init`, the `/init` of a slot's initramfs. It is a static
program of plain `std`: no serde, no async, and its system calls are `lemnos-linux-sys`'s safe
wrappers (`rootfs`). It reads and writes the same files as the driver through the shared crate
`drivers/slot-store` (the layout, the board's state, the manifest and the boot records).
`docs/building.md` has the static builds for aarch64, armv7 and riscv64.

### The initramfs

The initramfs holds `lemnos-slot-init` as `/init` and, when they are modules, `dm-mod`,
`dm-bufio` and `dm-verity` under `/lib/modules/<release>/` with that kernel's `modules.dep`. They
must be uncompressed. A module that is built in fails to load, which is logged and harmless. The
kernel needs devtmpfs, ext4, vfat, EROFS, loop and dm-verity.

### What it does

1. It mounts `/proc`, `/sys` and `/dev`, and reads the slot arguments from the kernel command
   line. The prefix is whatever stands before `.slot=` (the driver's `boot_args`).
2. It finds the data partition from `<prefix>.data=`, `PARTUUID=` (MBR or GPT), `LABEL=` (ext4) or
   `UBI=<device>:<volume>` (raw NAND, below),
   from `/sys/class/block` and the disks themselves. It waits up to 10 s, and if nothing answers it
   takes the partition labelled `data`. A filesystem whose superblock records errors is mounted
   read-only. There is no fsck here.
3. It reads the boot partition (partition 1 of the same disk) read-only. If a `boot-record` file
   is there, the record picks the slot: its `trial=`, which is cleared before the slot starts,
   else its `default=`. The root hash is then the one on that slot's own
   `slots/<id>/cmdline.txt`.
4. A boot is a trial when the firmware's tryboot flag is set, when the `file` record gave the
   trial, or when the slot isn't the board's default. A trial writes `board/slots/trial-init`.
5. It refuses the slot if:
   - its manifest is missing, isn't schema 3 or is for another slot;
   - the manifest's root hash isn't the command line's;
   - the image isn't exactly `image.size` bytes;
   - `kernel` names a kernel other than the running one.
6. It loads the device-mapper modules, opens the image through a read-only loop device and
   dm-verity with the table above, and mounts it read-only as EROFS.
7. It moves `/data` into the new root and writes `board/slots/booted`, plus `trial-started` on a
   trial. Then it `switch_root`s into `/sbin/init`.

### When the slot can't come up

The failure is kept and recorded before anything else happens:

- The kernel log goes to `board/slots/last-failure.log` on the data partition and to
  `slot-init.log` on the boot partition.
- The slot's `.meta` gets `FAILED=yes`, `FAILURES` plus one and `FAIL_REASON`. A refusal by
  dm-verity reads `dm-verity refused the image: ...`.

The next step is a pure function (`service/slot-init/src/decide.rs`). Each rule has a test that
fails without it:

1. **A trial that fails** restarts plainly, so the boot loader starts the default (on the way it
   takes the next attempt, if it has one left).
2. **The default failing** `3` times in a row stops falling back. The count is
   `board/slots/fallbacks`, which a confirmed boot clears. The board then shows the reason on the
   console for 30 s and restarts.
3. **Otherwise** the board restarts into another slot, on trial. The candidates, best first:
   - the previous slot, if it hasn't failed since it was confirmed;
   - any other slot that booted well and hasn't failed, newest first;
   - the previous slot even though it failed;
   - any other slot, newest first.

   Only a slot whose boot directory is on the boot partition can be started.
   - The trial goes into the record the boot partition holds: `trial.txt` and a `0 tryboot`
     restart, `boot-record.env` with `trial_attempts=`, `boot-record`'s `trial=` and
     `trial_attempts=`, or on raw NAND the `boot-record` volume with `trial_attempts=`. The
     attempts are the board's `trial_attempts`, read from `board/slots/settings.env`.
   - The board's state gets `TRIAL` set to that slot, so a `confirm` from it makes it the default:
     the board has rolled back.
4. **With no candidate**, such as a board with one slot, the board waits, then restarts into the
   same default.

It never writes the default. The worst it does is start one other slot once, on trial, so a plain
restart always returns to the default. With the data partition read-only, nothing is recorded on
it, but the trial is still written to the boot partition.

**Untested on board.** The tests run the whole flow over a temporary directory that stands in
for `/`, with a fake for every system call. `testing/board-runs/I5d.md` is the board run.

## How a trial resolves

A trial is a slot the board starts once, so it can be judged: `trial` makes it the trial and
restarts. Its fate is decided by the boots that follow, not by a tool calling `confirm`. A board on
a robot updates, reboots and runs with nobody connected, so a trial confirms itself.

**Attempts.** The record holds `trial_attempts`, 3 by default: the boots that may still start the
trial. Before each trial boot the loader takes one. The script does that for `uboot` and `ubi`,
`lemnos-slot-init` for `tryboot` and `file`. A boot that never reaches the daemon has used its
attempt, and three in a row fall back. When the daemon starts in the trial slot, the driver gives
the attempt back, once per boot. So a trial survives power cuts while its daemon has come up, and
keeps its attempts until it confirms or runs out.

**Confirmation.** After a trial boot, the driver confirms on its own once every condition holds:

- the daemon has run for `confirm_uptime_s` (30 s);
- no device that was running and healthy in the previous good slot now regresses. A device from
  the board file's own list, on a bus that stays put, regresses when it is missing, did not bind,
  is failed or is `Gone`. A hotplug device (one an `[[each]]` entry made, or one on a USB port: the
  board's origin for it is `Hotplug`) is not the board's own hardware: when it is unbound, `Gone`
  or absent it is simply not there, and only one that is bound and failed blocks. An unplugged
  USB device's driver is dropped and retries as failed, so its unbound state is what the board
  sees, and it must not block. The previous slot's healthy devices are written as
  `board/slots/<id>.devices` once that slot has run for its uptime, and at each confirm, each with
  its origin (`builtin` or `hotplug`);
- every `health` source in the board file is good: `device:<id>` is `Ok`, `file:<path>` exists and
  was written since this boot began, `socket:<path>` accepts a connection. A socket is connected
  without waiting: a listener whose backlog is full answers busy, the look is not good, and the
  next look tries again. A socket still busy after 2 s is not good for good, and the reason says
  so. Nothing on a tick waits on another process;
- the state and the boot record can be written.

Each of these is looked at each second while the trial waits, and the reason it is still waiting
is kept in `trial.env` and shown by `list`. The confirm is recorded in `resolved`, by `auto`.

**The deadline.** If `confirm_deadline_s` (600 s) passes from the daemon's start with a condition
unmet, the reason is kept, the trial's attempts are set to 0 and, with `reboot_at_deadline` (the
default), the board restarts plainly. The default comes back, and the trial is recorded as fallen
back with that reason. With `auto_confirm = false`, nothing confirms or restarts by itself, and
`confirm` works at any time during the wait.

**When the reason can't be kept.** The restart is the way the default comes back, so it is never
held for long. The reason is written to the state (the trial's `trial.env`) with the attempts set
to 0. If the state can't be written, the driver tries again with a capped backoff: at 0, 1, 3, 7,
15 and 23 s after the deadline (the waits double from 1 s and stop at 8 s), and restarts at the
last try whatever happened. The reason is also written to `trial-reason` on the boot partition, beside
the boot record, whenever that can be written, and the next start reads it there when the state
has no reason. The boot loader never reads that file, so it can't change what boots. `list` shows
the failure as `trial.record_error`, and it is null once the reason is kept.

**Fallen back.** A trial that is out of attempts is resolved on the first boot of the default: its
state is `rolled-back`, the slot is marked failed with a reason (the trial slot's own `FAIL_REASON`
when it has one, else "did not reach a running daemon in N boots"), and `resolved` says so.

**What `list` shows.** Two fields at the end of the object: `trial` is null, or the trial that is
pending across boots, `{slot, attempts_left, attempts:[{n,time}], last, last_time, reason,
record_error}`, where `record_error` is null or the text of why the driver's record could not be
written, with
`last` one of `power-lost-before-daemon`, `daemon-reached-not-yet-confirmed`, `confirmed` and
`fell-back` (null before the trial's first boot). `resolved` is null, or the last resolution,
`{slot, outcome: confirmed|fell-back, by: auto|call, caller, reason, time}`. `caller` is the caller's
number for a `call` and null for `auto`.

**The extra pass on tryboot.** The firmware keeps no counter, so on the `tryboot` form every trial
boot runs the default slot's kernel and initramfs first: the initramfs takes the attempt and
restarts with `0 tryboot`. That is one extra firmware, kernel and initramfs pass on each trial boot.
It is the price of the guarantee that a trial kernel which can never boot always falls back: the
attempt is taken before the trial's kernel runs, so a power cut cannot leave the trial without
its count. `testing/board-runs/I5f.md` measures it. **Untested on board.**

## Settings

```toml
[[devices]]
id = "slots"
driver = "system-slots"
config = { data = "/data", boot = "/boot/firmware", record = "tryboot", keys = "/etc/lemnos/slot-keys", boot_args = "os", require_signed = false, reserve_mb = 2048, keep = 3 }
# On raw NAND: config = { data = "/data", boot = "/boot", ubi = "ubi0", record = "ubi", boot_args = "os" }

[[devices]]
id = "reboot"
driver = "system-reboot"
```

The trial settings (`auto_confirm`, `confirm_uptime_s`, `confirm_deadline_s`,
`reboot_at_deadline`, `trial_attempts` and `health`) are in [Trials of a slot](boards.md#trials-of-a-slot-system-slots).
The driver's crate documentation, `drivers/system-slots/src/lib.rs`, lists every setting.

## Proof

```sh slow
cargo test -q -p lemnos-driver-system-slots -p lemnos-contract-system-slots -p lemnos-contract-system-reboot -p lemnos-slot-store -p lemnos-slot-init
```

Every test runs over temporary directories that stand in for the two partitions, with a record
fake that writes down what it was told. None of them touches a disk or the boot configuration.
`testing/board-runs/I5.md` is the board run. Until it runs, these drivers are untested on board.
