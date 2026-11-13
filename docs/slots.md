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
`<boot_args>.slot=<id> <boot_args>.roothash=<hex> ... panic=5`. The command line that boots is the
bundle's `boot/cmdline.txt`, byte for byte: nothing is appended at install. Its hash is in the
signed manifest's `boot.files`, so every boot file, the command line included, is checked against
one signed manifest. A bundle whose line does not name its own slot and root hash is refused by
name. The data partition is not on the command line (see `lemnos-slot-init`, below).

**Breaking, declared (I5i4):** the bundle's command line is not edited at install, and the
`<boot_args>.data=` argument is gone. A bundle built for an earlier layout is refused.

## A bundle

A bundle is a tar holding:

- `slot.json`, the manifest (below).
- `slot.sig`, optional. When present it is the shared signature record (`lemnos_slot_store::signing`,
  the same container first flash uses), exactly 108 bytes:

  | offset | bytes | content |
  |---|---|---|
  | 0 | 8 | `LMNSSIG1` |
  | 8 | 1 | algorithm: `1` (Ed25519) |
  | 9 | 3 | zero |
  | 12 | 32 | key id: SHA-256 of the signing key's 32-byte public key |
  | 44 | 64 | Ed25519 signature over the signed message |

  The signed message is the manifest's exact bytes (`slot.json` as it is in the bundle): no domain
  tag, no added fields. Vector: the test key is the 32-byte seed of `0x07` repeated (public key
  `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c`, key id
  `fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889`). The test
  `the_signature_member_is_the_shared_record_and_names_the_key_by_its_id` builds this record for a
  bundle and checks its header, key id and signature.

  A bare 64-byte signature (the first version's form) is malformed: refused, `INVALID_ARGUMENT`.
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

### Where the switch comes from, and the rules that keep a board up

`require_signed` is a setting of the `system-slots` device in the base image's board file (its
`config` table; `docs/boards.md`). The driver reads it when the daemon starts, and writes it for the
boot side as a marker, `require-signed`, in the state directory on the data partition. Switching it
on means editing the board file and restarting the daemon. Nothing in a slot image carries it.
The first-flash driver has its own `require_signed` in its board-file entry, so the two can differ
today; unifying the source is a follow-up (see the status note below).

The rules, so that a board always comes up:

- With the switch on, a slot whose signature did not verify is never set as the default nor tried
  (`set_default` and `trial` refuse it, naming it in `list`'s `error`).
- At start, if the switch is on and the default slot did not verify, the driver says so in `list`'s
  `error` (naming the default and what to install) and in `info` (`default_signature`). It keeps
  running; it does not refuse to start.
- At boot, an unverified trial is refused and the default starts instead, as for any failed trial.
- At boot, an unverified default always starts: the boot side never refuses the default. The start
  is recorded in `board/slots/boot-unverified` (the slot's id), which `info` shows as
  `boot_unverified`. A verified boot removes the record.

What is open: the switch is read through one function (`signing::security_on`) and written through
one (`signing::set_security`), so moving its source later is a change in one place. Orion has its
own security switch (`security`, read by its node's keys service; the Lemnos bridge does not read
it), and the two are not yet one setting. That is a follow-up.

### What this protects, and what it doesn't

The root hash on the kernel command line is only as trustworthy as the boot partition that holds
the kernel, the initramfs and the command line. dm-verity here gives integrity against
corruption, and against tampering with the data partition alone. It is **not** verified boot,
unless the SoC's secure boot verifies the boot partition. When `require_signed` is on, the
signature on `slot.json` protects what gets installed, not what boots.

Signing is a board setting. The `keys` file lists the keys the board trusts, one per line: 64 hex
digits, a space, then the key's owner. The record's key id is matched against those keys (the same
key id, SHA-256 of the public key, as first flash uses); a signature is checked against that key
when the board holds it. The policy is the table below. It is the same table first
flash applies (`lemnos_slot_store::signing`), and the outcome words are the same.

| `require_signed` | bundle | key | result | `signature` in `list` |
|---|---|---|---|---|
| off | unsigned | – | installs | `not_checked_unsigned` |
| off | signed | the board's key, verifies | installs | `verified` |
| off | signed | the board's key, does not verify | refused, before anything is written | (none) |
| off | signed | unknown key, or no key list | installs | `not_checked_unknown_key` |
| on | unsigned | – | refused | (none) |
| on | signed | unknown key or no key list | refused | (none) |
| on | signed | known key, bad signature | refused | (none) |
| on | signed | known key, verifies | installs | `verified` |

In every row, a malformed signature (a `slot.sig` that is not a 108-byte record: wrong length, magic,
algorithm or padding) is refused as an invalid argument. A
refusal is `NOT_PERMITTED` with the reason in `list`'s `error`, and nothing is installed.

`list` shows each slot as before, and adds one field at the end of the slot:

- `"signed"`: `trusted` (with the key's owner as `"signer"`), `untrusted` or `unsigned`. These are
  unchanged; `trusted` is `verified`, `untrusted` is `not_checked_unknown_key`, `unsigned` is
  `not_checked_unsigned`.
- `"signature"`: `verified`, `not_checked_unknown_key` or `not_checked_unsigned`. A slot installed
  before this field existed reads it from its `signed` label: `untrusted` there is
  `not_checked_unknown_key`, though that install may have been a bad signature under a known key
  (the record kept no key id), so the label cannot say more.

`info` shows the policy and the last install's signature result: `require_signed`, and
`last_install` with `slot` (empty when refused), `signature` (the outcome, or `refused`), `signer`
and `reason`. The install's job result is still the slot's id.

With `require_signed = true`, a slot whose signature did not verify is never set as the default
nor tried. The boot side enforces the same: the driver keeps a marker, `require-signed`, in the
state directory while the setting is on, and the boot program does not start a slot whose
signature did not verify. It falls back to another slot, as for any failed start, and the slot's
failure names the signature. So a slot installed with signing off stays installed, but it is not
bootable once signing is required.

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
| `list_page(after, size)` | The same object with one page of the slots: as many whole slots as fit the buffer, newest first, then `more` and `next`. `after` is `-` for the first page and the previous page's `next` after it (see below) |
| `info` | The signature policy (`require_signed`) and the last install's signature result |
| `install`, `write` | A streamed install (a long job) |
| `accept`, `install_file` | An install from a file in `.incoming` (a long job) |
| `set_default(id, force)` | The default from the next boot. The slot must have booted well, unless `force` |
| `trial(id, keep_default, keep_old, force)` | Makes `id` the trial, with `trial_attempts` boots allowed to start it, and restarts |
| `confirm` | A running trial becomes the default, recorded in `resolved` as confirmed by the caller. A trial that isn't running is recorded as failed. A trial also confirms itself, see [How a trial resolves](#how-a-trial-resolves) |
| `remove(id)` | Removes a slot. The running, default, trial and pinned slots are refused |
| `pin(id, pinned)` | Keeps a slot from housekeeping and from `remove` |
| `set_keep(n)` | How many slots housekeeping keeps; 0 keeps every slot |
| `boots` | The boot history: one line per boot kept, oldest first (see [Boot history](#boot-history)) |
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
  When the UBI device `ubi0` is attached, `lemnos-slot-init` takes its volume `data` as the data
  partition (no command-line argument names it). It mounts the data and boot
  volumes as UBIFS, makes a read-only `ubiblock` device over `slot-<id>`, and opens dm-verity on
  it. A fallback writes its trial into `boot-record` the same atomic way.

The script and the UBI calls have run only against fakes. The tests check the volume names, the
structures and request numbers against `mtd/ubi-user.h`, and the script's text against the
decision table. `testing/board-runs/I5c.md` is the board run.

## Two-copy records

State that must survive a power cut, and that a boot loader or the initramfs needs, is kept as
two-copy records (`lemnos_slot_store::twocopy`). A record is a pre-created file on the boot
partition (or a fixed raw place) of two 512-byte sectors. Each copy carries a magic, a version, a
kind, a sequence number and a CRC-32 over the sector. A write changes exactly one sector: the copy
that does not hold the highest valid sequence. A reader takes the valid copy with the higher
sequence. A cut leaves the old value or the new one, never neither, because the other copy is
never written by the same update. The format is in the module's documentation; the kinds are
`TRIAL`, `REASON`, `BOOT_HISTORY` and `SECURITY`.

The record files are never created, grown, renamed or deleted at run time, and nothing mounts the
boot partition: the placement is found by reading the FAT once, read-only, in our own code
(`lemnos_slot_store::bootfat`), and a file that is missing, fragmented, or not exactly the size
the image builder wrote is refused by name before anything is written. A run-time record write
therefore changes no filesystem structure.

Adding a kind: a new `Kind` constant with the next free number (numbers are never reused), a file
name in `Kind::file_name`, and an entry in the image build's list of kinds for `precreate`.

One honest limit: on a board whose firmware reads its pointer files (`default.txt`, `trial.txt`),
promoting a trial to default is a single-sector write with no second copy. Its safety rests on the
storage device completing or not starting a single-sector write, and it is what the pull-the-plug
procedure in `testing/board-runs/I5i.md` measures.

## Booting a slot: `lemnos-slot-init`

`service/slot-init` builds `lemnos-slot-init`, the `/init` of a slot's initramfs. It is a static
program of plain `std`: no serde, no async, and its system calls are `lemnos-linux-sys`'s safe
wrappers (`rootfs`). It reads and writes the same files as the driver through the shared crate
`drivers/slot-store` (the layout, the board's state, the manifest and the boot records).
`docs/building.md` has the static builds for aarch64, armv7 and riscv64.

### The initramfs

The initramfs holds `lemnos-slot-init` as `/init` and nothing else: no kernel modules, and no
`/lib/modules`.

**Rule: the whole chain is built into the kernel.** Every piece below must be `=y` (built in) in
the kernel that boots a slot; `docs/kernel-config.md` ("Slot path") has the table. The slot-init
program does not load modules. A board whose kernel has one of these as a module does not boot a
slot until that kernel is rebuilt with it built in.

**Pairing (this revision).** This revision's `lemnos-slot-init` needs a kernel with the slot
chain built in: `BLK_DEV_DM`, `CRYPTO_SHA256`, `DM_VERITY`, `EROFS_FS`, `EXT4_FS` (or `UBIFS_FS` on
raw NAND), `BLK_DEV_LOOP` (stock route), `MTD_UBI` (raw NAND), plus the storage host driver.
Paired with a kernel that has any of them as modules, it does not start a slot: the self-check
below names the option, and the board boots as described under "A kernel missing a piece".
Nothing in the initramfs makes up for it.

### What it does

1. It mounts `/proc`, `/sys` and `/dev`, and reads the slot arguments from the kernel command
   line. The prefix is whatever stands before `.slot=` (the driver's `boot_args`).
2. It finds the data partition by a fixed rule, not from the command line. On raw NAND, when the
   UBI device `ubi0` is attached, it is the UBI volume `data` (below). Otherwise it is the ext4
   partition labelled `lemnos-data` (`/sys/class/block` and the superblocks). The image builder sets
   that label. It waits up to 10 s for it; a board with two partitions carrying the label is refused
   at once, by name, since it cannot say which is its data. A filesystem whose superblock records
   errors is mounted read-only. There is no fsck here.
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
   - `kernel` names a kernel other than the running one;
   - a boot file of the slot doesn't match the manifest's hash (see "Boot-file checks" below).
6. It opens the image through a read-only loop device and dm-verity with the table above, and
   mounts it read-only as EROFS.
7. It moves `/data` into the new root and writes `board/slots/booted`, plus `trial-started` on a
   trial. Then it `switch_root`s into `/sbin/init`.

### The kernel self-check

Before anything is spent, written or mounted past the data partition, slot-init checks each piece
of the chain (`service/slot-init/src/kernel.rs`): device-mapper (`misc` has `device-mapper`),
SHA-256 (`/proc/crypto` has `sha256`), the verity target (`DM_LIST_VERSIONS` lists `verity`),
EROFS (`/proc/filesystems`), the loop device (`/proc/devices` has `loop`, stock route), the data
partition's filesystem (`ext4`, or `ubifs` on raw NAND), and `MTD_UBI` (raw NAND). Each absent
piece is logged by its kernel option, for example:

    slot-init: the kernel lacks CONFIG_DM_VERITY: the verity target over the slot's image

When no block device other than loop, RAM, UBI or device-mapper exists, the message names
`CONFIG_MMC_BLOCK` (or `CONFIG_BLK_DEV_NVME`) instead of a bare "no data partition".
`vfat` for the boot partition is optional: its absence is logged and the boot goes on.

**A kernel missing a piece is not a failed slot.** Nothing is marked: no `FAILED`, no `FAILURES`,
no `FAIL_REASON`, no fallback count, no trial on another slot. The check runs before any attempt
is taken, so an absent piece never uses up trial attempts.

Then, per boot:

- **Default boot, nothing else pending:** the boot keeps the kernel log (`slot-init.log` on the
  boot partition), prints `no slot can start: the kernel lacks CONFIG_...` and waits 30 s, then
  restarts. That is the last resort the boot had before, when no slot can be opened, with the
  reason named. It starts no slot and switches no root, so the board stays here until the kernel
  is fixed.
- **Trial boot** (the firmware's tryboot flag, a `file` record's trial with attempts, or a
  U-Boot or UBI record's trial): the attempt the boot that started the trial took is given back
  (`tryboot` counter and the U-Boot or UBI record count, up to the board's `trial_attempts`), the
  reason is written to `trial-reason` on the boot partition, and the board restarts plainly.
  - A `tryboot` or `file` trial stays in its record, held: the default boot takes no attempt for
    it while `trial-reason` holds a kernel-gap reason, and the default slot starts. The daemon
    reads `trial-reason` when it resolves the trial, so `list` shows the kernel option, not a bare
    mount error. A trial whose kernel was rebuilt is resolved by the daemon, which clears the
    reason.
  - A U-Boot or UBI record's trial is withdrawn (its trial cleared), because the loader takes an
    attempt on every trial start and can't be told to wait.

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

## Boot-file checks

Every boot file a slot has (kernel, device trees, overlays, `cmdline.txt`, initramfs) is listed in
the signed manifest's `boot.files` with its SHA-256. The checks compare what is on the boot
partition with that list.

- **Every boot, by `lemnos-slot-init`.** Before the image is opened, the booted slot's files are read
  raw through the FAT volume (`lemnos_slot_store::bootfat`: directory walk with long and short
  names, cluster chains followed, bounds and loop checks; a damaged chain is refused by name). No
  mount is made to read them. On raw NAND, where the boot partition is UBIFS, the read goes through
  its read-only mount. A default that does not match goes to the existing failure path, which
  starts another verified slot on trial; a trial that does not match fails the trial.
- **Before a restart and on a slow period, by lemnosd.** The default's files are checked against its
  manifest, read from the boot partition where the running system has it mounted (lemnosd does not
  mount it). When the default does not match and the previous does, the default is marked failed
  and the boot record is pointed at the previous, before the restart. When neither matches, the
  restart is refused and the board stays up. The slow period runs at once and then every ten
  minutes; it repoints the same way and otherwise only reports.

Not covered, and known: an edit followed by a power cut, or by a plain `reboot` that bypasses
Lemnos, is caught by the next boot's check, not before it. On the U-Boot form the `extlinux.conf`
that Lemnos generates from the command line is not in the manifest, so an edit to that file is not
caught. The repoint from the lemnosd check is a whole-file write of the record, not the one-sector
write the design names for the Raspberry Pi pointer files; the residual window is the same as for a
set_default (see the records section). The fall-back on a slot-init mismatch uses the existing
candidates, not a pointer write.

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

**Paging `list`.** `list` is one reply, so a board whose slots do not fit one reply (about 170 slots
in the bridge's 60 KiB; a slot is about 350 bytes of JSON) uses `list_page`. Its page is the slots
after a cursor, in the order of their install numbers, newest first and ties by id. The first page's
cursor is `-`; each page says `"more":true|false` and `"next":"<seq>:<id>"` (empty when no page
follows), and the next call passes that `next`. Guarantee: the pages are cuts of one order, so every
slot that exists for the whole walk is on exactly one page, and nothing is repeated. A slot installed
during the walk has a larger install number than any listed, so it sorts before the cursor and is on
no later page; a slot removed before its page is simply absent. A cursor that is not `-` or
`<seq>:<id>` is `INVALID_ARGUMENT`; a buffer too small for one slot is `BUFFER_TOO_SMALL`.

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

## Boot history

The history is `lemnos/boots`: one line per boot, kept with the slot records in
`board/slots/boots` on the data partition. When the state directory can't be written, the same
history is kept on the boot partition as `boot-history`, beside `trial-reason`; the reader takes
the copy with the later boot. The loader never reads it.

Each line is seven space-separated fields, a value's whitespace replaced by `_`:

```text
<n> <utc> <slot> <outcome> <reset> <requested> <boot>
```

- `n`: a monotonic boot number, starting at 1.
- `slot`: the slot the boot ran (`-` when none was known).
- `outcome`: `on-trial` while a trial runs unresolved, `confirmed` or `fell-back` once it
  resolves (the entry is rewritten once, when the trial resolves), `unknown` for any other boot.
- `reset`: the board's reset reason, raw: the `reset_reason` file, or the first watchdog's
  `bootstatus`; `none` when neither reads.
- `requested`: `by:<caller>` when the previous run restarted through `system.reboot`, else `-`.
- `boot`: the kernel's boot id. A boot writes one entry, so a driver started again within a
  boot writes none.

The history keeps the newest `boots_keep` boots (16 by default). It is written at the driver's
start, once per boot, after the trial's accounting. It is a record like the slot records: the
rule that drivers keep no state across restarts is about run-time state, not this.

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
The boot history's keys (`boots_keep`, `reset_reason`, and `data` on `system-reboot`) are in [Boot history](boards.md#boot-history-system-slots-system-reboot).
The driver's crate documentation, `drivers/system-slots/src/lib.rs`, lists every setting.

## First flash

A board that may be blank (no partition table, no data partition, no slot) is flashed from a
flasher image: a RAM-only initramfs running `lemnosd`, the bridge and a recovery build of the
layer above. It can't install a slot, because a slot lives on a data partition that isn't there.
So it writes a whole disk image to the board's storage with `system.flash/1`, served by
`system-flash`. The image defines the partitions, the one slot and the boot record; the driver
writes them and invents none of them.

Settings (`system-flash`'s `config`): `path` (the one target, a block device: the only place a path
comes from), `allow_first_flash` (`false` by default: a write is refused unless it is `true`, which
only a flasher's board file sets), `kind` (`block`, `file` for a regular file in tests, or `mtd`
for a raw NAND partition: see "Raw NAND" below),
`max_dict_mb` (8 by default; 1 to 8, the ceiling: a board file may lower it, never raise it),
`keys` (the trusted keys: one per line, 64 hex digits, a space, then the owner), `require_signed`
(`false` by default: the policy table below), and `mountinfo` (the mount table the running-system
check reads, `/proc/self/mountinfo` by default). A board's `[board] id` is given to the driver, and a
manifest for another board is refused first.

### The image

- The image is the whole first-flash disk, as the OS builder lays it out. It is written from byte 0
  of the target. Bytes past its end are left as they were, so the image must cover the target's
  partition table.
- The build produces `image.img.xz`: an xz file whose first stream is the raw image, and which may
  carry one more stream: the signature record (below). `image.img` is raw. The image is one stream
  of one or more xz blocks with check `none`, `CRC32`, `CRC64` or `SHA-256`, and the LZMA2 filter
  only (no BCJ, no delta).
- The xz dictionary is the decoder's memory. A block that declares a dictionary over the board's
  limit is refused at its header, before anything is allocated for it. The decoder's memory is the
  dictionary plus 256 KiB (8.25 MiB at the 8 MiB ceiling).

```text
xz -6 --check=crc64 -T1 -k image.img        # image.img.xz, dictionary 8 MiB
```

A file holds one image stream and optionally one record stream, and nothing else. A third stream,
or data after the second that is not zero padding in fours, is refused as "not a first-flash image".

### The signed message and the record

The signature is Ed25519 over a fixed binary message, which the board rebuilds from its own board id,
its format (`disk` for a block target), and the image's size and SHA-256. The sender never signs a
JSON file:

```text
"lemnos-first-flash\0"    19 bytes: the domain tag, with its NUL
u32 LE version = 1
u32 LE length, then the board id bytes
u32 LE length, then the format bytes ("disk")
u64 LE raw image size
32 bytes raw image SHA-256
```

The record is 108 bytes: `LMNSSIG1`, `u8` algorithm 1 (Ed25519), three zero bytes, the 32-byte key id
(SHA-256 of the public key), and the 64-byte signature. The record is the second xz stream of the
image file, and the same bytes go in the transport manifest's `signature` field (216 lowercase hex
digits). The record stream is built with the same limits as the image:

```text
xz -0 --check=crc64 -c record.bin > record.xz
cat image.img.xz record.xz > signed.img.xz     # stream 1 is the image, stream 2 the record
xz -t signed.img.xz                             # stock xz accepts both
xz -dc --single-stream signed.img.xz | sha256sum  # the image alone
```

### The manifest

The transport JSON, `<name>.json` for a streamed image (see Over Orion), schema 1:

```text
{"schema":1,"board":"<board id>","format":"disk","image":{"size":N,"sha256":"<64 hex>"},
 "signature":"<216 hex: the record, optional; omit when unsigned>"}
```

`format` is the raw image's kind (`disk`), not the compression. `image.size` and `image.sha256` are
stream 1's decoded bytes. The JSON is transport only: the board checks it against the signed message
it rebuilds, and nothing depends on byte-identical JSON. The transport's SHA-256 (Orion's, over the
pushed file) covers the stream as it was sent, and both are checked.

### Signature policy

The board checks, in order, and before the first byte is written:

1. The JSON's `board` is the board's id. Otherwise the manifest is refused by name, "this image is for
   board X, and this is board Y" (`-95`), before any signature is looked at.
2. The record is empty, or 108 bytes with the magic, algorithm 1 and zero padding. Anything else is
   refused in every row: "malformed signature record" (`-22`).
3. The record's key id is looked up in `keys`. A board with no key list has no known keys.
4. The table decides. The size and the raw SHA-256 are enforced during the stream in every row.

| `require_signed` | image | key | result | outcome |
|---|---|---|---|---|
| off | unsigned | – | accepted | `not_checked_unsigned` |
| off | signed | key id in the board's list, signature verifies | accepted | `verified` |
| off | signed | key id in the board's list, signature does NOT verify | refused, before any write | `refused` |
| off | signed | key id unknown, or the board has no key list | accepted | `not_checked_unknown_key` |
| on | unsigned | – | refused, before any write | `refused` |
| on | signed | unknown key or no key list | refused, before any write | `refused` |
| on | signed | known key, bad signature | refused, before any write | `refused` |
| on | signed | known key, verifies | accepted | `verified` |

The outcome is reported as a stable word in `info` (`manifest.signed`, with `manifest.reason`), in
the log, and in the write job's result after its byte count: `0` verified, `1` not checked (unsigned),
`2` not checked (unknown key). The sender's UI shows the word and its reason.

In every accepted row the file's own second stream must be the record given at `manifest`, byte for
byte. An empty record and a file with no second stream count as the same. A raw image has no second
stream, so the record check does not apply to it.

### Calls

1. `manifest(json, record)`: accepts the transport JSON and the record (empty when unsigned, which is
   what the table above is about). Nothing is written.
2. `write(size, format)`: a long job. `size` is the stream's bytes as sent (both streams), `format` is
   1 for xz and 0 for raw. Its progress is the stream bytes taken; its result is the raw image's byte
   count, `u64` little-endian, then the outcome code.
3. `data(offset, bytes)`: the stream in order from offset 0. The last piece ends the stream, and the
   job then checks the image, its record, and ends.
4. `verify()`: optional, a second long job. It reads the target back from its start for the
   manifest's size and checks the hash.
5. The flasher restarts the board. The driver does not restart it.

The job's results: `0` with the raw byte count and the outcome code, after the image's hash matches
and its record is the one given; `-22` for a malformed stream, a stream cut short, a dictionary over
the limit, a size or hash that isn't the manifest's, a stream that holds more than its image, a file
that is not an image (a second stream that is not one record, or a third stream), or a file record
that isn't the one given; `-28` when the image is larger than the target (refused at `write`, nothing
written); `-5` for I/O on the target; `-125` cancelled; `-4` when the service restarted. A write that
fails or is cancelled leaves the target partial, and the state says so: `info` shows `cancelled` or
`failed` and must not be booted. A new write starts again from byte 0. Nothing is kept across a
restart.

`info` (JSON) says whether a write may start (`writable`, and why not in `reason`), the target's size,
whether it holds anything, the state and the last error, the last manifest's facts and outcome, and
the effective `max_dict_mb`.

### Safety

- The target is named only in the board file. No method takes a device path.
- A write is refused when the target is the device the running system is on: a mounted filesystem
  whose device is the target, or a partition of it (its sysfs parent). A flasher runs from RAM, so
  the check passes there; a normal image's root is on the target, and the check refuses.
- `allow_first_flash` must be `true`.
- The image must fit: `image.size` at most the target's size.

### Over Orion

A stream's image is pushed to `lemnos/<device>/<name>` where `<name>` is one of the names
`LEMNOS_ORION_STREAMS` gives the device (for example `flash=image.img.xz+image.img`). Its transport
manifest (`<name>.json`) is pushed first, to the same device, like any small file. There is no
signature sidecar: the record is in the manifest's `signature` field and in the image itself. Then:

1. The bridge reads the manifest, decodes its `signature` field, calls `manifest`, starts the `write`
   job on its own lemnosd connection (it is the job's starter, and only the starter may send `data`),
   and acknowledges the push as received.
2. Each chunk goes to `data` before the bridge acknowledges it, so a slow device slows the sender.
   The bridge holds one chunk at a time.
3. At the commit the bridge checks the stream's SHA-256 against the sender's, then waits for the
   job: the answer is `committed` with the raw byte count and the signature outcome, or `rejected`
   with the job's status or the refusal.

A push that breaks is begun again from zero; a streamed image is not resumed.

### Raw NAND

`kind = "mtd"`: `path` is `/dev/mtdN`, one driver per MTD partition, and a method never takes a
device path. The board file also sets `image` (`"raw"` by default: a linear partition image; or
`"ubi"`: a UBI image of whole erase blocks), and `mark_bad` (`false` by default: when true, a block
whose erase or write fails is marked bad in the chip's table; a wrongly marked block is permanent).
The board's format string, which the signed message is built from, is `mtd-ubi` or `mtd-raw`; a
manifest's `format` must equal it, else it is refused by name (`NOT_SUPPORTED`). The `write`
format argument is still the compression (1 xz, 0 raw).

- Geometry (erase block, write unit, sub-page, size) is read from `/sys/class/mtd/mtdN`. Erase
  blocks up to 8 MiB, write units up to 64 KiB.
- Reads are checked against the chip's ECC counters, not only the data: the character device
  returns the data of an uncorrectable read and counts the failure, so a read that the ECC could
  not correct fails the job as an uncorrectable ECC error. Corrected bit flips are counted.
- Image erase block `i` goes to the `i`-th good erase block; bad blocks come from the chip
  (`MEMGETBADBLOCK`). An image that needs more good blocks than the chip has is refused with
  `NO_SPACE` before any erase.
- UBI: each image block must carry a valid erase-counter header (magic `UBI#`, version 1, the
  header CRC), with the VID and data offsets for the target's sub-page and page sizes. A header
  for another size is refused by name. The first block is checked before anything is erased; each
  later block is checked before its own erase. A target block with a valid header keeps its count:
  it is carried forward (+1, the CRC recomputed). A block with no valid header takes the mean of
  the counts seen on the chip, or the image's own count on a blank chip. Good blocks the image does
  not cover are erased at the end, so no stale volume survives on them for UBI to attach.
- Raw: linear, the last page padded with 0xFF. UBI: trailing erased pages of a block are not
  written, so UBI sees them as free.
- Each good block is erased right before its bytes are written, written page by page, and read
  back in full: ECC-corrected bit flips are counted (`corrected_bits` in `info`); an uncorrectable
  read, or a page that differs, fails the job by name, as does a failed erase or write.
- Running-system check: the partition's `mtdblock` device and any UBI volume attached from it
  (`/sys/class/ubi/ubiN` with `mtd_num`, and its volumes' `dev`) are looked up in the mount table.
- A cut or a cancel leaves the target undefined. A re-run erases and writes from the first good
  block; nothing is kept across a restart.
- Memory: the write holds one erase block and one page beyond the decoder's bound.

### Not delivered

- A UBI attach of the written partition on a board was not run (see `testing/board-runs/W1b.md`).

### After the write

The driver `fsync`s the target and asks the kernel to read its partition table again
(`BLKRRPART`, through `drivers/system-flash-sys`). A kernel that holds a partition open refuses,
and `info` says so: the flasher restarts the board, and that makes the new table the one in use.

## Proof

```sh slow
cargo test -q -p lemnos-driver-system-slots -p lemnos-contract-system-slots -p lemnos-contract-system-reboot -p lemnos-slot-store -p lemnos-slot-init
```

Every test runs over temporary directories that stand in for the two partitions, with a record
fake that writes down what it was told. None of them touches a disk or the boot configuration.
`testing/board-runs/I5.md` is the board run. Until it runs, these drivers are untested on board.
