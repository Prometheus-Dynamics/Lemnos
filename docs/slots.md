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
name. The data partition is not on the command line (see `lemnos-slot-init`, below). The identity device's serial
settings follow as `<boot_args>.serial.<name>=<value>` words, never the serial itself, for the machine id (see
"The machine id").

**Breaking, declared (I5i4):** the bundle's command line is not edited at install, and the
`<boot_args>.data=` argument is gone. A bundle built for an earlier layout is refused.

## A bundle

A bundle is a tar holding, in this order and no other:

1. `slot.json`, the manifest (below). It is the **first** member.
2. `slot.sig`, optional. When present it is the **second** member, right after `slot.json`. It is the
   shared signature record (`lemnos_slot_store::signing`, the same container first flash uses),
   exactly 108 bytes:

   | offset | bytes | content |
   |---|---|---|
   | 0 | 8 | `LMNSSIG1` |
   | 8 | 1 | algorithm: `1` (Ed25519) |
   | 9 | 3 | zero |
   | 12 | 32 | key id: SHA-256 of the signing key's 32-byte public key |
   | 44 | 64 | Ed25519 signature over the signed message (below) |

   A bare 64-byte signature (the first version's form) is malformed: refused, `INVALID_ARGUMENT`.
3. `rootfs.erofs`, the image with its dm-verity hash tree appended, and `boot/`, the slot's boot files
   (the `boot/` directory entry and each file under it). These come **in any order** after the
   signature, and each member at most once.

Any other member is refused by name. A bundle whose order differs from these rules is refused with a
sentence that names the rule it breaks: `slot.json` not first, `slot.sig` not second, a member
repeated.

### Writing one with lemnos-ctl

A format is written by the program that reads it, so a builder does not assemble `slot.json`, the signed
message, the `slot.sig` record or the member order itself: `lemnos-ctl bundle` does, through the same
`lemnos_slot_store::bundle` functions the installer checks with (the manifest's bytes are
`Manifest::to_json`, the message `signing::slot_bundle_message`, the record `signing::record_bytes`, the
tar `tar::write`; the installer's member rules, size rules, content checks and command-line check are the
functions `bundle::check_*` that `finish` calls).

```text
lemnos-ctl bundle --layout-of BOARD [--socs DIR] --slot ID --payload DIR --rootfs FILE --verity-table FILE \
  --product P --version V [--variant TEXT] [--kernel TEXT] [--display-name TEXT] \
  [--build clean|dev-reuse] [--medium sd|emmc|spi-nand|spi-nor] \
  [--key-file FILE [--expect-key-id HEX]] [--payload-out DIR] --out FILE
lemnos-ctl bundle --verify FILE [--layout-of BOARD [--socs DIR]] [--pubkey FILE]
```

The builder supplies the slot id, product, version (and optionally variant, kernel, display name, and the slot's `--build` mode and `--medium`, both written only when given), the
payload directory (the five roles `boot-partition --payload` takes), the erofs image with its hash tree
appended and the table of the verity step (`key value` lines: `hash_type 1`, `algorithm sha256`, `root_hash`,
`salt`, `data_blocks`, `data_block_size 4096`, `hash_block_size 4096`, `hash_offset`, `tree_bytes`,
`image_bytes`, `hash_start_block`; `uuid` and `table` are ignored). The tool derives `schema`, `board` and
`boot.overlay` (from the board file, as `tree --manifest-boot` does), `boot.files` (exactly the five role
files, in role order, with the SHA-256 of each as stored), `image.size`, `image.sha256`, the `verity` object,
the key id and the signature. It fills the command line's `<prefix>.roothash=@roothash@` hole (the one
`tree --cmdline-slot` writes) with the table's root hash; a line with no hole must already carry that hash.
`--payload-out DIR` writes the five boot files as the bundle holds them, so the first slot's boot partition
(`boot-partition --payload`) carries the same bytes as its bundle.

Before anything is written it refuses, by name: a payload directory with a missing or extra file (the same
rule as `boot-partition`); the wrong kernel form for the board's record (the Pi firmware form stores the
kernel as shipped, gzip allowed; the U-Boot form refuses a compressed one); a file that does not fit its bay;
a command line that does not name its slot, root hash and the board's boot sources; a slot id, product or
version that is not valid; a table that contradicts itself (`hash_offset` is `data_blocks` times 4096,
`tree_bytes` is the manifest's own computation, `image_bytes` is both, `hash_start_block` is `hash_offset`
over 4096) or is not format 1, SHA-256, 4096-byte blocks; an image whose length is not the table's
`image_bytes`, whose erofs superblock does not give the table's data length, whose tree is not the tree of its
data or whose root hash is not the table's. The tree is recomputed in the tool (SHA-256 only, `slot-store`'s
`verity`, checked against the `veritysetup` vectors in this document).

The key file is a raw Ed25519 seed: exactly 32 bytes or 64 hex digits, one trailing newline allowed. Its
path is on the command line; its bytes never are, and the tool never prints them. A file readable by group or
others is refused (`the key file PATH is mode 0644: it must not be readable by group or others (0600 or
0400)`). That is the only thing checked about the file: the group and other permission bits of the opened file (so a 0400 file, a read-only mount or another owner is accepted). A minisign key file is not read. The key id (SHA-256 of the public key, `signing::key_id`) is
printed; `--expect-key-id HEX` refuses, before signing, a key whose id differs. Without `--key-file` the bundle
is unsigned and the output says `unsigned`.

`--verify` runs the installer's checks short of installing and prints one line per check (`ok `, `BAD` or
`n/a `, then `check: what was found`), then `ok` or `refused: N`; it exits 0 or 1. The checks are `members`
(framing and order), `manifest`, `signature`, `image`, `verity` (beyond the installer: the tree after the
image recomputes to the manifest's root hash), `boot files`, `roles` (`boot.files` is exactly the five
role files), and with `--layout-of` `board`, `kernel`, `bays`, `cmdline` and `overlay`. It reports the
signature; it does not apply the board's security switch, which is the board's policy: unsigned is `ok`, a
valid signature is `signed by key id X: signature valid`, a key it has no public half for (`--pubkey FILE`, a
bare 64-hex public key or a key list, or the board's `[security] keys` file) is `n/a ... not checked`, and a
bad signature is `BAD`.

### The signed message

The signature is Ed25519 over a domain-separated message, built by
`lemnos_slot_store::signing::slot_bundle_message` (one builder for every signed kind: a `Domain` enum
gives each kind its tag, so a signature for one kind never verifies as another under the same key;
first flash's tag is `lemnos-first-flash\0`, and its message is unchanged):

```text
"lemnos-slot-bundle\0"    19 bytes: the domain tag, with its NUL
u32 LE version = 1
u32 LE length, then the manifest's exact bytes (slot.json as the bundle holds them)
```

Vector: the test key is the 32-byte seed `0x07` repeated (public key
`ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c`, key id
`fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889`). For the manifest `{"id":"vector"}`
(15 bytes):

```text
message (42 bytes):
6c656d6e6f732d736c6f742d62756e646c6500 01000000 0f000000 7b226964223a22766563746f72227d
record (108 bytes):
4c4d4e535349473101000000fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889
56be579db020703eb207da579036a89f0321fcd8ce2bac78a9aa939a58217e265ad27a7e1fc05829b0db05d37ac4d8dba35263c7a1b2644322b45ada4a101402
```

The record's signature is over those 42 bytes. A signature over the bare manifest (no tag) does not
verify. Tests: `the_slot_bundle_vector_is_the_documented_record` (system-slots),
`the_slot_bundle_message_is_tagged_and_length_prefixed` and `the_domain_tags_differ_so_no_kind_verifies_as_another`
(slot-store), `a_signature_over_the_bare_manifest_is_refused_as_a_bad_signature_under_a_known_key`.

### When the signature is decided

The signature policy is decided while the bundle comes, before any payload byte is written:

- at the end of `slot.sig`, when the bundle is signed; or
- at the first payload member (`rootfs.erofs` or a `boot/` entry), when it is not.

So a bundle the policy refuses writes nothing: its work directory is empty and its root image is not
begun. The board's own checks run at the same point (the manifest's `board` against the board id, and
the slot id's form at `slot.json`). Tests: `a_refused_bundle_writes_nothing_and_the_root_image_is_not_consumed`,
`slot_json_must_be_the_first_member`, `slot_sig_must_be_the_second_member`,
`a_repeated_member_or_an_unknown_one_is_refused_by_name`.

### What the signature binds

The signature covers `slot.json`, so everything the bundle's other members are checked against is
bound through it. Nothing else is trusted:

| item | bound by | checked |
|---|---|---|
| the board | `board` | against the board file's `[board] id`, at the decision (before payload) |
| the slot id | `id` | its form (`valid_id`) at `slot.json`; not already running or default |
| the root image | `image.size`, `image.sha256` | its size and SHA-256 at `finish`, before the slot is put in place |
| dm-verity | `verity.root_hash`, `salt`, `data_blocks`, `hash_offset` | the image size is the data plus its tree; the root hash goes in the command line |
| each boot file | `boot.files`: path to SHA-256 | the file's SHA-256 (so its length too) at `finish`; a file the manifest does not list is refused at its header, before its bytes are written |
| a listed member | `boot.files`, `image` | a listed member the bundle lacks is refused at `finish`, before the slot is put in place |
| the set of boot files | `boot.files` | exactly the board's declared set: the five role files the boot partition stores (`kernel8.img`, `initramfs.cpio`, `board.dtb`, `cmdline.txt`, `overlays/lemnos-board.dtbo`) and each firmware overlay the board's `[boot] firmware_overlays` names, on every form; an entry outside that set, or a declared file the list leaves out, is refused by name at the decision, before the first payload byte (see "Manifest and members") |
| the firmware config lines | `boot.config` (SHA-256 of the board's `[boot] config_lines`, absent for a board with none) | equal to the boot partition's digest, or refused by name with both digests, before the first payload byte (see "Firmware overlays and config lines") |
| the kernel command line | `boot/cmdline.txt` (a listed file, so digested), `verity.root_hash`, `id`, the board's `boot_args` and PARTUUID | built only from those; no member outside the list reaches it |

No member is installed unsigned: with the security switch off, an unsigned bundle installs as
`not_checked_unsigned`, and then nothing in it is authenticated, its manifest included.

### Manifest and members

The manifest is checked in full at `slot.json` (`Manifest::parse`, `slot-store/src/manifest.rs`); a
malformed one is refused before anything else arrives. Then each member is checked against it as above.
Each member's bytes are hashed as they arrive; `finish` compares them with the digests before
`boot::put` and the rename that puts the slot in place, and a bundle that differs is refused with
nothing left behind.

**`boot.files` is exactly the role files.** A slot's boot files live in a bay, and a bay stores five files,
one per role: `kernel8.img`, `initramfs.cpio`, `board.dtb`, `cmdline.txt` and `overlays/lemnos-board.dtbo`
(the same names on the Pi firmware form, the U-Boot form and the file form, which keeps no bay). The
every-boot check hashes each `boot.files` entry from the bay, so a manifest that lists any other file (a
second overlay, `overlays/README`, anything else under `boot/`) would install and then fail its first boot.
The install refuses it instead, at the same point as the signature and the board (before any payload
byte, on the stream and on a file in `incoming`, so nothing is written and the default keeps booting):
`<slot>: boot.files lists "overlays/extra.dtbo", which this board's boot partition does not store (it
stores: kernel8.img, initramfs.cpio, board.dtb, cmdline.txt, overlays/lemnos-board.dtbo): nothing was
written, the default keeps booting`. A role file the list leaves out is refused the same way
(`boot.files does not list "board.dtb", which this board's boot partition stores`). The board writes
`overlays/README` itself; a bundle does not carry it. One function, `lemnos_slot_store::bays::refuse_unstored`,
holds the rule; the check at boot names a listed file no bay stores as `not stored` when it meets one
(a slot installed before the rule).

### Firmware overlays and config lines

A board may declare two more things the firmware needs ([Boards](boards.md), `[boot]`): `firmware_overlays`,
overlay names each of which is a **bay member** in every bay set (`overlays/<name>.dtbo`, the size of the
board overlay's bay), and `config_lines`, verbatim config.txt lines written inside `[all]`. A board with
neither is written exactly as before: no firmware list file, no `boot.config`, five members.

- **Members.** The boot partition's declared set is the five roles, then the overlays in the board's order.
  Each overlay is hashed in the set's record (`bayinfo.rec` holds the names, lengths and hashes after the
  roles, behind a count byte; a five-role record is read as before). `bays::compare_hashes` checks them on a
  reinstall, a bundle's overlay is refused by name when its bay holds another build, and the every-boot
  check and `boot_view` read each overlay from the bay. A record member the manifest does not list is a
  mismatch (`is stored in the bay but not listed in boot.files`).
- **The list at the root.** A board that declares either key has a root file `firmware-overlays.txt`: a
  `config <sha256>` line when it declares config lines, then one `overlay <name>` line per overlay. The
  boot partition's own code (`bays::discover`) reads it, so the U-Boot form and the Pi form, and every
  reader of the bays, see the same declared set and digest. The image's `--verify` checks it as a root file.
- **Config lines are fixed at flash.** The lines are in the root `config.txt` and `tryboot.txt`, so every
  slot on the partition boots with the same lines. The bundle records their digest in `boot.config`, and
  the install compares it with the partition's: a bundle whose lines differ is refused with both digests
  named (`the bundle's firmware config lines are sha256 … and the boot partition's are … : the boot
  partition's lines are fixed at flash`), before the first payload byte. A slot that needs other lines needs
  a new boot partition. (A pointer-file design, with the lines in each pointer, was checked and not used:
  the firmware's documentation says bootloader-read settings take effect only in `config.txt` itself, not in
  an included file, and the pointer sector's parser refuses any line but `os_prefix` and its checksum.)
- **Checked at build, by name.** A `dtoverlay=NAME[,params]` line in `config_lines` whose NAME is not in
  `firmware_overlays` is refused, and so is a listed overlay that no `dtoverlay=` line loads. A config line
  that is not one line is refused. A missing `<name>.dtbo` in the `--firmware DIR` is refused by name.
- **Where the files come from.** `lemnos-ctl boot-partition --firmware DIR` and `lemnos-ctl bundle --firmware
  DIR` read `DIR/<name>.dtbo` for each declared overlay, so the bay and the bundle hold the same bytes.
- **Checked on the host, not by firmware.** `lemnos-ctl boot-partition --verify FILE --layout-of BOARD --dtmerge TOOL`
  applies the declared overlays to each committed bay's `board.dtb` with `TOOL` (dtmerge's command line, the
  parameters from the `dtoverlay=` line), then checks the board's `[boot] expect_nodes` in the merged tree. A
  failed overlay is named with the tool's status. Nothing here is loaded by firmware: the lines say so. The
  firmware itself applies the overlays at boot and is not checked by this.

### The manifest, schema 3

| Key | Format |
| --- | --- |
| `schema` | `3`. Any other schema is refused with a sentence saying so |
| `id` | the slot id: lowercase `[a-z0-9._-]`, 1 to 64 bytes, not starting with a dot (`valid_id`) |
| `product` | the operating system's product name, lowercase `[a-z0-9-]` |
| `version` | the operating system's version, text: not empty, at most 256 bytes, no control character (a byte under 0x20 or 0x7f). The identity fact `version.os` takes it as it is and would drop a longer or controlled one quietly, so the manifest refuses it (`slot.json's version is N bytes; at most 256` / `... has a control character at byte N`) |
| `variant` | optional text |
| `board` | the board the slot is built for, lowercase `[a-z0-9-]` |
| `kernel`, `display_name` | optional text, shown by `list` |
| `build.mode` | optional: `clean` or `dev-reuse`, what `lemnos-ctl bundle --build` takes. The reader keeps any other text and shows it as written. A `dev-reuse` slot is always unsigned: `--build dev-reuse` with `--key-file` is refused |
| `medium` | optional: `sd`, `emmc`, `spi-nand` or `spi-nor`, the board file's `[flash] medium` values (`lemnos-ctl bundle --medium`). The reader keeps the text as written; a board refuses a slot of another medium (see "The medium check") |
| `image.size` | `rootfs.erofs`'s size in bytes, hash tree included |
| `image.sha256` | `rootfs.erofs`'s SHA-256, 64 lowercase hex digits |
| `verity.root_hash` | 64 lowercase hex digits |
| `verity.salt` | exactly 64 lowercase hex digits (32 bytes), unique per image, required. It may be derived from the image's content (a reproducible build then gives the same salt and root hash); it need not be secret or unpredictable: the image is public and the root hash is what the signed manifest binds |
| `verity.data_blocks` | how many 4096-byte data blocks the tree covers |
| `verity.hash_offset` | the byte offset in `rootfs.erofs` where the hash tree starts |
| `boot.files` | each file's path under `boot/` to its SHA-256 |

**Installing an id that is already on the board**, exactly as the code behaves. Nothing compares versions: the
install counter (`SEQ`) orders slots.

| The id is... | Result |
|---|---|
| the running slot | refused at `slot.json`, `BUSY`: `<id> is the running slot` |
| the default slot | refused at `slot.json`, `BUSY`: `<id> is the default slot` |
| the previous slot (a bay form) | refused when the payloads are staged, after the bundle came, before any write: `bay<N> is the previous slot's bay: a new install takes one of the other bays` |
| any id, while a trial is pending | refused before any write: `slot <trial> is on trial with <N> attempts left: nothing was written; wait until the trial is promoted or falls back, then install <id> again` |
| the default's bay holds the id, and a boot file's stored hash differs from the bundle's | refused when the payloads are staged, before any write, naming the first file that differs: `bay<N> holds boot file <path> with a different hash than slot.json says: nothing was written, the default keeps booting`. The daemon refuses the default id above, so this is the bay layer's own check, the one any caller of the bay install gets |
| the default's bay holds the id with identical boot files | accepted and nothing is written (`AlreadyInstalled`) |
| installed, and none of the above (a `FAILED` slot included) | replaced in place: the same bay set, the image and manifest renamed over the old ones, fresh metadata (the `FAILED` mark and its count are gone, `GOOD` is `untried`) and a new `SEQ`; the default is not touched. The bay's stored hashes are not compared here: a bay that is neither the default, the previous nor the trial is rewritten, as the install of any id is |

A manifest with a `family` key is refused. That key belonged to schema 2. Any other top-level key is accepted
and ignored by the reader (a `build` object, for one), and `lemnos-ctl bundle` does not carry it: the bundle's
manifest is rebuilt from the fixed fields above (`Manifest::to_json`).

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
SALT=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')   # one way: a new random salt per image
# another: SALT=$(sha256sum rootfs.erofs | cut -d' ' -f1)       # derived from the content: a rebuild is byte-identical
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

The switch is not a board-file setting of a driver. It is the view `/etc/board/security`, written from the
record `[security] record` by the `--security-unit` step at boot, and read by every reader through
`lemnos_slot_store::security::enforced`. Switching it on is the one switch of the security device (see
"The security switch"); the per-driver `require_signed` is removed.

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

The slot driver's readers read the switch through `lemnos_slot_store::security::enforced` (the view).
The boot side (slot-init, which runs before the daemon) reads the boot mark `board/slots/require-signed`,
which the change writes itself, in the order given in "The order of a change" below, and which the daemon
repairs from the record at each start. So a switch changed while the daemon runs is in force at the next
boot, even if the board restarts before the daemon does. Orion has its own security switch (`security`, read by its
node's keys service; the Lemnos bridge does not read it), and the two are not yet one setting. That is
a follow-up.

### What this protects, and what it doesn't

The root hash on the kernel command line is only as trustworthy as the boot partition that holds
the kernel, the initramfs and the command line. dm-verity here gives integrity against
corruption, and against tampering with the data partition alone. It is **not** verified boot,
unless the SoC's secure boot verifies the boot partition. When the security switch is on, the
signature on `slot.json` protects what gets installed, not what boots.

Signing is a board setting, and the board has one trust root: the `[security] keys` file (the board file's
`[security]` table, "The security switch" below). The host gives that one file to `system-slots`,
`system-flash` and `system-security`, and refuses a `keys` setting in any of those devices' own config, so
there is no second key list. The same keys verify a slot bundle, a first-flash image and the authorization
that turns the switch off. The file lists the keys the board trusts, one per line: 64 hex
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
- `"build"` and `"medium"`: the slot's `build.mode` and `medium` as its `slot.json` says them, shown
  beside the signature. A slot that records neither reads `not recorded`. `info` adds
  `default_build`, `default_medium` (the default's, the same words) and `booted_medium` (see "The
  medium check").

### The medium check

A slot's `medium` is compared with the medium the board booted from. An install is refused before its
first payload byte, and a trial is refused before anything is written; both are `NOT_SUPPORTED`, with
the reason in `list`'s `error` and the slot's name and both media in it, for example
`slot os-2 is for medium emmc, and this board booted from sd`. The booted medium is the one of the
disk the data partition is mounted from, since a slot's image is a file there: `/proc/self/mountinfo`
gives its device, and sysfs its controller. An SD card's host reports `SD` and an eMMC `MMC`
(`/sys/block/<disk>/device/type`); a raw flash MTD reports `nor` or `nand`. When the board's medium
cannot be told (no data mount, a UBI data partition, another controller), `info` shows
`booted_medium` as `unknown` and nothing is refused on medium. A slot that records no medium is
never refused on it.

The K230's boot strap register (`SOC_BOOT_CTL`, `0x91102040`, bits 0-1: 0 SPI-NOR, 1 SPI-NAND, 2 SDIO0,
3 SDIO1) is not used. Two reasons: the SoC map (`socs/*.toml`) has no field for register facts, and the
strap fact is not verified locally (the SDK source it is quoted from is not on this machine). The medium
comes from the data disk's controller instead. The strap is a possible later source for the booted medium,
once a map field and a check against the source exist; U-Boot's environment is never trusted for it, since
the canmv board code forces SDIO1.

`info` shows the policy, the last install's signature result and, last, how the last install attempt ended (`last_attempt`, see "The last install attempt" below): `require_signed` (now always the security view's value, see "The security switch"), and
`last_install` with `slot` (empty when refused), `signature` (the outcome, or `refused`), `signer`
and `reason`. The install's job result is still the slot's id.

A client learns the board's state from `info` before it sends a bundle: `require_signed` (the security switch's value, the
policy), and `key_ids`, the keys the board holds, each as `{"id":"<64 hex: SHA-256 of the public key>",
"owner":"<the key file's owner>"}`. A client picks the key it signs with by its id, and never sees a
key. `key_ids` is appended last, so the fields before it are unchanged (additive; `info` stays within
its 4 KiB buffer for a board with a few keys).

With the security switch on, only bundles signed by a trusted key install. The switch is off by
default: see "The security switch" below.

### The last install attempt

A slot install is a long job, and a job belongs to its starter's connection: a session that did not
start it reads only its id, state and progress while it runs, and when the starter's connection ends
the host cancels it (`docs/daemon.md`, "What a session that did not start a job can do with its id").
An app that reconnects after an install therefore sees only the slot store: a new slot when it worked,
and nothing when it did not. So how the last install attempt ended is a fact about the device, and
`info` (method 14) states it for any session, appended after `key_ids` (additive: the fields before it
are unchanged and the contract's hash does not move):

```text
"last_attempt":{"state":"failed","slot":"os-2","reason":"rootfs.erofs doesn't match slot.json's sha256",
 "received":41943040,"total":104857600,"boot":"0123abcd-4567-89ab-cdef-000000000000",
 "started_ms":1203400,"ended_ms":1234500,
 "text":"last install: slot os-2 failed: rootfs.erofs doesn't match slot.json's sha256 (41943040 of 104857600 bytes; boot 0123abcd, +1234.5 s)"}
```

- `state`: `none` (this daemon knows of no attempt since it started: the text is `last install: none since the service started (boot ID8)` and `boot` is set; it does not say no install ever ended before the daemon started), `running`, `installed`, `refused`, `cancelled`, `interrupted`
  or `failed`.
  - `refused`: turned away by what the bundle says or the board requires, before the image was
    trusted: the signature or the policy, the manifest, the order of the members, a slot that is the
    running or default one, a boot file the manifest does not list, the boot loader's files, a payload
    that does not fit its bay, no room, a board that requires signing and holds no key.
  - `failed`: the work went wrong: the disk, an image or boot file that does not match the manifest, a
    bundle that ends in the middle, a bundle file that could not be read.
  - `cancelled`: the starter cancelled it, or its connection ended and the host cancelled it.
  - `interrupted`: the daemon restarted in the middle of it (see below).
  - `installed`: the slot is in place.
- `slot`: the slot id the attempt was for, from the bundle's `slot.json` (empty when the attempt ended
  before that member was read, or was turned away before it started). At most 64 bytes.
- `reason`: the words the starter was given: the board's state `error` that `list` shows for a refusal
  or a failure, and for a cancel `cancelled by the caller that started it` or, when the starter's
  connection ended, `the caller that started it went away`. Empty for `installed` and `running`.
  At most 192 bytes: a longer reason is cut on a character boundary and ends with `...` (the state
  keeps the whole text).
- `received` and `total`: the bytes taken and the bytes announced, the numbers the job's progress
  shows. For a refusal they are the bytes taken before it.
- `boot`, `started_ms`, `ended_ms`: when, by the host's monotonic clock (`CLOCK_MONOTONIC`, in
  milliseconds from the start of the boot `boot` names), never wall time. `ended_ms` is `null` while it runs.
- `text`: the record as one line, for a person: `last install: slot ID STATE: REASON (RECEIVED of TOTAL
  bytes; boot ID8, +SECONDS s)`, the reason and its colon left out when empty. `lemnos-ctl call slots info`
  prints it with the rest of the reply; there is no other text form.

Every install path records it: a streamed `install`, an `install_file`, and the Orion bridge's, which
reach the driver the same way. A new attempt replaces the record the moment it starts, so while an
install runs `info` says `running` with the job's own `received` and `total` (the figures `list` and
`describe` show for the job; the two never disagree) and the slot once `slot.json` has come. An
`install` turned away as `BUSY` is not an attempt and leaves the record alone.

**What survives a restart.** The record is kept in memory, for the daemon's life: the slot store
persists nothing about an install but the board's state record (`state.json`, on the data partition),
which holds a word and one error text, not the slot, the bytes or the time, and no new file or boot
partition write was added for this. After a restart `info` says `none` (since the service started, which is all it can know), with one exception: an install the
restart cut short. Its work directory is found and removed when the driver starts, and the state
record is set to `error` with the text `an install was interrupted: the service restarted`; the driver
reads that record when it starts and reports `interrupted` with that reason, an empty `slot`, and
`null` bytes and times. That holds until a later install or board operation rewrites the state record.
An install that ended any other way before the restart is not known after it.

## The security switch

One switch per board, off by default, for everything the board gates on security: the slot driver
and the first-flash driver. The switch is served by its own device, `system-security`, which serves
`system.security/1`. The board file says where its facts are, in a `[security]` table, and the
device takes them as its config (the host gives them to the one device that serves the contract):

```toml
[security]
record = "/var/lib/lemnos/security.rec"   # absolute: the switch's truth; "boot" on a slot board (below)
keys = "/etc/lemnos/slot-keys"            # absolute, optional: the board's trust root (see "Turning it off")
orion = false                             # true: Orion ships here, and answers before a switch goes on

[[devices]]
id = "security"
driver = "system-security"
uses = { slots = "slots", flash = "flash", identity = "identity" }   # the stakeholders, and the board's identity
```

- `uses` names the stakeholders: the `system-slots` and `system-flash` devices, which serve
  `system.security-precondition/1`. Each answers only for itself. Both are optional needs. A board with
  `slots` and no flash device names only `slots`: `precheck` and `set` ask `slots` alone (its answer, and
  its boot mark through `apply`), `flash` is not asked and is not a fault, and no trust-root answer of
  the switch's own is added while `slots` answers. A board with neither bound judges on its trust root
  alone. Naming a stakeholder the board does not have (`flash = "flash"` with no such device) is refused
  by name at validation and at start, not run degraded: leave the key out instead. `identity` is the board's `system-identity` device: its serial names the
  board in an authorization to turn the switch off (below). Without it an off on a board with a trust
  root cannot be authorized.
- With `orion = false` in `[security]` the switch can be turned on while Orion on that board stays
  open, so it says so. `state` has the line `orion: not covered by this switch on this board (the board
  file says orion = false)` after `orion absent`, and the reply to `set` to on is
  `note: orion: not covered by this switch on this board (the board file says orion = false)`. A change
  to off replies empty. With `orion = true` there is no such line: a change to on asks Orion, and is refused
  by name unless Orion says yes.
- The view is not configurable. It is rendered into `/run/board`, which the image links `/etc/board`
  to. A board file that names a view or a render directory (`security_view`, `run`) is refused by name
  or does not start the driver: no board file can point the switch's readers somewhere else.
  `require_signed` is refused the same way.

**The view** is `/etc/board/security`: exactly two lines, `on` or `off`, then `generation N`. Absent
is off. A file that is present but unreadable or not exactly those two lines is corrupt, and corrupt
is on. Every reader calls one function, `lemnos_slot_store::security::enforced_at`, with the view's
path. The directory `/etc/board/` is what gets watched: the image makes it a symlink to the tmpfs
directory `/run/board/`, and the view is replaced by writing a new file beside it and renaming it
over. The inode changes on each render, and a reader never sees a partial file.

**The truth** is a record of two 512-byte copies, each with a CRC and a sequence number (the
generation), so a power cut leaves the old value or the new one. The first write is a temporary file
renamed into place, so a cut before the rename leaves no record (absent, off). A record that is
present but has no valid copy reads as corrupt (on). The record is written before the view is
rendered; if the render fails the view is `pending`, the record stands, and each later call retries
the render. The daemon renders the view from the record again at start.

**On a slot board the truth is the boot partition's `SECURE.REC`**: the same two-copy record, in the
boot partition's file of that name, which the image builder pre-creates as off (two copies, at sequences 0 and 1, so a new image reads off at
generation 1). The
security device takes it when its own `record` is `"boot"` (the board's `[security] record` is
`"boot"` too, which the host gives it). The device finds the boot partition's node the way the slots
device does, from its own `boot_sources` config. A node that cannot be found, or an image without
`SECURE.REC`, reads as corrupt (on), reported; the switch is never off by accident.
`lemnos-ctl security render` takes exactly one of `--record FILE` (a file) and `--boot-image FILE` (the
`SECURE.REC` of a boot partition image). `lemnos-ctl security set --boot-image FILE on|off` writes that
image's record offline. On a board the switch is set through the daemon, as before. A missing or
unreadable record reads as on, and the render line says so.

**A change** (`set`, `lemnos-ctl security DEVICE on|off`) runs: the stakeholders' prechecks; Orion's
answer when Orion is on the board; for an off on a board with a trust root, the authorization (below);
the record; the render. Refused by name when a fact stops it. Refused in either direction while an
install or a trial is running, or a write or verify is running on first flash. An off has no other
precondition. On needs:

- a trust root: the board's `[security] keys` list has a valid line (one list, seen by each stakeholder
  and by the switch);
- on slots, the default slot and the running slot `verified` under that list;
- Orion's answer, when `orion = true`. The precheck is at Orion's pinned revision (`172054f4`, where
  `security_precheck` is interface level 5, the level `drivers/orion-precheck` asks for; "The Orion precheck").
  A change to on is refused by name when Orion says no, is below level 5, does not answer, or no `orion`
  device is bound.

### Turning it off

Turning the switch on is free of authorization: it only raises protection, and it already needs the
proof above. **Turning it off needs an authorization signed under the board's trust root** when the
board holds one. The board holds a trust root when its `[security] keys` file (the one list the slot and
flash drivers verify against) has at least one valid key line. Without a trust root, off needs no authorization, and one given is ignored (the
reply says so). That is the never-strand case: a board that has never had keys is never locked.

The authorization is an ordinary signature record, the form every signature in this document takes
(108 bytes, `docs/slots.md`, "The signed message and the record" and the signing table in "Signing"):

| offset | bytes | field |
|---|---|---|
| 0 | 8 | `LMNSSIG1` |
| 8 | 1 | algorithm: `1` = Ed25519 |
| 9 | 3 | zero |
| 12 | 32 | key id: SHA-256 of the signing public key (a key the board holds) |
| 44 | 64 | Ed25519 signature over the message below |

The signed message is built by `lemnos_slot_store::signing::security_switch_message`, in its own domain
(`lemnos-security-switch`), so a signature made for any other kind of message never verifies here:

| offset | bytes | field |
|---|---|---|
| 0 | 23 | `lemnos-security-switch\0`: the domain tag, with its NUL |
| 23 | 4 | u32 LE version = `1` |
| 27 | 4 | u32 LE length `L` of the board's serial |
| 31 | `L` | the board's serial, as `system.identity/1` `serial` gives it (lowercase hex) |
| 31+`L` | 1 | target: `0` = off (this message is only for off) |
| 32+`L` | 8 | u64 LE generation: the switch record's sequence number when the change is made |

**The board's identity is its serial**, not its family: the family (`[board] id`) is the model, the
same on every unit, and the serial is per unit (the source the board file gives `system-identity`).
So a signature made for one unit verifies on no other. A board with no serial (the identity device
absent or not answering) cannot be named, so an off on it with a trust root is refused: see "lost key",
below.

**The generation** is what binds the authorization to a moment. Each change of the record moves the
generation by exactly one, so after the change an authorization for the old generation no longer
verifies: it is spent by the change it allowed.

**Test vector.** Test key = seed `0x07` repeated (32 bytes of `07`; public key `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c`; key id `fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889`).
Serial `0123456789abcdef`, target off, generation 1. Message (56 bytes):
`6c656d6e6f732d73656375726974792d73776974636800010000001000000030313233343536373839616263646566000100000000000000`.
Record (108 bytes): `4c4d4e535349473101000000fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889`
followed by the 64-byte signature `f42567f5963b8e7333394a8c248e380617a7d964d5e5509b8f99dbe56c9e32376275c457f831a4163d5e959a584694e06fab342ebf036002b97d4190888aad00`.
Checked by a Rust test (`the_documented_vector_is_the_message_and_the_record`) and by an independent
Ed25519 implementation.

**What the check does** (`set(off, authorization)`): when the board holds a trust root, it reads the
record's current generation and the board's serial, then:

| refusal (`NOT_PERMITTED` unless noted) | the reason, in words |
|---|---|
| no serial | `missing: this board has no serial, so no authorization can name it: the switch cannot be turned off by software (reflash over USB boot, docs/slots.md)` |
| nothing given | `missing: no authorization given: off needs one signed under this board's trust root for generation N (lemnos-ctl security DEVICE request-off)` |
| not a 108-byte record (`INVALID_ARGUMENT`) | `malformed authorization: <why>: a 108-byte signature record is needed (LMNSSIG1, algorithm 1, zero padding)` |
| key id not in the trust root | `signed by a key this board does not hold: the key id is not on this board's trust root` |
| held key, the message is for on | `made for another state: it turns the switch on, not off` |
| held key, the message is for an earlier generation | `made for generation G, the switch is at M: the switch changed after it was signed` (the earlier generations are tried, up to 4096) |
| held key, none of the above | `signature does not verify for this board: it was made for another board, or it is corrupted` |

The last line is honest about what the board can see: the authorization carries no fields, so a
signature made for another board fails here the same way as a corrupt one. The text does not claim
more than that. A pass is the only way to a change; a refusal changes nothing, and the record is not
written.

**The request.** `lemnos-ctl security DEVICE request-off` (the device's `request_off`) prints, from the
board, what a signer must sign: the target, the generation, the serial, the message in hex, and the
keys the board holds (`key <id> <owner>`). A signer takes the message bytes, signs them with one of
those keys, and writes the 108-byte record: no field is guessed. Without a trust root it says that off
needs no authorization. It is read only: it changes nothing.

**The CLI.** `lemnos-ctl security DEVICE off --authorization FILE` sends the file (its bytes, as hex)
to the daemon's `set`; with a trust root, `off` without `--authorization` is refused by name. `on` takes
no authorization (`--authorization` with `on` is refused by the CLI). `state` names the serial, the
trust root, each held key id and owner, and the generation. `lemnos-ctl security render --record FILE`
reads the record and renders the view: it never writes the record, so it cannot turn the switch off.

**On is unchanged.** Turning on takes no authorization: it needs the proof of the stakeholders and the
trust root, which raise protection only. An authorization given with an on is ignored, and the reply says so.

**A lost key.** If every key the board holds is lost, no authorization can be made, and an off is
impossible by software. Nothing in software bypasses this, by design. The way out is physical: a
reflash over USB boot writes the board's image afresh, and the switch's record is part of what it
writes, so the switch comes back off at generation 0 (absent). Until then the switch stays on and its
readers stay enforcing.

**Crash safety.** An off checks the authorization first, before any step runs, then writes in the order
"The order of a change" gives: the record (the truth: the generation moves by one, the value is off), then
the view (rendered from the record; if the render fails the view is `pending` and the record stands), then
the boot mark. A power cut at any point leaves the board on, or consistently off, and the same
authorization ends in the same state. The view rule keeps a cut from reading as off by mistake: a view
that is absent reads off, one that is malformed reads **on**, and the daemon renders it from the record at
each start.

| the cut comes | the record reads | the view | the boot mark | the authorization |
|---|---|---|---|---|
| before the record's sector is complete (any byte) | on, generation G | old (on) | on | still verifies: a retry ends at off, G+1 |
| after the record, before the view | off, G+1 | stale (on) | on | spent |
| after the view, before the mark | off, G+1 | off | on (stricter) | spent |
| after the mark | off, G+1 | off | off | spent |

After a cut past the record, the next start renders the view and clears the mark from the record. The
board is never "the view says off while the record says on": the view is written only after the record.
A spent authorization is refused with `made for generation G, the switch is at G+1`, so it cannot be
replayed to any other end state, and a replay after a later on is refused the same way.

Tests: a cut at every byte of the record write
(`a_cut_at_every_byte_of_the_off_leaves_the_same_end_state_or_a_spent_authorization`), a power cut before
each step of an authorized off (`an_authorized_off_cut_before_each_step_ends_consistently_and_the_authorization_follows_the_record`)
and a failed render (`a_cut_after_the_record_and_before_the_view_leaves_the_view_pending_and_the_authorization_spent`).

**Why not a binding on on.** An on binds nothing: it only raises protection, and its precheck already
names what is missing. An authorization for on would add a key-holder's step to every enable for no
gain in protection.

`lemnos-ctl security DEVICE precheck on|off` says what would stop a change, with no change made. The
switch applies at once: the slot and flash drivers read the view at each decision, with no restart.

`lemnos-ctl security render --record FILE [--dir DIR]` renders the view from a record with no
daemon. `lemnos-ctl tree BOARD --security-unit FILE` writes the systemd unit that runs it before the
daemon and the comm core start (`packaging/README.md`, "The security view"). Orion reaches the switch
through the bridge's generic `<resource>.call` action: `precheck on|off` and `set on|off` are methods of
the device, so the bridge forwards them like any other.

With the switch on, only bundles signed by a trusted key install, and first flash refuses unsigned
images (the policy table in "Signing" and "First flash" above).

The board file's `require_signed` (the old per-device key on `system-slots` and `system-flash`) is
removed. A driver whose config still has it does not start, and `lemnos-ctl validate` names it.
Replace it with the switch.

### A malformed view, and the way back

The readers read the view file, `/etc/board/security` (the image links it to `/run/board/security`).
Its one form is `on` or `off`, then `generation N`. A file that is absent reads off. A file that is
present but unreadable, or not exactly those two lines, reads **on**, whatever the record says. The
record is the truth; the view is its copy for the readers, rendered at each daemon start and each
change, and by `lemnos-ctl security render` (below).

A board in that state, with no trust root (no valid line in `[security] keys`):

- **What a person sees.** `lemnos-ctl security state` (method `state` of `system.security/1`) says
  `view corrupt: readers read on, whatever the record says` and names the render command, next to
  `switch` (the record's value). `info` (`system.slots/1`, method 14) reports `require_signed: true`:
  it reads the view, as the readers do, so `info` agrees with the installs. A default that did not
  verify is named in `list`'s `error`. The path writes no separate log line: the state, `info` and
  `list` are where it shows.
- **What works.** Boot of the current default: the boot side reads the boot mark, which the change
  wrote (or the start repair wrote from the record); an unverified default boots, an
  unverified trial is refused, as the rules above say. Install of any bundle: refused, `NOT_PERMITTED`,
  because the board holds no trust root (a bundle is never installed unchecked while the switch
  reads on). Setting an unverified slot as default or trial: refused. Reading `state`, `info`, `list`:
  works. `set(off)` (`lemnos-ctl security off`): works, see below.
- **The way back, without editing files by hand.**
  1. `lemnos-ctl security render --record FILE --dir /run/board` re-renders the view from the record,
     with no daemon. A valid record renders its switch; an unreadable record renders on, reported.
     No restart is needed afterwards for the readers (they read the view at each use).
  2. A restart of `lemnosd` renders the view from the record too (the driver's start).
  3. `lemnos-ctl security off` (the device's `set(off)`) writes the record as the next generation, then
     the view, then clears the boot mark (see "The order of a change"). It needs the daemon. It is refused only while a stakeholder reports work in
     progress (`busy:`). Turning the switch **on** is never done to fix a view: it is refused until the
     board has a trust root and its default and running slot verify under it.
- **The protection, stated plainly.** Turning on needs proof: a trust root, and the default and running
  slots verified under it. Turning off needs an authorization signed under the board's trust root when the board holds one
  ("Turning it off" above); without a trust root it needs only access to the daemon's socket, and the
  stakeholders' "not busy", so anyone who can call the daemon can turn the switch off, as they can install. A process that can write `/etc/board/security` directly (root on the image)
  can also set the view; the view is what the readers read until the next render, which the daemon does
  at its next start or change. 
- **A corrupt record** (both copies of the two-copy record unreadable) reads on, and the view is
  rendered on. `set(off)` writes a valid record as the next generation and repairs both.

### The order of a change, and the boot mark

A change to the switch (`set(on)` or `set(off)`) writes three things, each durable (file and directory
synced) before the change answers success:

1. **The boot mark**, which slot-init reads before the daemon starts. The stakeholders write it through
   their `apply(on)` (method 2 of `system.security-precondition/1`; `system-slots` writes the marker
   `board/slots/require-signed`, `system-flash` has none).
2. **The record**, the two-copy truth (`lemnos_slot_store::security`).
3. **The view**, which the readers read (`/etc/board/security`).

The order is what keeps the mark never less strict than the record:

- **To on:** the mark, then the record, then the view. A crash after the mark and before the record leaves
  the mark on and the record off (stricter). A crash after the record and before the view leaves the view
  stale (off) until the daemon starts, and the daemon renders the view from the record before it serves.
- **To off:** the record, then the view, then the mark. A crash after the record leaves the mark on (stricter)
  until the mark is cleared; the next start clears it from the record.

A **failed** mark stops a change to on before the record is written, so the switch does not change. For a
change to off, a mark that cannot be cleared is reported as an error, and the mark stays on until the next
start repairs it.

**The start-up repair.** When the security device starts, it makes each stakeholder's mark match the record
(`apply(record)`), then the view is rendered from the record. A repair that fails is shown as `last:`
`boot mark not repaired` in `state`. Whatever a crash left, the first start brings the mark and the view to
the record before the daemon serves a call.

**The crash points, tested** (`drivers/system-security/src/flow/tests.rs`): a simulated power cut before each
step of a change to on and of a change to off; after each, the mark is checked never weaker than the record,
and a restart's repair brings the mark and the view to the record. A change with no fault leaves the mark,
the record and the view agreeing.

### The Orion precheck (wired)

The `orion-precheck` device (`drivers/orion-precheck`, `docs/drivers.md`) serves `system.security-precondition/1`
for the switch, backed by Orion's `security_precheck` (wire 0xB0 and 0xB1, interface level 5). It is bound in the
`system-security` device as `uses = { orion = "<the device>" }`; the driver asks it through the same path as the
other stakeholders, so its lines are named `orion: ...` in the refusal.

- **Where.** A device in the host, with its own Orion session to the comm core named by its `address` setting (a
  `unix:<path>` or a socket path; unset, the board's own socket, as `ORION_ADDRESS` names it). It is not a bridge
  device: the bridge publishes to Orion and is a separate process.
- **Boards without Orion.** On a board whose `[security]` says `orion = false` the bound device is not asked, and
  the switch says so by its `note:` line as before.
- **What it calls.** `security_precheck(On)` for a change to on, `security_precheck(Off)` for a change to off. A
  change to on on a board with Orion and no `orion` device bound is refused by name:
  `missing: orion: the board has Orion, and no orion device is bound to ask it`.
- **Answer shapes.**
  - *Yes*: no line (the empty text). Orion says so when a way in exists (an enrolled operator, a recovery key, or
    an enrollment key).
  - *No, with a reason*: the `missing:` lines Orion gives, for example `missing: orion: nobody but root could get in
    once it is on: enroll an operator, add a key to <recovery keys file>, or set enrollment.key`.
  - *Below the level*: `missing: orion: the node speaks interface level N; the precheck needs level 5`.
  - *Did not answer*: a call that times out, is refused, loses its session, or finds no session says
    `missing: orion: it did not answer (<why>)`. A refusal of the change, never a change to the switch.
- **Connecting and deadlines.** The session is kept by the driver's own worker thread. Connecting is tried on a
  backoff of 1 s, doubling, capped at 8 s, and is never given up; the device's health is `Degraded` while it is not
  connected. A call has a 1.5 s reply deadline, and the driver waits 1.7 s for the worker's reply, under the host's
  2 s call deadline. A call is never retried. A lost session is dropped and reconnected at once.
- **The boot mark.** The device has none: `apply` does nothing, since Orion keeps no mark for the switch.
- **Tests.** `drivers/orion-precheck/tests/precheck.rs` (a fresh node, a node with an enrollment key, a node at level
  4, a silent node, no comm core) and `drivers/system-security/tests/orion_bound.rs` (the bound device's answer, the
  unbound refusal, and a board without Orion not asked).

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
size, then one unit for each of the finish's five steps: the checks, the slot's files, the payloads
into a bay, the boot directory into the store, and the commit (the image and manifest into place,
then the metadata). The total is the size plus those five, so the figure never goes back. Its result
is the new slot's id. One install runs at a time.

The finish is not in the call that takes the last byte. That call returns at once with the job
running, and the driver runs the finish from its ticks.

**Each tick is bounded by time.** A tick works in pieces of 64 KiB (`drivers/system-slots/src/budget.rs`)
and looks at the clock after each: it stops at the first piece past a quarter of the host's call
deadline (the board file's `call_deadline_ms`, 2000 ms by default, so 500 ms), or past 1 MiB, whichever
comes first. One piece always runs, so the install moves however slow the board is. The bound is time,
not bytes, so it is the same on every target: a slow card, a slow SHA-256 or an emulated disk only make
the install take more ticks. Reading a bundle from its file, hashing and copying the boot directory into
the store, and writing the payloads into a bay (on the tryboot and the U-Boot forms alike) all go by
pieces. A file's flush (fsync) never runs on the tick: it runs on the flush worker, and the tick only
asks whether it is done. The offline writer (`lemnos-ctl data-partition`) has no call deadline and runs
each step through. The host gives the deadline to every driver in its config (`call_deadline_ms()`).

**The slow-period look is off the tick too.** Every 600 s (and at the first tick) the driver reads and
hashes the default slot's boot files against its manifest (`boot_guard`). That read is a thread of its
own over a record of its own, and it writes nothing; a tick only takes its answer. Only when the default
no longer matches and the previous does does a tick run the check that moves the default (it reads both
again before it writes).

So a call from another session answers between any two ticks. A step that refuses ends the job with
the refusal, and the reason is in `info`. A cancel after the payloads are in a bay frees that bay,
since nothing points at it yet. The commit is one step, so a cancel falls before it or not at all;
once it has run, the install is in place. `docs/daemon.md` ("When a client gets `BUSY`") has the job's
contract.

Tests: `on_a_slow_store_each_tick_of_a_boot_file_copy_stays_inside_its_share_of_the_deadline` and
`on_a_slow_store_the_payload_write_into_a_bay_stays_inside_its_share_of_the_deadline` (a clock on which
each piece takes 100 ms, boot files that differ from every stored one; both fail with the time check taken
out), `the_slow_boot_file_look_runs_off_the_tick_and_its_answer_is_taken_by_a_later_tick`, and
`engine/host/tests/call_deadline.rs`. In QEMU, `testing/qemu/run-slot-install.sh` with
`THROTTLE_BPS=4000000 MAX_TICK_US=1000000`: the guest's disk writes held to 4 MB/s, the install's boot
files differing from the stored ones, the default deadline. The longest tick in the job was 581 ms and
621 ms (two runs); the daemon before this change took 1912 ms in one tick, and at 2 MB/s ran a tick past
the deadline, which ended the job interrupted.

**Not bounded yet: a call that writes.** At 2 MB/s, a stream `write` call can wait in the filesystem
for over 2 s (ext4 holds the write while a journal commit flushes ordered data), and the call deadline
then ends the job. No piece size bounds one blocking `write(2)`; it needs the install's file writes off
the call. Seen in 2 of 3 runs at 2 MB/s, none at 4 MB/s.

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

## The first slot, written offline

`lemnos-ctl data-partition --layout-of BOARD --install BUNDLE --boot-image FILE --keys FILE --time UTC
--data-size BYTES --out DIR` writes a data partition's tree for one slot, with no daemon running. It
runs the daemon's own install, trial and confirm over `DIR`, and keeps the boot image's record in a
copy that is never written back. `--verify DIR --layout-of BOARD` reads the tree back, read-only.

- **Room.** `--data-size` is the bytes the data partition has for the tree. The writer applies no
  reserve. The slot and its records (the sum of their file bytes) must fit in it, and a write that
  does not fit is refused, with both sizes named: `no room for this slot: the slot and its records
  need N bytes, --data-size gives M bytes: give the data partition more room`. A refused write
  leaves no directory.
  The sum counts file contents only; the filesystem's own block and metadata overhead is not in it,
  so give the partition some headroom.
- **Live installs keep their reserve.** A live install needs free space of the slot plus `reserve_mb`
  from the board file's system-slots device (2048 MiB when it is not given). That rule is unchanged.
- **The boot image's bay is compared by hash.** The slot's bay in the boot image must hold this
  bundle's build: each of its five boot files hashes the same as the bundle's payload for that role
  (`bays::compare_hashes`). A bay of another build of the same slot id is refused by name, and
  nothing is left. `--verify` with `--boot-image` checks the bay's stored hashes against the tree's
  `slot.json`.
- **`--verify` shows it.** With `--layout-of`, `--verify` prints the reserve the board will keep for
  live installs: `the board keeps N MiB free for live installs (its reserve_mb); the offline writer
  applies none`.
- **`--verify --boot-image` checks `SECURE.REC` (M36f).** A freshly built image must have both copies of its
  switch record good. The check is stricter than the device on purpose: the device reads one surviving copy,
  the image check refuses a copy that is damaged. It prints `ok the boot image's SECURE.REC has both copies
  good and reads off at generation N` (or `on`) and refuses by name:
  - **Missing:** `BAD the boot image has no SECURE.REC: the image builder must pre-create it, off, as a new
    image has it (`security set --boot-image` only rewrites an existing SECURE.REC and cannot create one);
    without it the switch reads as on`. The image builder pre-creates it; `security set` does not.
  - **A copy damaged:** one line per bad copy, naming it `first` or `second` and the check that failed, e.g.
    `BAD the boot image's SECURE.REC first copy is damaged: the CRC does not match the copy; the image check
    needs both copies good: rebuild the boot image`. The checks are the magic, the version, the reserved
    bytes, the payload length, the CRC, the record kind, and the value (`on` or `off`).
  - **Both damaged:** both lines appear and the verify refuses with `refused: 2`.
  - **Runtime, unchanged:** the device reads a record with one valid copy (`security render`, the daemon).
    Two valid copies that disagree at one sequence are refused as `record copies disagree at the same sequence`.
  Without `--boot-image` the verify does not look at the record.

## Actions

| Method | What it does |
| --- | --- |
| `list` | Every slot (with its `product`, `version` and `board`), the pointers, free space, the reserve, the number kept, the last error |
| `list_page(after, size)` | The same object with one page of the slots: as many whole slots as fit the buffer, newest first, then `more` and `next`. `after` is `-` for the first page and the previous page's `next` after it (see below) |
| `info` | The signature policy (`require_signed`), the last install's signature result and how the last install attempt ended (`last_attempt`) |
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
| `reboot_to(target)` (`system.reboot/1`, added) | Restarts the board into its way back, when the board declares one (see [The way back on request](#the-way-back-on-request)) |

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
  two sectors (1024 bytes) at a fixed place on the boot partition, pre-created at image build. Each
  sector's first line is `os_prefix=slots/bayN/` (the bay set the pointer names, see
  [The boot partition: payload bays](#the-boot-partition-payload-bays)), then a `#lemnos-crc32=` checksum
  line over that first line, then `#` padding. The checksum is what refuses a torn sector (see
  "Pointer files", below).
- A restart with the argument `0 tryboot` reads `tryboot.txt` once. A trial's attempts are in the
  trial record `trial.rec` (FAT name `TRIAL   REC`, `lemnos_slot_store::boot_raw`), `0` to `3`. The firmware's own boot
  count is used only by the count-based fallback, below.
- The initramfs knows a trial boot from `/proc/device-tree/chosen/bootloader/tryboot`.
- The default slot's `lemnos-slot-init` takes the attempt. When the trial record names a slot, the
  firmware didn't boot the trial, and its attempts are above 0, it writes one fewer and restarts
  with `0 tryboot`, which boots the trial. With 0, it clears the record and boots the default. See
  "The extra pass on tryboot", below.

### `config.txt` is written once, offline

On every Raspberry Pi board, `config.txt` is the firmware's first file, and it is written in one place
only: `lemnos-ctl boot-partition`, when the boot partition is made. It holds the fixed text
(`lemnos_slot_store::firmware::CONFIG_TXT`), the board's `gpio=` lines and its `config_lines`. Nothing at
run time writes it: not the driver, not `lemnosd`, not `slot-init`, not the install.

The fixed text selects the slot through the pointer files it includes:

- `include default.txt`, then, on the Raspberry Pi 5 family only, `[boot_count>3]` and
  `include previous.txt`: the default, or the previous when the count is above 3 (`firmware::FALLBACK_AFTER`,
  the design's trial attempts). That is the count-based fallback, and it is written in `config.txt` itself,
  so it needs no run-time edit. The firmware rule is the expression filter in the Raspberry Pi documentation,
  `documentation/asciidoc/computers/config_txt/conditional.adoc` at `raspberrypi/documentation` commit
  `b01a5c1a`: `[ARG>VALUE]` is selected when ARG > VALUE, and `boot_count` is "Raspberry Pi 5 and newer
  devices only". The threshold form is used because the mask form `[ARG&MASK]` is selected when
  `ARG & MASK != 0`: `[boot_count&4]` would be true at counts 4 to 7 and false at 8 to 11, so a board that kept
  failing would bounce between previous and default. A test over all 256 counts checks the threshold
  (`the_firmware_picks_default_up_to_the_threshold_and_previous_above_it_for_every_count`), and another shows
  the mask form disagrees (`the_mask_form_would_bounce_between_the_pointers_and_the_threshold_form_does_not`).
  On a Pi that is not in the Pi 5 family the text has no filter and no `include previous.txt`
  (`CONFIG_TXT_NO_COUNT`): the board file's `count_fallback = true` is refused at build by name, and
  `count_fallback = false` on a Pi 5 writes the same text as the older boards.
- **The count's life.** The firmware's `boot_count` is an 8-bit value, incremented at each boot and wrapping at
  256. It is **cleared by power loss**: a board that fails, is power-cycled and fails again starts the next round
  from 0, so the fallback counts from the last power-on, not from the first failure. It is cleared by a good
  boot: a confirmed trial (`confirm`) and a default boot that has run its uptime with its boot files matching
  the manifest (`boot_good`, `count::Fallback::clear_count`, through the firmware mailbox tag `0x0003808d`). A
  good boot whose boot files do not match keeps its count, and the state says so. The count is read as the
  device-tree property `/proc/device-tree/chosen/bootloader/count`, a raw big-endian `u32`, not text
  (`count::Firmware::word`; `the_count_bytes_are_big_endian_and_the_little_endian_order_is_refused`).
- The pointer files (`default.txt`, `previous.txt`, `trial.txt`) hold the run-time state: two sectors each,
  one `os_prefix` line per sector, the form in "Pointer files" below. A default switch, a confirm, a
  demotion and a trial arm are writes to these files, and to the trial record. `tryboot.txt` includes
  `trial.txt` and is written offline too.

A board's own `config.txt` is not written by Lemnos. The one line every Pi needs is `include default.txt`, and the
count's section is required only where the image was built with it. If the line is missing, the driver
still starts, and it is **degraded**: its health names the reason
(`config.txt has no \`include default.txt\` line: ...`), `list` carries it as `config_refusal`, and reads still
work. The writes that need the file are refused by name, and the file is left as it is: `install`, `set_default` and `trial` on the Tryboot record,
and the first-slot writer (`lemnos-ctl data-partition --boot-image`) all read the file with
`firmware::check_config_text` first. The refusal names the missing line and says that Lemnos never edits
`config.txt`.

**A board with the old count filter.** Before the threshold, the Pi 5 text carried `[boot_count&4]`. Its mask
form is selected at counts 4 to 7 and 12 to 15 and not at 8 to 11, so the firmware bounces between the pointers,
and `pick` models only the threshold. Lemnos never rewrites `config.txt`, so it does not run a firmware view it
disagrees with: a `config.txt` carrying `[boot_count&` is refused (`firmware::OLD_FILTER_REFUSAL`). The driver starts
**degraded**, its health and `list` name `[boot_count&4]`, and `install`, `set_default` and `trial` are refused by
name, as for a missing include line. Test: `a_config_txt_with_the_old_mask_filter_is_degraded_and_refuses_a_trial`
and `a_config_txt_with_the_old_mask_filter_is_refused_by_name_and_the_threshold_text_is_not`.

Such a board is brought back by rebuilding its boot partition offline: `lemnos-ctl boot-partition` builds the image
with the threshold text (the Pi 5 family's `CONFIG_TXT`), and that image is written to the board's boot partition
while the board is not running a slot from it. Rebuilding the partition rebuilds its bays, so each slot the board
keeps is installed again from its bundle (`install` or `lemnos-ctl data-partition --boot-image`). The data partition
and the slots' images are not touched by the rebuild. Until then the board runs its own boot as it did, and no
trial or default switch is made on it.

**What a power cut leaves (the firmware's view).** The firmware applies the `os_prefix` lines of a pointer
file in turn, so the last whole line is the slot it boots. Lemnos writes sector 1 first, then sector 0,
and reads sector 1 first. With a single-sector write that completes or does not start (the storage contract
this design already relies on), the firmware's view and Lemnos's view are, at every cut between sector
writes, both the old slot or both the new one: `at_every_sector_cut_the_firmware_and_lemnos_agree_and_see_old_or_new`.
A completed switch lands as the new slot. The check is against a naive single-sector update, which reports
success and leaves the firmware on the old line: `a_naive_single_sector_switch_breaks_the_agreement_at_a_cut`.
A tear inside the one sector the firmware reads last is not covered by the layout alone; it is covered only
by the single-sector contract above. The byte-level cut test (`a_pointer_cut_at_every_byte_is_the_old_slot_the_new_slot_or_refused`)
checks Lemnos's reader, not the firmware.

Tests: `the_install_the_default_and_the_trial_never_write_config_txt` (a stand-in partition that fails every
write into `config.txt`), `a_config_txt_without_the_select_lines_refuses_slot_setup_and_is_never_edited`,
`a_boot_image_whose_config_txt_has_no_select_lines_is_refused_before_anything_is_written`,
`a_config_txt_without_the_default_line_is_refused_by_name_and_the_image_texts_pass`,
`a_config_txt_without_the_select_lines_leaves_the_driver_running_degraded_and_listed` (system-slots: the driver
starts, health and `list` name the reason, and a write is refused), the Pi 5 gate in
`boot_partition::count_fallback_tests`, and `the_fixed_text_and_the_slots_reading_of_it_are_one_constant`.

### The `uboot` record

U-Boot's standard boot flow starts the slot. The record is U-Boot's own redundant environment, written
raw: `default`, `trial` and `trial_attempts` are variables of it, and so are `default_bay`, `trial_bay`
and `previous_bay`, the payload bays those slots are in (see
[The boot partition: payload bays](#the-boot-partition-payload-bays)). Where it is, and the Kconfig
fragment the board's U-Boot is built with, are in [Boards](boards.md#u-boot-environment-system-slots-record--uboot).
The format is U-Boot's, from its sources at tag `v2026.10` (commit 5508406): `include/env_internal.h`,
`env/common.c`, `env/mmc.c`. Each copy is a CRC-32 over the whole data area, a one-byte serial and the
data (`env/common.c:582-600`, `env_export`); the serial is the active copy's plus one, wrapping from
255 to 0; U-Boot takes the copy whose CRC is right, and of two, the higher serial (255 against 0: the 0
is newer), and on a tie the primary copy, the one at `CONFIG_ENV_OFFSET` (in the GPT form, copy 0 at the top of the partition), as `env/common.c:569-570` takes `tmp_env1` (`env_check_redund`, `env/common.c:484-551`).

- **Where the copies are.** The environment is the one partition on the boot disk whose GPT type GUID
  is `3de21764-95bd-54bd-a5c3-4abe786f38a8` (`include/part_efi.h:61`). With `CONFIG_PARTITION_TYPE_GUID`
  U-Boot puts copy 0 in the last `CONFIG_ENV_SIZE` bytes of that partition (rounded up to 512-byte
  blocks) and copy 1 in the block run below it (`env/mmc.c:114-148`); a partition too small for two
  copies is U-Boot's own refusal (`env/mmc.c:135-141`) and Lemnos's. Lemnos reads the primary GPT
  (header at LBA 1, entry array, both CRC-checked) to find the partition: none, or more than one, is
  refused by name. For an environment outside the GPT the board file names the device and the
  offsets (`uboot_env_device`); flash and UBI volumes are refused by name.
- **The first write is checked.** Before anything is written both stored copies are read and
  CRC-checked at the board file's size. With neither valid nothing is written, by name:
  `no valid U-Boot environment of N bytes here: U-Boot may be built with another CONFIG_ENV_SIZE`;
  an erased area (every byte 0xFF, or every byte 0, in both copies) is its own case: `the U-Boot
  environment area is erased: U-Boot has not saved an environment here yet (boot.cmd saves the first
  one at the first start), so none is written`. The rule for the erased case: Lemnos never writes the
  first environment, because an import replaces U-Boot's built-in defaults with what the environment
  holds (`env/common.c:443-466`, `himport_r` without `H_NOCLEAR`), so an environment of only our three
  variables would leave U-Boot without its own. `boot.cmd` saves the first one itself (below). With
  one copy valid (the other torn by an earlier cut) the valid one is the active one and the other is
  the copy written.
- **Cut safety.** Lemnos writes only the copy that is not active, in one write of the copy's sectors,
  with the serial of the active copy plus one, then reads both copies again: the new copy must be
  valid and the one U-Boot's rule selects. A cut at any byte of the write leaves the active copy
  untouched and valid, so U-Boot (and the next Lemnos read) takes the old environment or the new one,
  never neither; the default it names was bootable before the write and is unchanged by it. The tests
  cut at every byte of the write, from every serial value (the wrap included), with sectors landing
  in any combination and torn over garbage (`drivers/slot-store/src/uboot_env_tests.rs`), and in
  slot-init's fallback (`service/slot-init/src/boot/tests.rs`). `saveenv` in `boot.cmd` is U-Boot's
  own write of the same kind (`env/mmc.c:329-376`: the copy that is not active, then the active one
  flips).
- **What `boot.cmd` does with it.** It reads the variables U-Boot loaded, takes one attempt for a
  trial with attempts left, calls `saveenv`, and only then starts the trial. A save that fails skips
  the trial. At 0 or none, it starts the default; the root `extlinux/extlinux.conf` is the last resort,
  the install-time copy of the default's. The first start saves the environment once: `lemnos_env` is
  not set while U-Boot runs on its built-in defaults, so the script sets it and runs `saveenv`.
- The serial byte is not covered by the CRC. Its order makes that harmless: the new serial is the
  active one plus one, so a torn copy whose serial still reads old loses to the active copy.
- **Who writes it at run time.** The daemon writes the environment through the file its root phase handed
  over (`uboot::root_phase`, `lemnos_slot_store::uboot_env::hand_over`), never by opening the disk: the disk is
  `root:disk` and the daemon's user is not in that group. The root phase finds the partition by its type
  GUID, numbers it from the GPT (the entry's place in the table, which is the kernel's partition number), and
  hands over that partition's node. Its offsets are the partition's own, and the kernel confines the write to
  it. Read only, the daemon refuses a write by name with the reason (`refusing to write the U-Boot environment
  on the GPT partition: the boot partition holds uboot.env`, when the FAT file is there). A device-form
  environment is handed over as its device. slot-init writes the same copies in its own unit, as root.
- slot-init reaches the environment through `uboot-env.txt`, the locator at the boot partition's root,
  which `lemnos-ctl boot-partition` writes at image build from the board file (`form = "gpt"` with the
  size, or `form = "device"` with the device, size and offsets; unknown keys, a wrong version or
  sector and keys of the other form are refused by name). It is read raw (no mount). The fallback's
  trial, the withdrawal of a held trial, and the repoint of `default` when the default's boot files
  do not match are each one write of the inactive copy. The locator is not in any slot's signed
  manifest: a bundle is per slot, not per board, so its check is the locator's own fit against the
  device, not the every-boot hash (an open gap: the every-boot check cannot see this file; see
  `testing/board-runs/I5i7.md`).
- The kernel-gap reason goes to the pre-created `reason.rec` (raw), as on the other forms
  (`boot-partition` pre-creates it on the image for this record).
- **Stated limits.** MMC only (`CONFIG_ENV_IS_IN_MMC`; the copies in one user area, not the two eMMC
  boot partitions, `env/mmc.c:216-231`). Little-endian CRC word. A primary GPT that is damaged is
  refused rather than read from the backup. 512-byte blocks. Not run on a board:
  `testing/board-runs/I5i7.md`.

### Trial updates on the `uboot` record

A trial needs the environment's writes to be read by U-Boot, and the boot script to save them: the
script saves only where the environment's device has a partition named `uboot-env` (`part start mmc
<dev> uboot-env`, `packaging/uboot/boot.cmd`), and it starts a trial only after that save. So the
driver finds, at bind, after each look at the board (every ten seconds, and before each trial request),
whether a trial can be recorded and started. It reads only; nothing is written by the check. The
answer is typed, with a fixed set of reasons, checked in this order (the first failing check is the
reason):

| Reason (`info` code) | Found when | Sentence (a refused trial and `health` say it) |
|---|---|---|
| `no-env-partition` | the environment's device (the boot disk for the GPT form, the device for the device form) has no partition named `uboot-env`, or no readable GPT | the environment's device has no uboot-env partition, so the boot script never saves the environment and never starts a trial |
| `env-unreadable` | the device can't be opened, or the environment has no valid copy, or (GPT form) the boot disk isn't known | the environment can't be read |
| `fat-env-file` | GPT form: the boot partition holds `uboot.env`, which U-Boot reads before the GPT partition, so a GPT write would not be read | the boot partition holds uboot.env |
| `boot-partition-unreadable` | GPT form: the boot partition is not known, or can't be read as FAT, so `uboot.env` can't be ruled out | the boot partition can't be read as FAT |
| `loader-not-updated` | the environment does not hold `lemnos_env_rev=1`: the image's U-Boot does not carry the slot environment change (the marker below) | the image's U-Boot does not carry the slot environment change |

When trials are available, `info` carries `"trial_updates":"available"` and an empty `trial_block`.
When not, `info` carries `"trial_updates":"unavailable"`, `trial_block` (the code above),
`trial_block_text` (the sentence) and `trial_block_detail` (what was found). `health` is `Degraded`
with the sentence, since restarts still work. A `trial` request on such a board is refused before
anything is written: its status is `NOT_PERMITTED`, and the state's `error` is the sentence. A board
with the partition and the marker reports `available`.

**The U-Boot marker.** `lemnos_env_rev` is the variable the image's U-Boot carries in its default
environment, with the value `1` (`uboot::ENV_MARKER`, `uboot::ENV_MARKER_VALUE`). It is defined by the
patched U-Boot, not by this repository: the image's U-Boot fragment sets it in the default environment
(`CONFIG_EXTRA_ENV_TEXT`, `lemnos_env_rev=1`), and `packaging/uboot/boot.cmd` names it in its header.
It reaches the stored environment when U-Boot first saves a valid environment: an absent or invalid
environment is rebuilt from the defaults, and `saveenv` writes them with the rest. An environment
written by an earlier U-Boot never gains it, so such a board reports `loader-not-updated` until its
environment is rebuilt by a U-Boot that carries the marker. The image builder checks the marker
before it builds an RK slot image (`docs/boards.md`, "What an image builder checks").

### What the driver writes into the environment

The environment is one copy of `uboot_env_size` bytes (header included) in U-Boot's form: the 5-byte
header (CRC-32 word and serial, `encode_copy`), then `name=value` strings each ended by a NUL, then one
more NUL that ends the list (`lemnos_slot_store::uboot_env::encode_vars`). The variables the budget
counts are the ones the driver writes, the ones the image's preamble and the restart count store, and the
marker. Each is stored by `saveenv`, so each takes its bytes whether or not the script still needs it:

| Variable | Set by | Worst-case value | Entry bytes (name + `=` + value + NUL) |
|---|---|---|---|
| `default` | the default switch | a slot id, 64 bytes (`valid_id`, `store.rs`) | 7 + 1 + 64 + 1 = 73 |
| `default_bay` | the default switch | `bay2` (`SETS` = 3, `bays.rs`) | 11 + 1 + 4 + 1 = 17 |
| `previous_bay` | the default switch | `bay2` | 12 + 1 + 4 + 1 = 18 |
| `trial` | the trial | a slot id, 64 bytes | 5 + 1 + 64 + 1 = 71 |
| `trial_bay` | the trial | `bay2` | 9 + 1 + 4 + 1 = 15 |
| `trial_attempts` | the trial, the attempt bookkeeping | `3` (`MAX_ATTEMPTS`, `records.rs`) | 14 + 1 + 1 + 1 = 17 |
| `lemnos_env_rev` | U-Boot's default environment (the marker) | `1` | 14 + 1 + 1 + 1 = 17 |
| `lemnos_envdev` | the image's boot preamble (`service/ctl` `boot_dir`) | two digits (`uboot_env_mmc_index` is at most 99) | 13 + 1 + 2 + 1 = 17 |
| `lemnos_restarts` | the restart count: the script raises it, the driver clears it (BB2) | `8` (the script saturates it at 8, `packaging/uboot/boot.cmd`) | 15 + 1 + 1 + 1 = 18 |
| `lemnos_episodes_bay` | the episodes: the driver (BB2) | `bay2` | 19 + 1 + 4 + 1 = 25 |
| `lemnos_episodes` | the episodes: the driver (BB2) | `255` (`demote_after_episodes` is at most 255) | 15 + 1 + 3 + 1 = 20 |
| `lemnos_usb` | the preamble (`uboot_fastboot_usb`) | two digits (at most 99) | 10 + 1 + 2 + 1 = 14 |
| `lemnos_led` | the preamble (`uboot_led`) | 32 bytes (the board file's limit) | 10 + 1 + 32 + 1 = 44 |
| `lemnos_rst` | the preamble (`uboot_reset_word`) | `0x` and eight hex digits | 10 + 1 + 10 + 1 = 22 |

The sum of the entries is 388 bytes; the list's final NUL makes 389; the header makes **394 bytes**
(`uboot::ENV_WORST_CASE_BYTES`). The budget the variables must stay within is 400 bytes
(`uboot::ENV_BUDGET_BYTES`). Two tests hold the number: one computes the worst case from
`uboot::worst_case_entries` and fails if it differs from the stated 394 or exceeds the budget; another
encodes the worst case and checks that the stored length is 394. A variable the driver starts to write
must be added to `VARIABLES` (or `COUNT_VARIABLES`) and given a worst-case value in `worst_value`; a name
without one is counted at the longest slot id, so it is over-counted rather than missed. The board file's
caps that the table depends on are checked at validation: `uboot_env_mmc_index` and `uboot_fastboot_usb` at
most 99, `uboot_led` at most 32 bytes, `uboot_reset_word` at most 10 bytes.

**The board file.** `uboot_env_size` below 394 is refused at validation, by name: the sentence says the
variables need 394 bytes, header included, in the worst case. A 0x2000 (8192-byte) copy holds them with
7798 bytes to spare. Whether U-Boot's own default environment fits in the rest is not known to Lemnos:
the default environment is compiled into the image's U-Boot, and nothing here reads it.

**A write that does not fit.** The encoder refuses it before any byte is written, with the bytes it
needs and the copy's size (`the environment is full: N bytes needed for its variables, M in the copy`).
The driver records that text as the state's error, so `info` shows it. The write is refused with `IO`.

**Not in the budget.** `saveenv` writes the whole environment in U-Boot, not only the variables above.
The boot script also sets interim variables before its saves (`lemnos_saveable`, `lemnos_envpart`,
`lemnos_env`, `lemnos_dtb`, `lemnos_conf`, `lemnos_src`, `lemnos_why`, `lemnos_pass`, `take_trial`, and
others: `grep -o 'setenv [a-z_]*' packaging/uboot/boot.cmd`), and those are stored too. Their worst case
is not derived here. The sentences in `lemnos_why` are the longest of them. A board whose default
environment and these interim variables together exceed `uboot_env_size` gets a write refused as above.

### Loader-dependent operations

An operation whose effect is the boot loader reading the record is refused before any write while trial
updates are unavailable, with the reason's sentence as the state's error (`boot::loader_reads_record`).
The decisions, by caller in `system-slots` (and `service/slot-init`, which is outside it):

| Caller | Operation | Decision while unavailable |
|---|---|---|
| `Slots::set_default` (`lib.rs`) | the switch: writes `default`, `default_bay`, `previous_bay` | refused, `NOT_PERMITTED`, nothing written |
| `Slots::trial` (`lib.rs`) | the trial: writes `trial`, `trial_bay`, `trial_attempts` | refused, `NOT_PERMITTED`, nothing written (checked before the others) |
| `Slots::install` completion (`lib.rs`) | the bays and the slot's files; no environment write | written; the install's reply says the boot loader will not start it |
| `resolve`: confirm's switch to the running slot (`confirm_as`) | `default`, then `trial` cleared | refused before anything is written; the trial stays pending |
| `resolve`: confirm's trial-only branch | clears `trial` | allowed: removing a trial starts nothing |
| `resolve`: give-back of an attempt (`resolve_at_start`) | `trial_attempts` to the count given back | refused; the attempt is not counted in the record |
| `resolve`: `writes_work` | `trial_attempts` rewritten to the count left | refused when a count is left; a count of 0 is written |
| `resolve`: fallback, back-on, and the give-up | clears `trial`, or sets `trial_attempts` to 0 | allowed: they stop a trial |
| `boot_guard::repoint` | the default moves to the previous slot | refused before the old default is marked failed; the restart is refused with the reason |
| `count::point_default` | the default moves (the `tryboot` count's fallback) | refused (guarded for uniformity; the `uboot` form has no count, so it is not reached there) |
| `arrive_uboot`: the arrival's episode (`record_episode`, `set_episodes`) | `lemnos_episodes_bay`, `lemnos_episodes` | allowed: the driver's own count, which the loader never reads; it is what demotion is decided from |
| `arrive_uboot`: the arrival's demotion (`point_default`, `set_default`) | the default moves to the previous slot | refused, as `repoint` is: the demotion is not made, the arrival's note says why |
| `arrive_uboot` and `boot_good_uboot`: clearing the episodes | `lemnos_episodes_bay`, `lemnos_episodes` removed | allowed: removing bookkeeping starts nothing |
| the restart count's reset (`clear_count` on a good boot, and `guard_check` before an intended restart) | `lemnos_restarts` set to 0 | allowed: the driver writes only 0, which stops a count the loader would act on; a count that is already 0 writes nothing |
| `UbootEnv` baseline | none on this form | refused by name (the trait's defaults), no write |
| `OfflineRecord` (`offline.rs`) | pointer records of the `tryboot` form | no environment write: the U-Boot form refuses pointer operations by name (`boot_raw`) |
| `service/slot-init` (boot-time attempt accounting) | `trial_attempts`, `trial` and the environment at boot | not refused: it runs after the loader has started a slot, and refusing would strand the count |
| `packaging/uboot/boot.cmd` | saves the environment at its first start and when it takes a trial | the script's own; it runs on the device, not in the driver |

There is no rescue operation in `system-slots`; the nearest is the give-up path above.

### The `file` record

The `file` form (one `boot-record` file on the boot partition, for a loader with one fixed kernel)
has no raw path in the design, so no run-time code writes it. Its trial and default are refused by
name: it is not a production slot form.

### Boot partition writes audit

`drivers/system-slots/src/boot_writes_audit.rs` scans the run-time sources (the slot store, the slot
driver, slot-init, lemnosd) for every non-test line that writes under a boot-partition path. A line
that is not in its list fails the test and names itself; a listed line that no longer exists fails
it too. The verdicts of the listed lines:

| Lines | Verdict |
|---|---|
| (none) | **Install.** The install-time file writes of the first design (the renames of a slot's files into `slots/<id>`, the root copy of the default) are gone: an install is raw sector writes inside pre-created bay files (see [The boot partition: payload bays](#the-boot-partition-payload-bays)), which the scan does not take for a file write |
| `uboot.rs`: `sync_dir(&self.boot)`, `replace(extlinux.conf)` | **UBI form only.** The raw-NAND form keeps a root copy of the default's `extlinux.conf` current on its UBIFS boot volume; the boot-partition form (`UbootEnv`) writes nothing here |

The run-time record writes (trial, default, reason) are not on this list: they are raw sector or raw
environment writes, which the scan does not take for a file write.

### Under the unit: the writes that are still root's (design, not built)

The daemon runs as its own user under `ProtectSystem=strict` (`packaging/README.md`). The boot
partition is handed over by the root phase, and the state dirs are the daemon's by `slots.conf` and
slot-init. Three writes need more than that, and none of them can be made under the unit today:

| Write | What it needs | Why a grant is not enough | Plan |
|---|---|---|---|
| The `uboot` record's environment (`Place` on a raw partition or an MTD node) | read and write of one node, root `0600` | nothing else: the record is plain reads and writes at offsets | The root phase hands it over as it hands over the boot partition. It opens the node read-only, probes it (both copies' CRC, the board file's geometry, the driver's own keys), and opens it read-write only when the environment is ours. No udev rule: a rule on a raw partition gives the whole node to the group for good. |
| Growing the data partition (`grow`: the whole disk's table, then `BLKPG_RESIZE_PARTITION`) | the whole-disk node, and `CAP_SYS_ADMIN` when the ioctl is made | the ioctl checks the capability when it is made, not when the node is opened, so a handed-over descriptor does not carry it | Moves to slot-init, which is root and runs before the daemon. Grow happens once, at the first start, and its record (`board/slots/grow`) already says where it got to. The driver reads the record and reports it, and writes nothing to the disk. |
| UBI slots (`make`, `rename` and `remove` on `/dev/ubiN`, `begin_update` on a volume, `ubiblock`) | `CAP_SYS_RESOURCE` for every one of those ioctls | the same: the kernel checks the capability at each ioctl | A small root helper forked by the root phase, built like the gadget helper. It keeps uid 0 and `CAP_SYS_RESOURCE` alone, under its own seccomp allowlist. It acts only on the UBI device the board file names, and only on volumes with the slots' prefix. For an update it opens the volume, starts the update, and passes the write descriptor back over its socket pair, so the image bytes never go through the helper. |

Until these are built, a board with a `uboot` record, `grow` or UBI slots can't make those writes
under the unit: the kernel refuses the open (`EACCES`) or the ioctl (`EPERM`).

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

### The boot partition: payload bays

A slot's boot files are not written under the slot's name. The boot partition holds **three bay sets**,
`slots/bay0/` to `slots/bay2/` (the layout contract, `contracts/slot-layout.toml` version 6). Three sets
are the default, the previous, and the one being installed or on trial; no other slot has boot files on
the partition, so the partition does not grow with the keep count. A set is the directory a pointer
names (`os_prefix=slots/bayN/`), so every file in it is under the name the firmware reads by default,
**at its exact size**: the firmware sizes a kernel from the end of its file and a device tree from its
header, and a padded file fails both (see "Firmware layout" below).

**What a set holds.** On the Pi firmware form: `kernel8.img`, `initramfs.cpio`, the device tree under
each of the board's default names (`[boot] device_tree_names`, for example `bcm2712-rpi-5-b.dtb`; each
name a copy of the bundle's `board.dtb`), `cmdline.txt`, `overlays/lemnos-board.dtbo`, each firmware
overlay the board declares (`overlays/<name>.dtbo`), each firmware file the board declares
(`[boot] firmware_files`, under `overlays/`: on the Raspberry Pi 5 `overlay_map.dtb` and `hat_map.dtb`,
which the firmware reads from the prefix, built from the pinned kernel tree by
`scripts/firmware-maps.sh`), the generated `overlays/README` (it makes `os_prefix` apply to overlays),
`bayinfo.rec` and `reserve`. On the U-Boot form: the five roles under the names above, with the device
tree as `board.dtb`, the generated `extlinux/extlinux.conf`, `bayinfo.rec` and `reserve`. No `kernel=`,
`cmdline=` or `device_tree=` line names a file: the firmware's default names are the only names.

**The region.** Each set owns, for the life of the image, one contiguous run of clusters that starts on
a multiple of 256 and is a multiple of 256 long: on FAT16 a FAT sector holds 256 entries, so the FAT
sectors that describe a region hold nothing else. The region is big enough for every file of the set at
the largest size the board allows it at once: `[boot] bay_kernel_bytes`, `bay_initramfs_bytes`,
`bay_device_tree_bytes` (for each default name), `bay_cmdline_bytes` and `bay_overlay_bytes` (for the
board overlay and each firmware overlay and file), each rounded up to whole clusters, the sum rounded up
to 256 clusters ([Boards](boards.md), no default). Each is the size of the **stored** file: the bytes as
the slot bundle carries them, written as they are. Nothing is decompressed or recompressed, so on the Pi
form `kernel8.img` is the gzip kernel the firmware decompresses and `bay_kernel_bytes` is that compressed
size; on the U-Boot form the kernel is stored raw (a compressed one is refused by name: `booti` needs
per-slot decompression addresses). The rule for choosing a size: take the largest file of the role over
every slot you expect to install, add margin (the board file's comment says 10 %), and round up. A
region cannot grow after the image is built. A bundle whose file is bigger than its role's size is
refused by name at install, before anything is written and with the default still booting (`<slot>: the
kernel payload is <n> bytes but its bay holds <m>: nothing was written, the default keeps booting;
rebuild the image with a larger kernel bay or a smaller payload`), and `lemnos-ctl boot-partition
--payload DIR` names the role at build. Whatever the files leave of the region is the file `reserve`, so
no cluster of a region is ever free and nothing else on the volume can take it.

**The record.** `bayinfo.rec` (two copies, like every record) keeps the set's region, the slot it holds
(none for an empty set), each file's length and the first eight bytes of its SHA-256, at most 30 files,
and the SHA-256 of the whole list (each path, its length and its full hash). A set whose record names no
slot holds no slot, whatever its files are. The payload begins with the byte `L` and the layout
version (6). A record of layout version 5 or before begins with its slot name's length (0 to 64), so it
is never read as a record of this layout. A partition whose set records are not this layout's is not
Lemnos's to write: the daemon's root phase hands its node over read only, the writer does not open it,
and the installer, a free and `--add-to` each refuse it by the record's path before their first write.

**The directory form.** In each set directory, and in each of its subdirectories, the entries an install
never changes come first (`.`, `..`, `bayinfo.rec`, the subdirectories), padded with deleted entries to
the end of their sector; the files and `reserve` start on the next sector. The image builder lays out
the empty sets in this form, and an install writes only the directory sectors whose bytes change, so the
sector holding the record's entry is never written after the image is built.

**Install.** The caller says which sets must not be touched (`bays::Protected`: the default's, the
previous's, the trial's, and a pending trial with attempts left); the Pi form takes them from the
pointers and the trial record, the U-Boot form from the environment (`default_bay`, `previous_bay`,
`trial_bay`, `trial`, `trial_attempts`). Each step is durable before the next:

1. refuse, before any write: a pending trial; the previous's or the trial's set; a payload bigger than
   its role's size; files that do not fit the region; and a default set that holds the slot with a boot
   file whose stored hash differs from the payload's (the hashes are compared by
   `bays::compare_hashes`, which a data-partition writer can reuse; the payloads are the bundle's boot
   files, which the bundle check has matched to `slot.json`);
2. (a) write the set's record empty (it keeps the region), so the set holds no slot;
3. (b) write the files' data into the region;
4. (c) write the region's FAT sectors, the second FAT copy, then the first;
5. (d) write the set's directory sectors that change, the `reserve` entry last;
6. (e) read every file back through the FAT, parsed again, and compare its hash;
7. (f) commit the record (the slot, each file's length and hash) and read it back.

Nothing outside the set's region, its FAT sectors and its directories is written. Switching the default
or the trial is a separate step that moves a pointer (Pi) or environment variables (U-Boot) and no
payload. A removal clears a set's record and nothing else.

**A power cut at every step.** A cut at any write of an install leaves: the default booting (nothing it
reads is written); every sector outside the set byte-identical; the cut set reading as uncommitted
(its record names no slot); and the next install into that set repairing it whole. The slot-store tests
cut the install at every write, and again at sampled bytes of the real-size image, and check those four
facts at each cut.

**The image builder** (`lemnos-ctl boot-partition`) writes the whole volume: FAT16 with two FAT copies
and the smallest power-of-two cluster of at least 512 bytes that gives fewer than 65,525 clusters, the
firmware text, the pointers, the records, the three regions, and the first slot in `bay0`. On the Pi form
it runs the firmware-layout check (below) on the image it made and writes no file when a rule fails.
`--min-size` prints the smallest `--size` for the board's bays, one line of digits (with `--payload DIR`,
that line first, then one line per role, `role stored_bytes bay_bytes`, and a non-zero exit naming a
role whose payload is bigger than its bay; the formats of every mode are in [Building](building.md)).
`--verify FILE` reads an image or a dump of a real partition and prints one `ok`/`BAD` line per finding:
each set's region, each committed set's files against its record, the pointers, the records and the
generated root files. An image written by `--out` is a standalone FAT image of the boot partition: the
whole partition is replaced when it is written, which is **option R** of the boot-partition plan. Adding
to a partition that another system boots from is `--add-to`, below.

**The U-Boot form.** The environment is U-Boot's own, written raw as before; the variables are `default`,
`default_bay`, `previous_bay`, `trial`, `trial_bay` and `trial_attempts`, and `lemnos_env`, which the first
start sets (the loader's own marker, not part of the record). Those six are the whole record: the script
reads exactly these six and the writers write exactly these six (`uboot::VARIABLES`); there is no variable
`previous`. The default switch writes `previous_bay`, `default` and `default_bay` in one write of the
inactive environment copy, so a cut leaves the old three or the new three. Nothing that was true of the
environment changed: both copies are checked at their size before the first write, an environment U-Boot
has not saved is refused, and `packaging/uboot/boot.cmd` reads `slots/${trial_bay}/`,
`slots/${default_bay}/` and `slots/${previous_bay}/` (it has no copy of a kernel at the partition's root; the
third is the step in "Previous before flashing mode" below). The bay's
`extlinux/extlinux.conf` is composed from the slot's command line and names the bay's own files by
relative path (`../kernel8.img`, `../initramfs.cpio`), with no `fdt` line: the script loads
`board.dtb` and names the boot source in it. The install writes it with the set's other files, before
the set's record. A compressed kernel is refused by name on this form (`booti` needs `kernel_comp_addr_r` and
`kernel_comp_size` set per slot); the Pi firmware form takes a gzip kernel.

**Records on the volume.** The Pi form has `trial.rec`, `reason.rec` and `episodes.rec`; the U-Boot form
has `reason.rec` only (its trial and default are in the environment, and
`lemnos_slot_store::boot_raw::open_records` opens whichever the image has).

**The every-boot check** (lemnosd's `boot_guard` and slot-init's) finds a slot's set through the set
records and hashes each file the set stores at its recorded length: a file the manifest lists against
the manifest's hash, each copy of the device tree against `board.dtb`'s, and `overlays/README` against its
fixed text (`extlinux/extlinux.conf` is checked entry by entry against the slot's files and command
line). A stored file the manifest does not list, or a listed file the set does not store, fails it.

**Not built: raw NAND.** Payload bays on NAND (UBI volumes) are parked. The `ubi` record keeps its files
under `slots/<id>` on its UBIFS boot volume as before, and its install is refused by name (the message
says UBI bays are parked); a board that asks for bays on NAND gets that refusal, not a partial install.
The `file` record has no bay path either: its install writes nothing to the boot partition (its loader reads one fixed kernel).

Board checks that cannot be made on this PC (how the firmware reads a pointer file with a torn sector,
whether `os_prefix=slots/bayN/` applies to every file it reads, U-Boot reading `slots/<bay>/` through
`sysboot`) are in `testing/board-runs/I5i8.md`, each marked untested on board.

### Firmware layout: what the firmware loads, checked

The Pi firmware has no error path a board can recover from: a kernel it cannot size, a missing device
tree or an overlay it cannot apply ends in a halt, and the boot ROM only tries again. So every file it
loads is checked against the rules it reads by, before anything reaches a board. `lemnos-ctl
firmware-check IMAGE` (crate `lemnos-firmware-check`) reads a boot partition image as the firmware does
and prints one `ok`/`BAD` line per rule, then `firmware-layout rule N: PASS` or `FAIL (n BAD)`, with a
non-zero exit when any rule fails. It reads only.

**A view** is one way the firmware reaches a directory: `config.txt` (a normal boot), `--trial`
(`tryboot.txt`, with `[tryboot]` on), or `--dir PREFIX` (`config.txt` with `os_prefix` taken as `PREFIX`).
The text is followed as the firmware follows it: `include`, the model section filters the board has
(`--filter NAME`; any other model filter is off, and a filter the check does not know is off and printed
as such), `[boot_count...]` (`--boot-count N`), and `os_prefix`, prepended to every file.

**The rules** (`RULE_VERSION` 1):

| rule | holds when |
|---|---|
| names | no `kernel=`, `cmdline=` or `device_tree=`: the firmware's default names only |
| kernel | the first default name that is there (`kernel_2712.img`, then `kernel8.img`; `--kernel NAME` to change); a gzip kernel is one member ending exactly at the end of the file, its CRC-32 right, its ISIZE equal to the decompressed size both in the trailer and as the file's last four bytes (the firmware sizes its output from those), and the arm64 Image magic after decompression; an uncompressed one carries the magic |
| dtb | each default device tree the board's firmware loads (`--dtb NAME`, one or more) is there, its `totalsize` is the file size, and it carries `__symbols__` |
| overlays | `overlays/README` under the prefix (so overlays load from the prefix, not the shared `overlays/`), `overlay_map.dtb` and `hat_map.dtb` there at their `totalsize`; each `dtoverlay=` and each `--needs-overlay NAME` (one the firmware applies by itself) there at its `totalsize`, and applying to every default device tree: each `__fixups__` label is in the tree's `__symbols__` and each `target-path` names a node (through `/aliases` when it is an alias) |
| cmdline | one line, nothing after its newline, no blank padding, at most 1600 bytes |
| initramfs | plain newc cpio ending at its trailer (followed by at most one 512-byte block of zeros, a cpio tool's padding), or gzip, zstd or xz ending at the end of the file; zstd is walked frame by frame and block by block, not decompressed |
| case | every file and the `overlays` directory show the name exactly as the firmware asks for it: a long name, or an 8.3 name with its lowercase flags (byte 12), as Linux vfat writes them |

The gzip end is found by inflating the stream (miniz_oxide, decompress only): no other reading says where
a gzip stream ends.

**Where a bad payload is stopped.** At creation, never on the device. The image builder (`lemnos-ctl
boot-partition`) runs the check on every Pi-form image it makes and writes nothing when a rule fails:
it prints the check's lines, then `the image fails <n> firmware-layout rule(s) (listed above): <file> was
not written`. Not built yet: the bundle step (`lemnos-ctl bundle`) running the same check and recording
the rule version in the signed manifest (`firmware_layout`), and the device's install refusing by name a
bundle whose manifest has no `firmware_layout` or an older one. Until then the device install verifies
the signature and the sizes only.
The check and its inflate are host-side: lemnos-ctl links them (and lemnos-ctl ships on images), the
daemon and slot-init never do, and `scripts/check-shipped.sh` holds that for lemnosd, lemnos-slot-init,
lemnos-good-boot and lemnos-orion, together with a licence allow-list for the check's dependency tree.
Layout version 6 (each file at its exact size, above) is the layout that passes this check; version 5
padded each file to its bay's size and failed it.

### Adding to an existing boot partition

`lemnos-ctl boot-partition --add-to DUMP` is for a board that runs another system from a FAT16 boot
partition: it adds this system's boot files beside that system's, so a slot can be started once by the
firmware's tryboot, and it changes nothing the existing system boots from. It works on a dump of the
partition, a regular file. A block device is refused: the caller writes the result back.

A slot started this way runs once and can't be confirmed. Confirming writes the default pointer, and the
default is the existing system's `default.txt`, which the add never makes ours, so the record write is
refused by name. On that boot slot-init logs that the firmware started a trial whose records can't be
read, that no attempt is taken and it can't be confirmed, and that the next restart starts the default.
To make a slot the board's own, the board gets a full image (`lemnos-ctl boot-partition` without
`--add-to`).

**Nothing is written under the other system's `/data/board/slots`.** The slots state on the data
partition is the existing system's, often in this system's own format (an older build of it), and that
system reads it on its next boot. So on such a boot:

- slot-init writes no `trial-init`, no `trial-started`, no `booted`, no boot-unverified mark, no meta or
  state, and on the fail path no `last-failure.log`. Its record goes to the console and the kernel log
  only. The decision is the fact the line reports: the firmware started a trial (its tryboot flag) and
  the trial records can't be read. The line ends in fixed words an operator's tooling can match:

  ```text
  slot-init: the firmware started a trial, and the trial records can't be read: <node>: default.txt record file is 55 bytes, expected 1024: the image is not ours; it runs with no attempt taken and can't be confirmed (confirming writes those records); the next restart starts the default; state dir left untouched: nothing is written under board/slots this boot
  ```

- the `system-slots` driver decides ownership with the record writer's own test: every record and
  pointer file is on the boot partition at the size the image builder writes (read only, through the
  partition's node). A file missing or of another size is another system's partition. While the boot
  records are not ours the driver writes nothing at start (no `TRIAL_ATTEMPTS` setting, no trial
  resolution, no boot history, no crash record, no count or grow) and nothing on any tick (no good-boot
  baseline, no slow boot-file check, no install). Every call that would write (`install`, `accept`,
  `install-file`, `write`, `set-default`, `trial`, `confirm`, `remove`, `pin`, `set-keep`, and the
  security switch's `apply`) is refused with `FAILED_PRECONDITION` before it reads or writes anything.
  Reads still answer. Its health is degraded with the fixed word
  `foreign: the boot records are not ours; the slots state here is another system's and is left untouched`,
  `list` and `info` carry the reason as `foreign` (`""` on a board whose records are ours), and the
  security switch's precondition lists it as `busy`. A partition that can't be read far enough to say
  (a device fault) is `foreign-unknown`: nothing is written, and it is looked at again every 8 s and on
  each call.
- under the daemon's unit the decision needs no write access: the daemon's root phase opens the
  partition read-only, probes it with the same test, and opens it read-write only when the records are
  ours (`lemnos_slot_store::boot_raw::prepare`); otherwise the driver holds a read-only descriptor at
  most. slot-init gives the daemon's dirs, `board/slots`, `slots` and the boot-file store
  `slots/bootfiles` (`Layout::daemon_dirs`, the one list), to the daemon's user at every owned boot
  (`lemnos_slot_store::owner::give_daemon_dirs`), the dirs alone and never through a link; the
  first-flash tree (`lemnos-ctl data-partition`) makes all three as root, and a board flashed before
  the store was in the list gets it at its next boot. With the records ours but any of the three not
  writable (still root's, or no `slots.conf` drop-in), health says `state-unwritable` and every call
  that would write is refused with `FAILED_PRECONDITION` before a boot record is touched (the dirs are
  looked at again at each such call and every 8 s, so a grant given since is taken); with the
  partition not handed over read-write, `record-unreachable`; a write that failed later is
  `write-failed`, and `info` and `list` name the file and the error as `write_failed`
  (`packaging/README.md`). The QEMU stanza `route-a` runs the real daemon through the unit launcher on
  both kinds of partition and reads the records back raw.
- Another system's records with no slots state at all (what slot-init leaves: it makes neither dir), and
  a partition of another layout version (layout 5 records), start the same way. The slots drop-in's paths
  are optional (`ReadWritePaths=-/data/board/slots -/data/slots`), so the unit's namespace setup leaves
  them out and lemnosd starts and reaches ready. Health says `foreign` with the reason, `list` and `info`
  carry it, every write is refused, and nothing is created under `/data`. Before the change a missing dir
  failed the namespace setup (226/NAMESPACE) and the restart loop reset the board. The stanza
  `route-a` checks the no-dirs and layout-5 starts as well as the state-present one.

```text
lemnos-ctl boot-partition --add-to DUMP --layout-of BOARD [--socs DIR] --slot ID --payload DIR
    [--firmware DIR] [--set N] [--replace-trial] --out FILE --changed-out LIST --undo-out UNDO
lemnos-ctl boot-partition --undo UNDO --of FILE --out RESTORED
```

`DUMP` is never written. The three outputs are new files (an existing one is refused), and the same inputs
give the same three files.

**What it adds.** Every file the image builder emits for the board, taken from the builder's own tree
(`bootimage::tree`), except `default.txt` and `config.txt`. Those two are the existing system's: they are
never created, replaced or resized. For a Pi firmware board that is `slot-layout.toml`,
`firmware-overlays.txt`, `previous.txt`, the records
(`trial.rec`, `reason.rec`, `episodes.rec`, `count.rec`, `SECURE.REC`), and under `slots/` the three bay
sets `bay0` to `bay2` with every bay file, `overlays/README`, each firmware overlay's bay and
`bayinfo.rec`. `--slot` is installed into bay set `--set N` (0 when not given) by the installer's write
part (`bays::stage_with`: the payloads in place, read back by hash, then the set's record), which moves no
pointer. The command works on the Pi firmware form only.

**`trial.txt` and `tryboot.txt`.** A partition that has neither gets ours. One that has them in another
form keeps them unless `--replace-trial` is given. With it, each is replaced: a new run is allocated and
written, the file's directory entry is rewritten in place (its first cluster and size, nothing else), and
its old clusters are freed, which is done only when no other entry's chain holds them. The old clusters
are not handed out in the same run, so the old bytes stay where they were. The output names each replaced
file with the size and SHA-256 it had. `trial.txt` written here names the set of `--set`.

**A second slot.** On a dump that already holds every one of these files (the result of an earlier run),
the command adds nothing and installs the slot into the set of `--set`, which must be empty; the slot must
not be in another set. The other sets are not written. A dump that holds some of the files and not others
is refused. A dump whose set records are another layout version's (layout version 5 and before) is
refused by the first record's path, and nothing is written.

**`bayinfo.rec` names its own region.** The record is a two-copy record of kind `BAY_INFO` (the
record form above): the region's first cluster and cluster count, the slot id, and each member's stored
length and the first eight bytes of its SHA-256. It carries no set number, so the same slot in the same
region has the same record bytes on any volume.

**Allocation.** The free map is read from the FAT: a cluster is free only when its entry is 0 in every
FAT copy, and a volume whose copies differ is refused. Each file is one contiguous run, first fit: the
lowest-numbered free run that holds it, files in the builder's order. A file is never split. When a file
has no run, the command refuses before anything is written, naming the file and the longest free run.
A directory that is missing is made (its own cluster, `.` and `..`); one that exists gains entries after
its last one. Names are the builder's: an 8.3 name as it is, any other as long-name entries and a
`BASE~N` short name no entry of the directory has.

**What it refuses**, each with nothing written:

- a block device, or anything that is not a regular file;
- a file that is not a FAT16 volume with 512-byte sectors: FAT12 and FAT32 are named;
- a dump that is not the size its boot sector says;
- FAT copies that differ, and a volume that is not sound (a chain that leaves the volume, runs through a
  free cluster or shares a cluster with another entry; a file whose chain is not the length its size
  needs; bytes after a directory's end marker);
- a name of ours that is already there and is not a file of ours (other than the two `--replace-trial`
  handles);
- a file with no run long enough, a full volume, a full root directory (512 entries, fixed) or a full
  existing directory;
- a set that already holds a slot, a payload bigger than its bay.

**The guard.** Before any output is written, every sector that differs from the dump is classified, and
the run is refused if one of them is in a cluster that was allocated before (other than a directory that
gained entries, or, for a second slot, the files of its own set), if an existing FAT entry or directory
entry changed (other than the two of `--replace-trial`), or if the boot sector changed. The volume is then
read again: every earlier entry must be where and what it was. Last, the command runs `--verify`'s checks
on the result and refuses unless exactly the expected lines are `BAD`: one for `default.txt` and one for
`config.txt`, and one each for `trial.txt` and `tryboot.txt` when they were left in the existing system's
form. The expected set is printed (`expected BAD: ...`), followed by the verify lines.

**The outputs.**

- `FILE`: the dump with the files added.
- `LIST`: every changed range of sectors, one line each, in sector order:
  `first-sector sector-count class sha256-before sha256-after`. Sectors are 512 bytes, counted from the
  partition's start. The classes are `fat1`, `fat2`, `root-dir`, `dir` (an existing directory that gained
  entries), `new-dir`, `new-file` and `own-file` (a second slot's bay files). A sector is listed if and only
  if its bytes differ.
- `UNDO`: the bytes every listed range held before. `--undo UNDO --of FILE --out RESTORED` gives the dump
  back, byte for byte; it refuses a file whose listed ranges do not hold what the run left there.
- On the output: one line per added file, `added PATH SIZE FIRST-CLUSTER CLUSTERS` (`added-dir` for a
  directory), `replaced ...` and `left ...` for the trial pair, the free map before and after, and the
  SHA-256 of the dump and of `FILE`.

**Writing it back** is the caller's step, not this command's: dump the partition, run `--add-to`, check the
dump's SHA-256 is still the partition's, then write each range of `LIST` from `FILE` to the partition at
the same sectors, read each range back and compare it with the `sha256-after` of its line. Writing the
`new-file` and `new-dir` ranges first, then the FAT copies, then `dir` and `root-dir` keeps every state
between two writes one the existing system reads as before: a FAT entry without a directory entry is
only an unused cluster. To undo, write the same ranges from the original dump (or from `RESTORED`) and
compare with `sha256-before`. The undo puts back every sector the add changed; a sector of a new file that
the add left as it was (zeros over zeros) and that was written later by the running slot keeps those
bytes, in clusters that are free again.

### Previous before flashing mode

When the board's loader cannot start the default, the previous slot is tried before the board offers
itself for flashing. The two boot forms do this differently, and only the U-Boot form decides it at the
moment the default's files fail to load.

| | U-Boot form (`packaging/uboot/boot.cmd`) | Pi firmware form (generated `config.txt`) |
|---|---|---|
| Order | in each pass: the trial (while it has an attempt), the default, the previous slot; three passes with a 2 s pause, then download mode (`fastboot usb`), and after each return the slots are tried once more before it is offered again | `config.txt`: `include default.txt`, then `[boot_count>3]` / `include previous.txt`, then `[all]` and the slot lines; `tryboot.txt` includes `trial.txt` and has no count filter |
| What picks the previous | the script, in the same pass, when the default's files did not load or did not start | the firmware's boot count: bit 2 set (counts 4 to 7, 12 to 15, and so on) reads `previous.txt` |
| What the previous is started as | a trial (see below) | a trial too: the previous slot is not the default, so slot-init writes `trial-init` for it and lemnosd judges it as a trial. It is not confirmed until it proves healthy, so if it also fails the count keeps rising to the rescue at 8. A boot that lands on it by the count is reported by lemnosd as described under "Boot count" |
| Flashing mode | download mode, after three passes that started nothing | RPIBOOT, requested by slot-init after 8 restarts without a good boot, and only from a refusal slot-init itself reaches (see "Boot count"); a firmware that cannot read the default's files never runs slot-init, so Lemnos asks for nothing there and the firmware's own `BOOT_ORDER` applies |
| Exists in generated files today | yes: `boot.cmd` (written into the image by `lemnos-ctl boot-partition`) | yes, the count filter is in `config.txt` (`lemnos_slot_store::firmware::CONFIG_TXT`); there is no Pi step that detects an unreadable default |
| Tested | in the gate: `testing/uboot/check.py` runs the script under stock U-Boot v2026.10 (`testing/uboot/build-uboot.sh`, pinned release, Raspberry Pi 64-bit build) in QEMU's raspi3b, with the disk as the SD card over a disk with the boot partition and the `uboot-env` partition, and checks each boot's slot, the environment it leaves, and the guards; structurally in `drivers/system-slots/src/uboot/tests.rs`; slot-init's side is run on image files | on this PC the text is only compared; the firmware's behaviour is **untested on board** (`testing/board-runs/I5i6.md`, rows 3 and 15, and `I5i8.md`) |

**The U-Boot form's third step, exactly.** The previous slot is named by the environment's `previous_bay`
alone: the environment holds no variable `previous` and no id for the previous slot (the loader needs
only the bay, which is what its files are read from). Its bay is protected from installs as the default's
is. The step runs only when `previous_bay` is set, is not `default_bay`, and is not the `trial_bay` the
first step just tried; otherwise it is skipped and the passes go on as before. The script sets and saves
nothing for it: no `saveenv` runs in a step, and `trial`, `trial_attempts`, `default` and the bays are not
touched, so a real pending trial keeps its attempts and its record. A test runs the script's choice over
the environment the real writers produce (`drivers/system-slots/src/tests.rs`,
`the_loader_script_reads_only_what_the_writers_wrote_and_its_previous_step_can_run`) and holds the
variables the script reads, the variables the writers write and the list in this section equal. That test
evaluates the script's conditions with a small evaluator; the gate's check (`testing/uboot/check.py`) runs the script under
stock U-Boot in QEMU, and the board evaluation is **untested on board** (`testing/board-runs/I5i8.md`, row U6). The step
loads `board.dtb` and `extlinux/extlinux.conf` from the previous bay, names the boot source in the tree as for the other steps, and, if the default had been named
and did not start, adds the property `lemnos,default-failed = "files"` to `/chosen` of the tree it hands to
the kernel. `previous_bay` is never repointed by the loader.

What slot-init then sees. The command line is the previous slot's own, which is not the board's default,
and that is what marks a trial boot: slot-init writes `trial-init` and, when the boot comes up,
`trial-started`. It reads the property: the default's `.meta` gets `FAILED=yes`, one more in `FAILURES`
and `FAIL_REASON=the boot loader could not load the default's files`, and, when the state holds no trial,
the state's trial becomes the previous slot with the error `slot <default> can't start: the boot loader
could not load the default's files`, as a fallback from a running default arms it. A trial the board
already holds is left as it is, and the default's failure is recorded anyway. Nothing is written to the
boot record or the environment by slot-init in this case. The previous becomes the default when lemnosd
confirms it, the same judgement as for every trial (the daemon ran for the uptime, no device regressed):
then the default switch writes `previous_bay`, `default` and `default_bay` as usual and the failed slot is
the previous one. If it does not come up, the board restarts and the passes run again; the failure of
the default is counted on each such start. The property is absent on every other start, which is then
unmarked.

### Pointer files, and the run-time rule for the boot partition

The Pi firmware's pointer files (`DEFAULT TXT`, `PREVIOUS TXT`, `TRIAL   TXT`, each two sectors holding
the same line) and the trial record are changed only by raw sector writes on the partition's block node
(`lemnos_slot_store::boot_raw::Boot`), found read-only through the FAT. A pointer file is written second
sector first, so at every cut at least one sector holds a whole line. A sector torn by a cut is refused
by its checksum, never read as another bay: an unchecked sector could be cut into a whole, valid,
different line.

No run-time code mounts the boot partition for writing. slot-init's only boot mount is a read-only
one for raw NAND's UBIFS volume, which has no raw reader. lemnosd finds the node from the boot
source and the board's `boot_sources` (see [The boot source](#the-boot-source)) and writes it raw.
An image without the pre-created files refuses the write by name; nothing falls back to a
filesystem write. The trial's reason is kept the same way, in the pre-created `reason.rec`
(`lemnos_slot_store::boot_raw::REASON_FILE`): a missing `reason.rec` refuses the reason by name, and
the boot goes on from the kernel log alone. On raw NAND (`record = "ubi"`) no reason is kept on the
boot partition at all: there is no `reason.rec` there, so the reason is in the kernel log only.

The records a boot may write (`TRIAL   REC`, `REASON  REC`, `EPISODESREC`, and the three pointer
files) are created when the image is built, and nothing creates them at run time.

Arming a trial writes the trial pointer first and the record second. A cut after the pointer leaves
no record, so no tryboot restart happens; the record is written only once the pointer names its slot.

Promoting the default (`DEFAULT TXT`) is the two-sector write above, after `PREVIOUS TXT` names the old
default's bay. A default whose boot files don't match its manifest is moved to the previous slot by that
write; the next boot starts the previous.

### Boot count: the fallback on the Pi firmware

The fixed `config.txt` (`lemnos_slot_store::firmware::CONFIG_TXT`, on the Pi 5 family) reads `default.txt`
unless the firmware's `boot_count` is above 3 (`[boot_count>3]`, `conditional.adoc` at `raspberrypi/documentation`
commit `b01a5c1a`), when it reads `previous.txt`. Boots 0 to 3 run the default and 4 and later the previous, until
a good boot or a power loss clears the count (see "The count's life" above). `tryboot.txt` has no count filter. The filter, the `kernel_watchdog_timeout`
keys and the count's read and clear are verified from the Raspberry Pi documentation
(`conditional.adoc`, `config_txt_boot.adoc`), not yet on a board: `testing/board-runs/I5i6.md`.

**The rule for `previous.txt` (M36b).** `previous.txt` names the default's bay until a real previous
slot exists, and never an empty bay. On a fresh image all three pointers name `bay0`, where the first
slot is installed, so a count with bit 2 set (4 to 7, 12 to 15, ...) starts the default's own files: a
fresh board does not start from an empty bay. A default switch points `previous.txt` at the old
default's bay (`Boot::set_pointer`). Removing the previous slot's bay points `previous.txt` back at the
default's bay before the old bay is freed (`Boot::free_slot`); removing the default's bay is refused.
The restart count that slot-init and lemnosd act on is not the raw count but the restarts since the
last good boot (`COUNT   REC`, see `lemnos_slot_store::firmware::since_good`); the firmware's own
rotation above still reads the raw count.

- The count is read from `/proc/device-tree/chosen/bootloader/count` and the reset status from
  `rsts` (four big-endian bytes each; `device_tree` names the directory). It is cleared through the
  property mailbox on `/dev/vcio` (`vcio`), tag `0x0003808d` with the value zero
  (`lemnos_driver_rpi_firmware_sys`). Both settings are the `system-slots` device's; the defaults
  are the paths above. `/dev/vcio` is `root:root 0600` on a board and the daemon runs as its own user, so the
  slots device's root phase opens it before the drop and holds it for the daemon's life; every
  clear takes a copy of that descriptor. The root phase's line says `<vcio>: held for the boot
  count`, or why it is not held (then no clear can work, and the health says each failure).
- The count is cleared at a boot that is good (by lemnosd: a confirmed trial, or a default boot that
  has run its uptime with its boot files matching the manifest) and before each restart Lemnos makes:
  lemnosd's restarts (the restart check runs first: a refused check makes no restart), and slot-init's
  restart into a trial (`0 tryboot`), which clears the count and sets the restart baseline to 0 before
  it restarts. slot-init clears nothing else: a default that fails in it, or after it, still counts.
  Without that clear, a trial whose kernel never boots pushed the board to `previous.txt`: each attempt
  is two boots (the default's pass and the trial's hang), so the fifth boot read a count of 5, above
  `[boot_count>3]`, and the board landed on the previous slot instead of the default, where the daemon
  took it as an arrival by the count against the default (an episode, and a demotion at the second
  such trial with the default threshold). Test:
  `a_trial_whose_kernel_never_boots_comes_back_to_the_default_and_the_count_stays_low`
  (`service/slot-init/src/boot/tests/e2e.rs`, the firmware modelled boot by boot; it lands on the
  previous before the fix). A count that can't be cleared does not refuse the restart the board was asked for:
  the state names it ("this boot is counted") and the indicator shows failed.
- **A refused clear is retried (M36c).** A clear the mailbox refuses after a good boot, or on an arrival
  from the count, is owed: the driver's tick tries it again, the wait starting at 1 s, doubling after
  each refusal and stopping at 8 s, and it never gives up or turns off. While it is owed, `health` is
  degraded, `retrying` shows the attempts and the next try, `info` carries `"boot_count"`, and the state
  says `the boot count is not cleared: <why>; retrying every Ns`. When the clear goes through, the state
  says `the boot count is cleared after N tries` once. A clear refused on the way to a restart is not
  retried: the board restarts, and the next good boot clears the count.
- A boot that arrives on the previous slot because of the count (the reading picks previous, and
  the board runs the previous) is reported first: the state's error and the boot history's entry
  for this boot name the count, `rsts`, and whether the default's files match. Then:
  - the default's files don't match its manifest: the default is marked failed, `default.txt` is
    pointed at the previous slot, and the count is cleared.
  - the files match and the last reset was a warm reset (`rsts` = `0x20` or `0x1020`): one episode is
    recorded against the default in the two-copy record `episodes.rec` (FAT name `EPISODESREC`, kind `EPISODES`), and the
    default is demoted when the episodes reach `demote_after_episodes` (the board file's key, default
    2, minimum 1). The episode is written before the count is cleared.
  - the files match and the last reset was a power-on reset (`rsts` = `0x1000`), or the full `rsts`
    value is one no rule names (`unknown`, reported with its raw value): no episode. The count is cleared.
  - Where the default stays, the next restart tries it again. A boot that is good clears the
    episodes of the slot it runs.
- **Refusals that repeat go to the firmware's flashing mode, on a Lemnos disk only (M36c).** A refusal no
  slot can answer (the boot disk or table is wrong, the data volume is missing, a kernel piece is absent)
  goes to the rescue: slot-init keeps the reason in the kernel log, waits 30 s and restarts. The flashing
  mode is asked only when the boot disk has exactly one partition labelled `lemnos-data` (or the data
  partition is already found); any other refusal, including an unreadable or absent disk and raw NAND,
  logs that it is not a Lemnos disk and restarts into the board's own boot order. The same refusal
  repeats on every restart, so slot-init also reads the firmware's count at the rescue: when `FLASH_AFTER`
  (8) restarts have gone by without a good boot (the daemon clears the count at a good boot, so the
  count is the run of restarts since the last one), it asks the firmware for RPIBOOT at the next boot
  instead (mailbox tag `0x0003808b`, value 3, the reply word `0x80000000` checked:
  `Mailbox::request_rpiboot`) and restarts. Nothing is written to the boot partition and no slot record
  changes. The count is the firmware's, so it also counts restarts for other causes: eight
  restarts without a good boot, whatever the cause, is the bound. Eight rides out a transient (a slow
  card, a late device) and, at 30 s plus a boot each, is a few minutes; it is not smaller because RPIBOOT
  has no timeout. The request applies to the next boot only and the stored `BOOT_ORDER` is never
  changed; a board sent there sits waiting for a USB host until it is flashed or power-cycled
  (BCM2712 boards, the Pi 5 class). The reason is in the kernel log and the saved log; the Pi keeps
  nothing across the restart without a boot-partition write. Not verified on a board: the request
  from the initramfs: `/dev/vcio` is never a node baked into the image. slot-init mounts devtmpfs on
  `/dev` first of all, before any path that can need the mailbox (a test pins the order), and
  devtmpfs makes `/dev/vcio` when the driver is built in. The kernel must therefore have `BCM_VCIO`,
  `BCM2835_MBOX` and `RASPBERRYPI_FIRMWARE` (with the menus `BRCM_CHAR_DRIVERS` and `MAILBOX`) built in, not
  modules: they are the `firmware.rpi-tryboot` group of `contracts/slot-kernel.toml` (version 2 added it; the file is version 3),
  checked by `scripts/check-kernel-config.py --check-config <.config> --firmware rpi-tryboot`, which
  names a symbol that is `=m` or unset. A test ties slot-init's use of the mailbox to that group.
  Still to confirm on a board: `testing/board-runs/I5i6.md`. A failed request restarts the old way.
- **Boots that die after slot-init reach the flashing mode too, where the board says so.** A boot can pass
  slot-init and die after it: a reset after the switch to the slot's root, or a daemon that never feeds the
  watchdog. slot-init makes no refusal then, so the rescue above never sees it. On a freshly flashed disk,
  `previous.txt` names the default's own bay, so the firmware's count fallback starts the same slot again, and
  the board loops on its disk. The count still shows the run, because only a good boot moves the baseline. A
  board file with `[boot] loop_return = "rpiboot"` (the Raspberry Pi 5 family only; any other value or SoC is
  refused at build by name) gets `<prefix>.loop_return=rpiboot` on each slot's command line. With it,
  slot-init checks the restarts since the last good boot as soon as the data partition is found, before any
  slot starts. At `FLASH_AFTER` (8) or more, wherever those boots ended, it says so at the kernel log's error
  level with the count and the baseline, asks the firmware for RPIBOOT as the rescue does, and restarts into
  it. A request the firmware does not take is said, and the boot goes on.

  A healthy board never reaches the limit. Each good boot keeps the count it started from as the baseline,
  so plain restarts and trials, each boot good, count at most the restarts since the last good one. A failed
  trial adds at most two: its last hang and the default's boot after it, since the restart into each attempt
  clears the count. A board whose daemon comes up but can't read its boot
  records to say whose they are (`foreign-unknown`) judges no boot good, so the baseline doesn't move. Once
  the daemon has run its uptime, it clears the firmware's count through the mailbox instead, and writes
  nothing to the records or the state. The next boot reads a count below the baseline, as after a power
  loss, so an ordinary restart of such a board never counts toward the limit. On another system's records
  (`foreign`), nothing is touched. Without the key, only a refusal asks for the flashing
  mode, as before. Tests: `boots_that_die_after_this_program_reach_the_flashing_mode_at_the_limit_before_any_slot_starts`,
  `a_healthy_board_across_plain_restarts_and_trials_never_reaches_the_flashing_mode`,
  `below_the_limit_or_without_the_way_back_the_boot_starts_its_slot` and
  `a_way_back_the_firmware_refuses_lets_the_boot_go_on`. On a real kernel (an arm64 QEMU guest, a boot
  partition from `lemnos-ctl boot-partition`, the count in `chosen/bootloader`), the request comes before
  any slot starts. The guest has no mailbox, so the restart into RPIBOOT itself is a board check
  (`testing/board-runs/I5i6.md`).
- Limits, stated: only the last reset's cause is visible; a broken previous alternates with the
  default for ever and nothing reports it (the fixed boot stage is the answer to that case); a
  power cut clears the count, so a cut in the middle of an arrival counts once at most; a plain
  `reboot` outside Lemnos counts one boot, which a good boot clears.
- `lemnos-ctl boot-partition --layout-of BOARD ...` writes the whole boot partition for a board whose
  record is `tryboot` or `uboot`: the firmware's
  text (`config.txt` and `tryboot.txt`: the fixed text, a header that says it is generated, and the
  board's `gpio=` lines after it), the pointers and the record files, and the bay sets. `lemnos-ctl tree
  --boot-dir` is removed (the flag is refused). Other records have no boot partition here and are refused
  by name.

### The U-Boot restart count

The `uboot` form has the Pi's count in U-Boot's environment: `lemnos_restarts` is the boots attempted since the
last good boot, this one included, and it saturates at 8. `packaging/uboot/boot.cmd` raises it once per script
run, with one `saveenv`, before any slot starts. lemnosd clears it at a good boot and before its own intended
restart, with a write only when it is not already 0 (so a healthy boot writes nothing).

- **The rung.** `lemnos_restarts & 4` (the U-Boot counterpart of the Pi's `[boot_count>3]`): at 0 to 3 the default starts, at 4 to 7 the
  previous starts first (the default is skipped in the first pass only, and taken on the next pass if the
  previous does not start), at 8 no slot starts and download mode is offered at once (the Pi's RPIBOOT
  equivalent; the host flasher is the exit). The previous at 4 is `lemnos,default-failed = "restarts"` on
  `/chosen`. slot-init treats the U-Boot previous exactly as it treats the Pi's count-picked previous: a slot that
  isn't the default is a trial (`trial-init` is written, `slot-init` boot.rs:1298), and only the value `files`
  marks the default failed (the test `a_count_picked_uboot_previous_is_a_trial_as_the_pi_previous_is` pins this).
- **Episodes.** An arrival on the previous slot by the count, with the default's files matching, records one
  episode against the default (`lemnos_episodes`, with the default's slot id in `lemnos_episodes_bay`). The
  episodes demote the default at `demote_after_episodes`, as the Pi's do, and the count is cleared after the
  arrival. A good boot clears the episodes of the slot it runs.
- **Power-on resets.** A power-on reset resets the count to 0 only when the image names the SoC's reset-status
  word (`uboot_reset_word`, see [Boards](boards.md#the-keys)). The word's bits are the CRU's global reset
  status, and a SoC gets a word only with a citation for its bits. No SoC has one today, so every boot counts,
  power-on included, and the count does not distinguish a power cut from a restart on any shipped board.
  The count's semantics mirror the Pi's, with that difference stated.
- **Writes.** A healthy power-on boot writes nothing. A healthy warm boot writes twice: the raise and the
  clear at its good boot. A failing boot writes once per restart. Rescue at 8 writes nothing while it waits.
  Every write is the cut-safe copy write of the environment (`drivers/slot-store/src/uboot_env.rs`).
- **Limits.** A boot that never reaches the script counts nothing. The U-Boot form has no baseline record: the
  count is the environment variable, cleared by its own write, so the Pi form's baseline (`count.rec`) does not
  apply here.
- **The rescue's exit.** Download mode at 8 waits for the host flasher, which is also the operator's default
  switch: a default switch clears the count and the episodes.

### The good-boot deadline

Every boot of a slot board is judged, by systemd and not by lemnosd. The unit
`packaging/systemd/lemnos-good-boot.service` runs at boot as a oneshot with no dependency on lemnosd, so it
still decides when lemnosd is wedged or never starts. It polls once a second (`lemnos-good-boot`,
`service/good-boot`) and exits 0 as soon as the boot is good, or 1 when the deadline passes.

**What good means, and only that.** Two facts: lemnosd has reached ready (its unit is `active`, which for
`Type=notify` means it sent `READY=1`), and the booted marker `board/slots/booted` on the data partition
names a valid slot. The marker is the one slot-init writes before `switch_root`; nothing else is written
for it. Device health is not a fact here: a failing device does not make a boot bad, and it does not make
it good. The rule is `lemnos_slot_store::goodboot::good`.

**The deadline** is the board file's `good_boot_s` on the `system-slots` device (seconds, default `300`,
from 30 to 3600; see [Boards](boards.md#good-boot-and-the-data-partition-system-slots)). A value outside
that range, or one that is not an integer, is refused by name at the daemon's start and by
`lemnos-ctl tree --good-boot-unit`. The image gets the deadline through a drop-in that
`lemnos-ctl tree --good-boot-unit FILE` writes, to `/etc/systemd/system/lemnos-good-boot.service.d/deadline.conf`:
its `ExecStart` carries `--deadline-s N` and its `TimeoutStartSec` is `N`, so the waiter and systemd agree.
The unit's condition is that drop-in, so a board with no system-slots device never runs it.

**Not good by the deadline means a restart.** The unit has `FailureAction=reboot-force`. On a trial boot
the restart falls back through tryboot, as any boot that does not reach the daemon does. On a confirmed
default each restart raises the firmware's boot count, so the count chain (previous at counts 4 to 7, RPIBOOT
at count minus the baseline of 8 or more) is what drives recovery when a default never boots good.

**Why `reboot-force`.** `reboot` stops every unit first, and lemnosd's stop can take its `TimeoutStopSec`
(30 s) or hang on a wedged driver, which holds the reboot. `reboot-force` has systemd call the reboot at
once. It is the choice for a failure of the boot, where nothing else is to be stopped cleanly. The state
files are each replaced whole and synced (see the store's module), so a reboot at this point cuts nothing
half-written.

**lemnosd is not changed for this.** `lemnosd.service` keeps `Restart=always`, `StartLimitIntervalSec=0`
and no failure action: lemnosd is never down by design, and its own failure never surfaces, which is why
this separate unit exists.

**Tests.** `lemnos-good-boot`'s tests run the waiter against a scripted probe and a fake clock: the
marker appearing late is good at that time; a boot never good fails at the deadline exactly; the last sleep
is cut to the deadline; a marker without readiness, or one naming no slot, is not good; the waiter asks
only the two facts; the real probe reads the marker from a directory. The drop-in, the board-file key and its
refusals are tested in `lemnos-ctl` and in `lemnos_slot_store::goodboot`. Not tested here: a QEMU guest that
reboots when good never comes (the harness does not yet boot systemd with this unit), and `systemctl` against
a running lemnosd. `systemd-analyze verify` runs on the unit offline where it is installed (see the
gate's packaging check).

### Growing the data partition at first start

The data partition is entry 2 of the MBR: the last entry, type `0x83`, labelled `lemnos-data`. An image
writes it at a size its content needs, and a disk larger than that image leaves the rest unused. When the
board file's `grow_data` on the `system-slots` device is on by default (`false` opts a board out), the
boot grows it to the disk's end at each start until it is done. The pass runs in slot-init, before the
slot's state dirs are given to the daemon and before the switch into the slot: there the data partition is
mounted writable and the boot disk's node is still root's, so the daemon's user needs neither and its
`/data` stays read-only. The pass is `lemnos_slot_store::grow` (`run_boot` runs it on a thread, polled
with the watchdog fed, and reports its state); the daemon only reads the state and never opens the disk
(`drivers/system-slots/src/grow.rs`). It acts only on a disk that validates as the Lemnos layout, so any
other table is refused with words and nothing is written. The arithmetic is `lemnos_slot_store::mbr`.

The boot leaves its state in `board/slots/grow-state` on the data partition, one line: `running <started
unix ms> <bound ms> <target bytes>` while a pass is in flight, then `not-needed`, `grown`, `refused <state>
<words>` (named, nothing written), `unconfirmed <baseline|-> <seen> <words>` (the kernel accepted the
resize and its view did not show it), or `failed <words>` (every try in that boot failed; the next boot
tries again). A failed try is repeated on the capped backoff for as long as the overrun bound of the disk's
size allows, then the boot goes on. A pass the kernel never returns from is not cut short: the daemon
reports it running past its bound. The daemon's `grow_state` info key says what the file says. An image
with no state file yet reports `pending`. The board file's `grow_data = false` reaches the boot on the slot's command line as `<boot_args>.grow=0`
(`lemnos-ctl tree --cmdline-slot` writes it from the board file). The boot then records `not-needed off in
the board file` and opens no disk, and the daemon reports that state. An attempt whose call never returns
is written first as `attempt started <boot id> <started ms> <bound ms> <target>`. The boot feeds the
watchdog only up to the hard bound (the overrun bound plus 15 s), then stops feeding, so the watchdog resets
the board. The next boot finds that attempt from an earlier boot id and records `refused grow-stalled: the
last grow did not return (boot <id>)`: it does not try again on its own. `lemnos-ctl grow-retry <data
mount>` (run as root) removes that one refusal, so the next boot tries once more.

The steps, in order. Each reads the state it finds, so the pass is the same whether it runs once, is cut, or
runs again:

0. **The baseline.** Before anything is written, the pass reads the stored baseline: the file
   `board/slots/grow` on the data partition (`lemnos-slot-store`'s `store::replace`: written to a temporary
   file, synced, renamed over it, the directory synced). When the table is to grow and no record exists, the
   filesystem's size on the mount (`statfs`, bytes) and the target partition length (sectors) are stored
   first, so a grow that has begun always has a baseline. The record is two numbers and a checksum; a record
   that does not decode, or that is for another length, is named by the confirmation as corrupt. A record
   that is missing although the table already holds the grown length is named as missing.
1. **The table.** The MBR is validated (its signature; entry 2 is `0x83`; no entry follows it; the kernel's
   start of partition 2 equals the table's; the partition's superblock is ext4 with the label `lemnos-data`;
   the table's end is at or before the disk's end). The new length is the start to the last whole MiB
   before the disk's end. The only write is the four bytes of entry 2's sector count (474 to 477 of sector
   0), then a flush, then a read back that must differ from the old sector in those four bytes only.
2. **The kernel's partition.** `BLKPG_RESIZE_PARTITION` on partition 2 to the table's length
   (`lemnos-linux-sys`, `blockdev.rs`), then `/sys/class/block/<p2>/size` must say that length.
3. **The filesystem.** `EXT4_IOC_RESIZE_FS` on `/data` to the table's length in blocks
   (`lemnos-linux-sys`, `ext4.rs`). The superblock is not read back after it: while the filesystem is
   mounted its new block count is held by the journal until a checkpoint, so a read of the device still
   shows the old count after a resize that succeeded. The kernel's size of the mount (`statfs`, the
   filesystem's total bytes) must grow past the stored baseline. It is checked on each pass that resized,
   up to 6 checks (`CONFIRM_TRIES`, 31 s on the backoff). When the view confirms the growth the stored
   record is cleared. When six checks do not show it, the state is `grown-unconfirmed`: `info` gives
   `grow_state`, `grow_baseline_bytes`, `grow_seen_bytes` and `grow_reason`, and `health` is degraded with
   that name. The passes go on on the same backoff, and the state stays until the view shows the growth.
   A baseline that is missing or corrupt cannot confirm anything: the state is `grown-unconfirmed` at once,
   and `grow_reason` says it is missing or corrupt. A restart after the resize and before the confirmation
   reads the stored baseline and confirms against it, so it does not report a grown disk as unconfirmed.
   The six checks count from the start of each daemon run, so a restart starts them again.

Nothing else is written. Partition 1 is never touched; sector 0 changes in four bytes; the kernel's view
and the filesystem only grow.

**A refusal writes nothing.** A table that is not the expected layout is refused by name at step 1 with the
reason, and the disk is left as it was: `data partition is not the last one: nothing grown`, the entry's
type, a label other than `lemnos-data`, a disk smaller than its table, a sector without the signature, and a
partition that is not ext4. A filesystem larger than its partition is refused at step 3 and not shrunk.

**A failed step is retried, never given up.** The driver runs a pass at its start tick and does not hold up
its start. A failed pass is noted in the state's error in words, once per reason, and tried again after a
backoff that starts at 1 s, doubles, and is capped at 8 s. A pass that finishes is dropped. The boot disk's
node comes from the same resolver the boot chain uses; when it is not known the reason is noted and the
pass is retried.

**Cut points.** Power cut or daemon restart before or after each write (table, kernel, filesystem). The next
pass after a power cut sees the kernel read the table, and after a daemon restart the kernel keeps its view;
either way it ends in exactly the state of an uninterrupted pass. Tested on a file standing for the disk, with the fake kernel
seam, for all six cases in `drivers/system-slots/src/grow/tests.rs`:

| Cut | Table | Kernel | Filesystem | The next pass |
|---|---|---|---|---|
| before 1 | old | old | old | writes the table, the kernel and the filesystem |
| after 1 | new | old | old | after a restart: resizes the kernel, then the filesystem. After a power cut the kernel already has the new length: the filesystem only |
| before 2 | new | old | old | after a restart: resizes the kernel, then the filesystem. After a power cut: the filesystem only |
| after 2 | new | new | old | grows the filesystem only |
| before 3 | new | new | old | grows the filesystem only |
| after 3 | new | new | new | nothing: the pass is a no-op and writes nothing |

A torn table write (the count's top byte lands wrong) is caught by the read back: the sector must be the old
one with exactly the four count bytes replaced, and the pass fails otherwise, naming the bytes that changed.
A new count whose byte equals the old one is not a refusal.

**Off the tick.** A pass runs on its own thread, one at a time, and the driver's tick only starts it and
takes its result (`try_recv`), so the tick never waits on the kernel and the trial watch and every other
call keep their cadence. The resize cannot be interrupted, so its bound only names an overrun: the bound is
10 s plus 30 s per TiB of disk, and past it `info` says `grow_state` `resize-overrun` (with
`grow_elapsed_ms`, `grow_bound_ms`, `grow_started_ms`, `grow_target_bytes`) and health is degraded. The
result is still taken when it arrives. Measured on the mounted builder shape (QEMU, NVMe): the resize takes
about 0.5 s at 15 GiB, 2.2 s at 512 GiB and 8.6 s at 2 TiB (about 4.3 s per TiB), so a 2 TiB disk exceeds
the 2 s call deadline the old on-tick call would have hit.

**The 256 GiB cap.** `grow::MAX_BYTES` is 256 GiB: the largest data partition a grow may make. A target above
it is refused by name before anything is written, with the sizes:
`grow-too-large: the data partition would be N bytes (M GiB) and the maximum is 274877906944 bytes (256 GiB):
nothing changed; the boot continues with the partition as it is`. `info` gives `grow_state` `grow-too-large`,
`grow_target_bytes`, `grow_max_bytes` and `grow_reason`, and the driver notes the reason once. Health is not
degraded by it: a partition left as it is is the ordinary state of a board with `grow_data` false. The media
this runs on is at most about 16 GB, and the bench cards go to 64 GB. Tests: `grow/tests.rs`
`a_grow_to_more_than_256_gib_is_refused_by_name_and_nothing_is_written`,
`a_grow_to_256_gib_or_less_is_allowed`, `a_refused_grow_is_named_once_by_the_driver_and_never_tried_again`.

**The QEMU check (`testing/qemu/run-grow-live.py`, guest init `testing/crates/grow-live-init`).** The engine host
runs with `system-slots` on the real disk and `hw-watchdog` on a QEMU i6300esb, a 2 TiB, 256 GiB or 64 GiB disk
with the builder's data filesystem, and a latency probe on `host.call` every 100 ms. Each case copies partition 2
out of the image, then checks it twice: `e2fsck -fn` (as the disk is) and `e2fsck -fy` after the journal replay,
which must change nothing and exit 0. A case passes only when the replayed check is clean, the block count is the
full partition, and the baseline record is gone. The results below were run on NVMe-backed QEMU, not on a board.

| Case | Disk | Result | Resize (driver clock) | Probe: calls, max latency, max gap | e2fsck -fn / -fy | Blocks |
|---|---|---|---|---|---|---|
| refuse | 2 TiB | `grow-too-large` at 0.58 s, nothing written | none | 5, 4.9 ms, 100.5 ms | 0 / 0 | 9728 (unchanged) |
| full 256 GiB | 256 GiB | finished | 0.27 s to 2.11 s (about 1.8 s) | 21, 2.0 ms, 103.0 ms | 0 / 0 | 67087616 |
| reset at 0.2 s | 256 GiB | 2 boots, full size | first boot cut at 0.21 s | 27, 4.4 ms, 105.8 ms | 0 / 0 | 67087616 |
| reset at 1.0 s | 256 GiB | 2 boots, full size | first boot cut at 1.02 s | 49, 2.6 ms, 102.8 ms | 0 / 0 | 67087616 |
| reset at 2.0 s | 256 GiB | 2 boots, full size | first boot cut at 2.01 s | 5, 1.0 ms, 100.9 ms | 0 / 0 | 67087616 |
| full 64 GiB | 64 GiB | finished | 0.27 s to 0.67 s (about 0.4 s) | 7, 0.9 ms, 100.4 ms | 0 / 0 | 16755968 |

Every case has no baseline record left, one or two boots, and the watchdog never fired. The 2 TiB disk is refused
by the cap, so the earlier 2 TiB runs (before the cap) are not part of the shipped check; they are kept as the
evidence for the decision below.

**Decision: the resize stays on its own thread.** Evidence, all on the same 2 TiB disk shape:
- The synchronous code (`c79935c0`) blocks the host's loop for the whole resize. Its `info` calls failed with
  `Status(-110)` at the 2 s deadline, then `Status(-19)`, the slots device reported `Failed("retrying")`, and the
  harness got no reply in 240 s. The threaded code (`8d01f944`) on the same disk: the tick returned at once, the
  probe's largest latency was 4.3 ms, and the resize finished in 17.1 s.
- The e2fsck failures and the stuck second boot recorded in GROW2 were not caused by the thread. Both come from the
  same kernel call, which the synchronous code makes too (see below).
- The "four times slower" figure (30 to 33 s against 8.6 s) is not explained by the thread: it only calls the same
  `EXT4_IOC_RESIZE_FS`. The synchronous run did not finish, so its resize time was not measured. The 17.1 s
  threaded run and the earlier 30 s runs differ on the same host; the cause of that spread is not known.

**What the GROW2 failures were.**
- The e2fsck exit 12 was the harness. Its partition copy mixed `os.lseek` on the descriptor with a buffered
  file's `seek` and `read`: a buffered seek inside its own buffer does not move the descriptor, so reads after a
  `SEEK_HOLE` came from the wrong place and blocks were lost (block 1077 of the data directory read as zeros
  where the raw image holds it). The copy now uses `pread`/`pwrite` at absolute offsets. With that copy the three
  finished 2 TiB runs are clean.
- The second boot that stuck (2 TiB, reset 6 s into the resize, before the cap) was a kernel defect. The journal
  held the resize (`needs_recovery`). After the replay, `e2fsck -fy` found "Last group block bitmap uninitialized"
  and wrong free block and inode counts, although `e2fsck -fn` (which does not replay) had called the copy clean.
  The second boot's mount logged `error 117` (EUCLEAN), and its resize call hit `kernel BUG at fs/ext4/resize.c:546`
  (`BUG_ON` in `setup_new_flex_group_blocks`: `group_data[0].group != s_groups_count`) on the grow thread. That
  thread never returned, and the unmount was busy. The kernel is `7.3.3` (pinned). This is ext4's online resize
  not being crash-consistent across the groups it adds. It is in the kernel, not in Lemnos, so it is reported
  there and not fixed here. The cap keeps a grow to at most 256 GiB; at that size three mid-resize resets came back
  clean, which is evidence, not a proof.

**Not proved here.** The card or eMMC writes one sector whole on a power cut, the board's own timing, and the
watchdog's feed on a board. The board check for a first boot is GROW3's (`testing/board-runs/GROW.md`).

**Measured in QEMU (not on a board).** With the kernel's own partition resize and online resize on an NVMe
disk, mounted `/data`, the builder's shape (9728 4 KiB blocks, 112 inodes per group, 4 reserved GDT blocks):
a 15 GiB disk grows partition 2 to 16,019,095,552 bytes and the filesystem to 3910912 blocks in one pass of
about 0.58 s (the resize itself 0.52 s); a 64 GiB disk grows it to 16755968 blocks in one pass of about
0.74 s (the resize 0.66 s). The kernel converts the filesystem to `meta_bg` online at 64 GiB, so the 4
reserved GDT blocks do not cap it. Inodes after the grow: 13440 at 15 GiB, 57344 at 64 GiB (112 per group).
A second boot finds nothing to do and writes nothing, in 13 to 18 ms. Those are the only claims; the card or
eMMC's speed and its sector atomicity on a power cut are not measured.

**Not proved here.** That the card or eMMC writes one sector whole on a power cut, and the board's own timing.
The board check is not written yet (no file in `testing/board-runs/`), and the test seam shares none of them with the kernel.

**The image-builder contract.** The builder makes the data filesystem with 4 KiB blocks (`mke2fs -b 4096`),
one inode per 16 KiB (`-i 16384`), `meta_bg` with `resize_inode` off (`-O meta_bg,^resize_inode`: the two cannot both be on), 256-byte inodes (`-I 256`), no reserved blocks
(`-m 0`), a 4 MiB journal (`-J size=4`) and the label `lemnos-data`. The first boot grows it online through
meta_bg: groups are added to a meta_bg filesystem without the reserved GDT blocks that cap the non-meta_bg
shape. The QEMU check builds exactly this shape.

**GPT disks.** The grow reads the table as GPT when sector 1 starts `EFI PART`, and the MBR path is unchanged
otherwise. The data partition must be entry 2 and the last used entry. The usable area ends 34 sectors before
the disk's end, and the grown end is the last whole MiB of it. The writes are, in order, each synced before
the next (`grow/gpt.rs::grow_table`):

1. zero the stale backup (its array and header at the primary's alternate LBA), which lies past the old
   partition's end: the primary still reads the old table;
2. the new backup's array at the new end;
3. the new backup's header: the backup now reads as the new table;
4. the primary's array: its CRC no longer matches the old header, so the backup is read;
5. the primary's header, the commit. It is read back and must hold the new end.

Every write is bounded to its own sectors before it is made. A cut after any write leaves the old table or the
new one readable, never neither, and the next pass finishes the grow from whichever it finds. A primary that is
damaged with a new backup is repaired from the backup (`repair_primary`). Tests:
`grow/tests.rs::a_gpt_last_partition_grows_and_both_copies_move_to_the_new_end`,
`...::a_stale_backup_gpt_past_the_old_end_is_zeroed_before_the_grow_writes`,
`...::a_gpt_grow_cut_after_any_write_reads_as_the_old_table_or_the_new_one` (cuts after writes 1 to 12, then a
restart that finishes the grow), `...::a_damaged_primary_is_repaired_from_the_backup_when_the_table_is_already_grown`.
Not done: the protective MBR's size field is left as it was (readers do not use it for the partition), and a
stale ext4 superblock elsewhere past the end is not scanned for; only the stale backup is cleared.

**Named states.** A grow that cannot run in this start is named in `info` as `grow_state` with `grow_reason`, is
noted once, and is not retried until the next start:

- `grow-too-large`: the target is above 256 GiB; `grow_target_bytes` and `grow_max_bytes` are given.
- `data-read-only`: `/data` is mounted read-only (`/proc/mounts`); nothing is written.
- `gpt-unavailable`: the GPT is unusable with a reason: neither header reads, partition 2 is unused, a used
  entry starts after partition 2, the backup is not past partition 2's end, or the primary is damaged while the
  backup still holds the old table.

Tests: `grow/tests.rs::a_gpt_with_no_readable_copy_is_the_named_unavailable_state_and_nothing_is_written`,
`...::a_gpt_partition_two_that_is_not_the_last_is_the_named_unavailable_state`,
`...::a_read_only_data_partition_is_a_named_state_and_nothing_is_written`, and for the `info` keys
`...::the_grow_too_large_info_keys_name_the_state_the_sizes_and_the_reason`.

**Measured in QEMU (NVMe-backed, not on a board).** The check is `testing/qemu/run-grow-live.py --table mbr|gpt`
with the guest init `testing/crates/grow-live-init`. Each case copies partition 2 out of the image and passes only
with `e2fsck -fy` exit 0 after the journal replay, the full partition as the block count, and no baseline record.

| Case | Table | Disk | Result | Resize, driver clock | Probe: calls, max latency, max gap | e2fsck -fn / -fy | Blocks |
|---|---|---|---|---|---|---|---|
| refuse | GPT | 2 TiB | `grow-too-large` at 0.38 s, nothing written | none | 5, 1.6 ms, 101.5 ms | 0 / 0 | 9728 (unchanged) |
| full | GPT | 256 GiB | finished | 0.38 s to 3.43 s (3.05 s) | 33, 9.6 ms, 112.9 ms | 0 / 0 | 67087360 |
| reset at 0.2 s | GPT | 256 GiB | two boots, finished on the second | cut at 0.22 s | 33, 4.6 ms, 109.9 ms | 0 / 0 | 67087360 |
| reset at 2.0 s | GPT | 256 GiB | two boots, finished on the second | cut at 2.04 s | not in the summary (see the console log) | 0 / 0 | 67087360 |
| full | GPT | 64 GiB | finished | 2.43 s to 4.67 s (2.24 s) | 27, 2.4 ms, 102.2 ms | 0 / 0 | 16755712 |
| full | MBR | 256 GiB | finished | 4.51 s to 7.21 s (2.7 s) | not recorded in this run | 0 / 0 | 67087616 |
| reset at 2.0 s | MBR | 256 GiB | two boots, finished on the second | second boot 0.30 s to 2.36 s | 23, 2.7 ms, 106.0 ms | 0 / 0 | 67087616 |

Each run is one sample on a shared host, so the times vary (the MBR 256 GiB run took 1.8 s in GROW2 and 2.7 s
here). In the 64 GiB GPT run the slots device's first bind timed out at start ("bind did not finish in time");
it retried and the grow still finished, so that run's health reads `Failed("retrying")` at the end. The e2fsck
output for every case is kept with the run (`e2fsck-fn.txt`, `e2fsck-fy.txt`). The GPT grown end is the last
whole MiB before the disk's usable end, so its block count is one MiB shorter than the MBR's (67087360 against
67087616 at 256 GiB).

**Finding for the kernel (parked, not fixed here).** A reset in the middle of an online resize at 2 TiB, on the
pinned kernel 7.3.3, leaves a journal that replays into an inconsistent filesystem: after replay `e2fsck -fy`
finds "Last group block bitmap uninitialized" and wrong free block and inode counts, although `e2fsck -fn`
(which does not replay) calls it clean. On the next resize the kernel hits `kernel BUG at fs/ext4/resize.c:546`
(a `BUG_ON` in `setup_new_flex_group_blocks`: `group_data[0].group != s_groups_count`), on the calling thread,
which never returns. This is why the cap stays at 256 GiB. Three mid-resize resets at 256 GiB came back clean;
that is evidence, not a proof. The evidence is kept with GROW2 (`tmp/GROW2/evidence`): the console log, the
replayed `e2fsck` output and the kernel's source line.

**Not proved here.** The card or eMMC writes one sector whole on a power cut, and the write order's safety
depends on it. The board check is `testing/board-runs/GROW.md`, untested on board.

### The slot layout (Pi firmware form)

`contracts/slot-layout.toml` (version 6) lists every file of a slot's bay set (`slots/bayN/`) on the
boot partition, with its role, the forms that store it, whether it is required, and its `source`: the
kernel (`kernel8.img`), the initramfs (`initramfs.cpio`), the device tree the slot's kernel built
(`board.dtb`, stored under each of the board's default names: its entry says `named_by =
"device_tree_names"`) and `cmdline.txt` are `build` (the image builder supplies them), and
`overlays/lemnos-board.dtbo` is `build` too (the compiled board overlay, see [the board overlay
file](boards.md#the-board-overlay-file)). `overlays/README` is `generated`: it makes `os_prefix` apply to
overlays, and `lemnos-ctl boot-partition` writes it into every Pi-form bay set, one fixed line.
`firmware-overlays.txt` at the root is `generated` too: the member list (the form, the device tree's
names, the config digest, each firmware overlay and file), which the driver reads to know a set's files.
Its `[bay]` table states the region's alignment (`region_align = 256`) and the name of the file that
holds the rest of a region (`reserve = "reserve"`).
The generated `config.txt` and `tryboot.txt` name the initramfs and the board overlay with the same
spelling, and the constants in `drivers/slot-store/src/layout.rs` are tested against the file.
`boot-partition` writes a copy as `slot-layout.toml` and checks it against the code before it builds an
image. The reader refuses any version but 6 (5 and 7 included), any unknown key, and a `source` other
than `build` or `generated`. What version 6 changed from 5 is stated in the file's header.

A slot's manifest may carry `boot.overlay` (`{"board": ..., "sha256": ...}`): the board's overlay
digest, which the live marker must equal (`docs/boards.md`). `lemnos-ctl tree BOARD --manifest-boot FILE`
writes that object for the board, as the whole `boot` object the generator is the authority for
(`docs/boards.md`, "The board overlay file"). The installer and the image builder
check that a `rpi-tryboot` slot has every required file (`layout::rpi_tryboot_missing`).

## The watchdog chain

A slot board is watched by two things, and they catch different hangs. The hardware watchdog resets
the board when nothing feeds it for its period. The good-boot deadline (below) restarts the board when a
boot runs but never becomes good. The boot counters decide which slot the next boot starts. This section
writes the window of the watchdog from the first power-up to steady running, who feeds in each, and what
bounds it. The numbers are the contract's: `contracts/slot-kernel.toml`, `[watchdog]`.

Exactly two programs open the device, in this order: `lemnos-slot-init` (the initramfs `/init`, the
first opener, once it has mounted `/proc`, `/sys` and `/devtmpfs`), and then lemnosd's `hw-watchdog`
driver (`drivers/hw-watchdog`, at its bind). Nothing else opens it. An image that gives the device to
systemd's `RuntimeWatchdogSec=` or `ShutdownWatchdogSec=`, or to a BusyBox `watchdog` daemon, adds a third
opener that feeds it even when lemnosd is dead. That hides the hang this chain exists to catch
(`packaging/README.md`, "The hardware watchdog"; `scripts/check-packaging.sh` refuses a unit that sets
`WatchdogSec=` or the systemd watchdog keys; the M37 board record's probe found systemd feeding the device).
slot-init holds the device from its open until its exec, and lemnosd takes it over at its bind. The windows,
in the order a boot passes them:

| # | Window | Who feeds in it | What bounds it | A hang in it |
| --- | --- | --- | --- | --- |
| 1 | Firmware or U-Boot arms the watchdog, before the kernel | the firmware: the device runs its own period, nothing feeds | the firmware's period (the board's boot setting; U-Boot's `CONFIG_WATCHDOG_TIMEOUT_MSECS` when the board sets it) | reset at that period; the firmware counts the restart |
| 2 | Kernel probe to the first open | the kernel, with `CONFIG_WATCHDOG_HANDLE_BOOT_ENABLED` | `CONFIG_WATCHDOG_OPEN_TIMEOUT`, 30 s from the driver's probe. After it the kernel stops feeding | reset at the device's period after the last kernel feed |
| 3 | Slot-init from its open to `switch_root` | slot-init, at each log line and each wait (`service/slot-init/src/sys.rs`) | the applied period, read back from the device: 15 s asked, at most 30 s allowed (`service/slot-init/src/dog.rs`). A feed every min(5 s, half the applied period) | reset at the applied period after the last feed; the boot counters decide the next slot |
| 4 | Unfed hand-over: from slot-init's last feed before the exec to lemnosd's root-phase feed | nobody. The exec closes slot-init's descriptor without the magic close, so the device keeps running with no feeder. The gap holds systemd's start up to lemnosd's exec and lemnosd's root phase | the applied period. The gap must be well under it (below) | reset; the restart starts the default or the previous slot |
| 5 | lemnosd's root-phase feed to its first healthy tick | nobody. The root phase opens the keep-alive watchdog as root and feeds it once; the host then binds the board (the watchdog's bind takes the root phase's descriptor) and the first tick waits until the daemon is healthy | the same applied period, from the root-phase feed; the daemon's healthy test (`service/lemnosd/src/liveness.rs`) | reset at the applied period after the root-phase feed, if lemnosd never becomes healthy |
| 6 | Steady running | lemnosd, at half its timeout, only while it is healthy | `timeout_s` on the `hw-watchdog` device (15 s by default, read back) | a daemon that stops being healthy stops feeding: reset at `timeout_s` |

The first-flash chain (below) has the same windows 1, 2, 5 and 6, and an unknown window 3.

**Window 2, and why the open timeout is 30 s.** The kernel's feeding runs from the driver's probe to the
first open, so the 30 s has to cover probe to slot-init's open. slot-init opens early: it does that right
after it mounts `/proc`, `/sys` and `/devtmpfs`, before it reads the boot partition or the data partition. So
the only work the 30 s covers is the start of the initramfs. The slowest such start is not known: the tree
gives no number for it. The first boot on each board measures probe to open (`testing/board-runs/WDT-CHAIN.md`).
A start longer than 30 s resets the board before slot-init arms it: the reset is counted like any early
hang, so a board that always starts slower than 30 s rolls back and never confirms a slot.

**Window 3, and the bound.** slot-init asks for 15 s. Hardware rounds that: the DesignWare watchdog of
the rk35xx family applies the first power-of-two period at or above the request, which is below 30 s
on any clock (`service/slot-init/src/dog.rs` derives it), and the sun60i family's watchdog stops at 16 s.
A device applying more than 30 s, or 0, is refused by name (`watchdog-period-outside-bound`,
`watchdog-period-zero`) and is still fed, at the same cadence. slot-init names the refusal in the kernel
log. `lemnos-ctl validate` refuses a board whose own declared period (the `hw-watchdog` device's `timeout_s`)
is outside 1 to 30 s, under the same name. A device that cannot be read at all is named
`watchdog-period-unreadable`. If the device node never appears, slot-init retries the open with a backoff
that starts at 250 ms, doubles and is capped at 8 s, until the waits reach 20 s; then it logs
`watchdog-node-absent` and arms nothing. The kernel's feeding of window 2 is then all there is until
lemnosd binds the device, and the open timeout bounds it.

**What slot-init does with a device it refuses.** It keeps the open descriptor and feeds it while
boot runs, at half the applied period, or every 5 s when the period is 0 or unknown. The device is armed
whether slot-init likes its period or not, so a dropped descriptor would leave it running unfed. The one
hand-over that closes the descriptor is the exec of `switch_root`, and it comes after a feed right before
it. Feeding stops, as before, when boot returns (`service/slot-init/src/main.rs`): a stuck final loop
must reset the board, so this is the one place a device is left to its period on purpose.

**Window 4, and the bound.** The gap starts at slot-init's last feed before the exec and ends at lemnosd's
root-phase feed. The bound is one third of the applied period: 5 s when the period is 15 s. The reason: the gap
is time nothing feeds, so it has to be well under the period, and the third leaves the rest of the period
for the host's own delays (the stamps, a slow disk, a busy kernel). The bound is a named constant in the
QEMU check (`testing/qemu/run-watchdog.sh`, `HANDOVER_GAP_DIVISOR`), which reads the applied period the
guest reports.

The root phase holds the watchdog (`LinuxBoard::hold_before_drop`, `backends/linux/src/watchdog.rs`): for every
keep-alive device, it opens the node its `uses` names as root, close-on-exec, writes one feed, and keeps the
descriptor for the daemon's life. Each bind of the device takes a copy of that descriptor instead of opening the
node by name. So the node's owner does not matter to the watchdog: devtmpfs makes `/dev/watchdogN` root 0600
at the driver's probe, and the udev rule that gives it to group `lemnos` (`packaging/udev/70-lemnos.rules`) is
off the watchdog's path. Without the hold, a rule that lands late cost its delay rounded up to the next bind
retry (1 s, 2 s, 4 s, capped at 8 s). A keep-alive device named by its `device` setting instead of `uses` is not
held: its bind opens the node by name.

A restart does not open the node. Opening a running watchdog feeds it, so a lemnosd that died after its root
phase, before it was healthy, would be fed by each restart's open, for good, and the watchdog would never catch
it. So the root phase keeps the descriptor it fed in the service manager's file descriptor store
(`FDSTORE=1`, `FDNAME=lemnos-watchdog`, `service/lemnosd/src/privilege.rs`, `keep_in_store`), and the next start
takes it back from `LISTEN_FDS` and feeds nothing (`lemnos_linux_sys::listen::take`,
`LinuxBoard::hold_before_drop`). The root phase logs `adopted, not fed` for that start. The store is the only
marker of a restart: the runtime directory no longer tells a restart from a first start (the unit keeps no
`RuntimeDirectoryPreserve=`), and nothing else does.

The store's rules, each tested on the lifecycle the service manager gives it:

- **A crash and a restart** keep the descriptor. The restart adopts it; a restart that dies before healthy is fed
  by nothing, and the watchdog resets the board one period after the last feed (QEMU `daemon-loop-bind`).
- **A failed unit** drops the store, as systemd does for a unit that ends in `failed`. Nothing feeds the watchdog
  after that, and the board resets one period after the last feed. The next start of the board is a boot's first
  start, and the reset is what the operator sees.
- **A stop, then a start** (an operator's `systemctl restart`, or a stop and start of the unit) empties the store
  (`FileDescriptorStorePreserve=` is not set). The start after it is a first start: the root phase opens the node and
  feeds it once, and logs `fed`. That feed is the deliberate restart's, the same as a boot's first start.
- **A `daemon-reload` or a `daemon-reexec`** keeps the store: the running unit is not restarted, and systemd serialises
  the store across a re-execution of itself.
- **A descriptor that is not the node's** (not a character device with the watchdog node's device number, as a
  store that was filled for another node or another boot would give) is refused by name, closed, and the node is
  opened and fed as a first start does. A stored descriptor that cannot be kept (no manager, or a manager that
  refuses it) is named, and the node is then opened by name at the next restart, which feeds it.
- **BusyBox** has no store. Its restart opens the node by name, as before: the feed on a restart is the documented
  limit of that init, which `packaging/README.md` says. The first start of a BusyBox board holds and feeds the same.

The QEMU check runs the real lemnosd under its unit (`lemnos-unit-check exec`, as 999:999) behind a hook that
arms the watchdog as slot-init does, with `LEMNOSD_START_TRACE=1`, and reads the pieces from the hook's last feed
(measured on KVM; TCG in parentheses):

| Piece | `daemon` (node 0660 to group) | `daemon-root` (node root 0600) |
| --- | --- | --- |
| lemnosd's exec | 0.041 s (0.848 s) | 0.084 s |
| the root-phase feed: the unfed gap ends | 0.045 s | 0.095 s |
| the drop | 0.047 s (1.217 s) | 0.099 s |
| the watchdog's bind begins, one bind before it | 0.051 s (1.458 s) | 0.113 s |
| the first tick | 0.064 s (1.723 s) | 0.164 s |

The TCG column is from before the hold (the gap then ended at the first tick). The part before lemnosd's exec on a
board, switch_root through systemd to the exec, is the image's: no test here runs it, and the board run measures it
(`testing/board-runs/WDT-CHAIN.md`). The start trace splits lemnosd's own part on a board: set
`LEMNOSD_START_TRACE=1` in `/etc/lemnos/lemnosd.env`, and lemnosd writes `lemnosd: start trace: <CLOCK_MONOTONIC s>
<event>` for its exec, the root phase's feed, the end of the root phase, each bind as it begins and ends, and each
device's first tick.

The QEMU check reads the start trace of a lemnosd it has stopped at a known point, so the check never races
lemnosd: `LEMNOSD_STOP_AFTER=root-phase` makes lemnosd stop itself (SIGSTOP to its own process) right after the
trace line `root phase done`, before any device is bound, and `LEMNOSD_STOP_AFTER=bind:<device id>` does the same
right after that device's `bind <id> ok` or `bind <id> failed` line. The check waits for the stop, then reads the
stopped process's stderr: every line up to the stop point, and nothing after it (a line after the stop point fails
the check). It is honoured only with `LEMNOSD_START_TRACE` set, and is a test seam: no unit sets it.

**Window 5.** A keep-alive device is not ticked while the daemon is not healthy (`engine/host/src/host.rs`,
`may_tick`). Healthy means the daemon's main loop turns and its socket answers; neither starts before the host has
made or cut every bind of the board. So the first tick comes at the first healthy turn after the start, and the
root-phase feed is what covers the start: one applied period from it. There is no feeder in between on purpose. A
feeder that ran while the daemon is not healthy would feed a lemnosd that hangs, which is the hang the watchdog is
for. A bind that does not return is cut at the host's call deadline (2 s by default, `call_deadline_ms` in the
board), and the next bind begins. The QEMU check proves both ends: a bind hung ahead of the watchdog is cut and the
watchdog is fed after it (`daemon-hang`: the watchdog's bind began 2.46 s and its first tick 2.55 s after the hook's
last feed), a lemnosd stopped right after its root-phase feed is reset one period later (`daemon-stop`: 15.1 s),
and a lemnosd killed at the end of its root phase on every start and restarted is fed only by the first start
and reset one period after it (`daemon-loop`: 14 starts, one fed, reset 15.7 s after that feed). The last case
is `daemon-loop-bind`: the node is owned by the daemon's group, so a restart's bind can open it by name, and each
start is killed after its watchdog's bind has ended, before it is healthy. Each restart takes the stored descriptor
and feeds nothing, so the guest is reset one period after the first start's feed. On a tree whose restart opens the
node by name, that case never resets (PING-ON-OPEN).

**Which mechanism catches which hang.**

| Hang | Caught by | Then |
| --- | --- | --- |
| In the firmware or U-Boot, before the kernel | window 1 (the firmware's period) | reset; the firmware's count goes up |
| In the kernel, before the first open, past 30 s | window 2 (the open timeout) | reset; the count goes up |
| In slot-init, after its open | window 3 (the applied period) | reset; the count goes up |
| In a slot-init loop after boot returned | window 3 (feeding stopped on purpose) | reset at the applied period |
| In lemnosd after the hand-over, or a daemon that is not healthy | window 6 (`timeout_s`) | reset; the count goes up |
| A boot that runs with lemnosd healthy but never confirms good (the marker is missing, the slot never reaches ready) | the good-boot deadline (`good_boot_s`, default 300 s, from 30 to 3600), which is not the watchdog: the daemon keeps feeding while it is healthy | `reboot-force` at the deadline |
| Restarts that keep failing, whatever the cause | the boot counters: the Pi firmware's count (previous at 4 to 7, RPIBOOT at 8 on the rescue), and the U-Boot restart count | the previous slot, then the flashing mode |
| A trial that fails | the tryboot record and its attempts (`trial_attempts`) | the default, by a plain restart |

The watchdog does not see a boot that is alive and wrong. That is the deadline's job, and the counters
decide what the next boot starts. Neither the watchdog nor the deadline changes a slot.

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

1. It mounts `/proc`, `/sys` and `/dev`, then arms the hardware watchdog (window 3 of "The watchdog
   chain"), and reads the slot arguments from the kernel command line. The prefix is whatever stands
   before `.slot=` (the driver's `boot_args`).
   The line is written by `lemnos-ctl tree BOARD --slot ID --cmdline-slot FILE` (`docs/boards.md`).
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
7. It checks that the root can start lemnosd, and refuses the slot by name when it can't ("What a slot's root
   must contain"). It moves `/data` into the new root and writes `board/slots/booted`, plus `trial-started` on a
   trial. It binds the machine id over the new root's `/etc/machine-id` (see "The machine id"; a refusal is
   logged and never stops the boot). Then it feeds the watchdog once more and `switch_root`s into `/sbin/init` (window 4 of "The
   watchdog chain").

### What a slot's root must contain

An image builder needs this list. slot-init and the `system-slots` driver rely on these paths and on
nothing else in the root:

| Path in the slot's root | Kind | Who uses it | When it is missing |
|---|---|---|---|
| `/sbin/init` | executable | slot-init `switch_root`s into it (`boot::INIT`) | `switch_root` fails and the boot goes to the rescue path ("When the slot can't come up") |
| `/data` | empty directory | slot-init moves the data partition onto it. The path is fixed: slot-init reads no board file, so on a slot board the `system-slots` setting `data` must be `/data` (its default) | the slot is refused: "can't move the data partition into the slot (no /data in the image?)" |
| `/usr/bin/lemnosd` | executable file | the unit and the init script start it | the slot is refused (below) |
| `lemnosd.service` enabled | a `*.wants/lemnosd.service` link in `/etc/systemd/system` or `/usr/lib/systemd/system` to the unit file, and the unit not masked; or, under BusyBox init, an executable `/etc/init.d/S50lemnosd` | lemnosd starts at boot | the slot is refused (below) |
| user `lemnos` in `/etc/passwd`, group `lemnos` in `/etc/group` | entries | lemnosd starts as root and drops to them (`--user lemnos --group lemnos` in both units) | the slot is refused (below) |
| `/etc/machine-id` | plain file (empty is fine) | slot-init binds the derived id over it ("The machine id") | the refusal is logged and the boot goes on, with systemd taking a random id |

The root image is read-only (EROFS behind dm-verity), so slot-init creates none of these.

**Why lemnosd's pieces are checked.** slot-init arms the hardware watchdog, and after its exec
lemnosd's `hw-watchdog` driver is the watchdog's only feeder ("The watchdog chain"). A root that can't
start lemnosd therefore always ends in a watchdog reset some seconds later, with nothing in the log
saying why. So before `switch_root` slot-init checks the three rows above (`service/slot-init/src/root_check.rs`)
and, when one fails, refuses the slot like any other bad slot. The refusal names every missing piece:
"the slot's root can't start lemnosd, which feeds the watchdog after this: ...". A trial then ends and the
board starts the default. Links in the root are followed inside the root (`/usr/...` is the root's own). The
user and group names are part of the contract. A drop-in that starts lemnosd as another user isn't
supported, and a test ties the names to `packaging/`. There is no opt-out: lemnosd is the watchdog's only
owner after slot-init, by design.

**The boot partition at run time.** Neither slot-init nor lemnosd needs the boot partition mounted on the
`tryboot` form. Confirming a trial, arming one, and `previous` are raw sector writes on the partition's
block node. lemnosd finds that node from the boot source and `boot_sources` ("Pointer files, and the
run-time rule for the boot partition"). The image must carry the pre-created pointer files, records and
bays. On the `tryboot` form the driver's `boot` setting is read by nothing. Only two forms use the
mount at that path: `file`, whose `boot-record` file is there, and `ubi`, whose fallback `extlinux` copy
is there. The `uboot` form writes U-Boot's environment on the boot disk and doesn't use the mount either.
slot-init mounts the boot partition read-only only where a form needs to read a file there, and unmounts
it before `switch_root`. A system that mounts `/boot/firmware` for its own use may do so; Lemnos doesn't
need it.

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

- **Default boot, nothing else pending:** the boot keeps the kernel log (`board/slots/last-failure.log` on
  the data partition when it is mounted writable; the boot partition is never written), prints `no slot can start: the kernel lacks CONFIG_...` and waits 30 s, then
  restarts. That is the last resort the boot had before, when no slot can be opened, with the
  reason named. It starts no slot and switches no root, so the board stays here until the kernel
  is fixed.
- **Trial boot** (the firmware's tryboot flag, a `file` record's trial with attempts, or a
  U-Boot or UBI record's trial): the attempt the boot that started the trial took is given back
  (`tryboot` counter and the U-Boot or UBI record count, up to the board's `trial_attempts`), the
  reason is written to `reason.rec` on the boot partition, and the board restarts plainly.
  - A `tryboot` or `file` trial stays in its record, held: the default boot takes no attempt for
    it while `reason.rec` holds a kernel-gap reason, and the default slot starts. The daemon
    reads `reason.rec` when it resolves the trial, so `list` shows the kernel option, not a bare
    mount error. A trial whose kernel was rebuilt is resolved by the daemon, which clears the
    reason.
  - A U-Boot or UBI record's trial is withdrawn (its trial cleared), because the loader takes an
    attempt on every trial start and can't be told to wait.

### A root that cannot be mounted

When dm-verity accepts the image but the kernel cannot mount it as EROFS (the usual cause is a kernel
without the decompressor for the algorithm the image was built with, `contracts/slot-kernel.toml`), slot-init
treats it as a slot failure like any other unreadable root: `FAILED=yes`, `FAILURES` one higher, and
`FAIL_REASON` set to `can't mount the root image <device> as erofs, read-only: <the kernel's error>`, where
the error is the kernel's own text with its number, for example `Invalid argument (os error 22)` or
`Operation not supported (os error 95)`. The normal fallback follows (the previous slot, or a plain restart
into the default for a trial). A test pins the text and the fallback
(`an_image_that_wont_mount_falls_back`).

### When the slot can't come up

The failure is kept and recorded before anything else happens:

- The kernel log goes to `board/slots/last-failure.log` on the data partition, when it is mounted
  writable. Nothing is written to the boot partition.
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
   - The trial goes into the record the boot partition holds: on the Pi firmware form, the trial
     pointer and the trial record, raw (see "Pointer files"), then a `0 tryboot` restart; on raw
     NAND, the `boot-record` volume with `trial_attempts=`. On the `uboot` form the trial is in the
     U-Boot environment (see "The `uboot` record"). The `boot-record` file and `boot-record.env` of
     the other forms are not written at run time, so their trial is refused by name. The attempts are the board's
     `trial_attempts`, read from `board/slots/settings.env`.
   - When the default's own boot files don't match its manifest (Pi firmware form, and the previous
     slot's files match), the default pointer is moved to the previous by one raw write, the
     default is marked failed, and the board restarts into the previous. If the previous doesn't
     match either, nothing is moved: the board reports both and waits.
   - The board's state gets `TRIAL` set to that slot, so a `confirm` from it makes it the default:
     the board has rolled back.
4. **With no candidate**, such as a board with one slot, the board waits, then restarts into the
   same default.

It never writes the default. The worst it does is start one other slot once, on trial, so a plain
restart always returns to the default. With the data partition read-only, nothing is recorded on
it, but the trial is still written to the boot partition.

**Untested on board.** The tests run the whole flow over a temporary directory that stands in
for `/`, with a fake for every system call. `testing/board-runs/I5d.md` is the board run.

## The machine id

A slot's root is read-only, so systemd on it cannot write `/etc/machine-id` and takes a random id on every
boot. The persistent journal lives under that id, so on `/data` it would scatter over a new directory each boot.
slot-init gives every boot of a slot the same id, before `switch_root`.

**Derivation.** The id is the first 16 bytes of SHA-256 over the bytes `lemnos-machine-id:` followed by the
board serial, in the normalised form the driver reads (`docs/identity.md`). It is written as 32 lowercase hex
digits and a newline. An all-zero id has its last bit flipped, because systemd reads all zeros as no id. The
same serial gives the same id on every boot, and the known vector is pinned by a test
(`machine_id::tests::the_known_vector_is_the_first_sixteen_bytes_of_the_digest`).

**Where the serial comes from.** The slot's kernel command line carries the identity device's serial settings,
never the serial itself: `<boot_args>.serial.kind=<kind>`, then `path`, `offset`, `length`, `byte_order` and
`key` as the board file names them (`lemnos-ctl tree BOARD --slot ID --cmdline-slot FILE` writes them, after
the `.src.` words; `docs/boards.md`). slot-init reads them with the parse the driver makes
(`lemnos_identity::settings_from_words`) and the same reader (`lemnos_identity::read_serial`). A board with no
`system-identity` device that names a `serial` writes none. The line is part of the signed bundle, so the
serial source is the bundle's own.

**What slot-init does.** Once the data partition is in the new root, and before `switch_root`:

1. It needs `<newroot>/etc/machine-id` to be a plain file. It creates nothing there.
2. It writes the id to `/run/lemnos/machine-id` in the initramfs, mode 0444.
3. It bind-mounts that file read-only over `<newroot>/etc/machine-id`. The bind keeps the staged file alive
   after `switch_root`.

**Refusals.** A refusal never blocks the boot. Its line goes to the kernel log, in the form
`slot-init: MACHINE-ID REFUSED (<reason>): <detail>`, and the boot goes on with no id. journald keeps a transient
id that boot. The line stays in the kernel log, which journald imports, so `journalctl -k` shows it after the
boot. The reasons:

| Reason | When |
|---|---|
| `machine-id-serial-absent` | The command line names no serial source, or the source cannot be read or does not fit the format. |
| `machine-id-target-missing` | `<newroot>/etc/machine-id` is absent or not a plain file. Nothing is created. |
| `machine-id-bind-failed` | Staging the id, the bind mount or its read-only remount failed. A failed bind leaves nothing mounted and no staged file. |

**The image contract.** The root ships an EMPTY `/etc/machine-id` (zero bytes, a plain file). That file is the
bind target. An unbound boot therefore has no id, and the driver answers it as absent (below). A root with no
such file is refused by name.

**Who reads it.** The `system.identity/1` contract's `machine_id` method answers the bound id (`docs/wire.md`).
It reads `/etc/machine-id`, not `/run/lemnos/machine-id`: the staging path belongs to the initramfs, and
`switch_root` takes it out of view. The method answers `NOT_FOUND` when the file is absent, empty or not an id.
It also checks the id against the board's serial: the driver recomputes the derivation from its own serial with the
same code slot-init uses. An id that does not match (a bind that failed, so the file holds systemd's transient id) is
refused the same way, and the device is `Degraded` with `transient: slot-init did not bind (see machine-id-* in the
kernel log)`. With no serial source configured there is nothing to check against: `machine_id` answers `NOT_FOUND`
and the device stays `Ok`. An absent or empty file (a boot that is not a slot's, or a refused bind with its logged
reason) is also a normal state: `NOT_FOUND`, and `Ok`.
It is a method, not a fact: the bridge's `call` policy admits it to local programs and operators only, and
`hello.*` never carries it.

**Consumers.** The id is derivable from the serial. That is acceptable because the serial is not secret on these
boards. Consumers use app-specific ids (`sd_id128_get_machine_app_specific`, or their own derivation from the
id) and never expose the raw machine id: what leaves the board is always the app-specific value.

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
  so. Nothing on a tick waits on another process.

  `tcp:<address>:<port>` is the same rule for a TCP port: `tcp:10.55.0.1:22`, or with an IPv6
  address in brackets, `tcp:[fe80::1%usb0]:22`. The address is an IPv4 address or a bracketed
  IPv6 address, never a name (nothing here looks a name up), and the port is 1 to 65535. A
  link-local IPv6 address (`fe80::/10`) carries its interface's name after a `%`, and only such
  an address does. An IPv4 address takes no interface: the board's routes pick it, and an
  address of the board itself is delivered on the board whichever interface holds it. Each look
  makes one connect that does not wait, sends no byte and closes the connection as soon as it is
  taken. A port on an address of the board answers at once, taken or refused. Every failure is
  that look's answer only, and the next look connects again: refused, no route, an address the
  board does not have yet, a zone whose interface does not exist yet (it is looked up at each
  look, so an interface that appears after the daemon starts is found then). The reason names
  the source and the kernel's sentence (`health source tcp:10.55.0.1:22 doesn't accept a
  connection: Connection refused (os error 111)`). A connect with no answer (the address is not
  on the board, so the connect went out on a link) is busy; it is kept and asked again at the
  next look, given up 2 s after it began (the look after that makes a new one), and a source
  busy for 2 s is not good for good, as a socket is. A good `tcp:` source proves that the port
  takes a connection made on the board itself. It does not prove the route from another machine:
  not the cable, the other side's address or a filter between them. Under the packaged systemd
  unit the daemon may open no TCP socket (`RestrictAddressFamilies=AF_UNIX AF_NETLINK`), and
  every look says `Address family not supported by protocol`: a board with a `tcp:` source
  installs the drop-in `lemnosd.service.d/health-tcp.conf` (`packaging/README.md`);
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
last try whatever happened. The reason is also written to `reason.rec` on the boot partition, beside
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

**The extra pass on tryboot.** The firmware keeps no count of a trial's attempts (its `boot_count`
counts every boot, and is what the fallback to `previous.txt` reads), so on the `tryboot` form every
trial boot runs the default slot's kernel and initramfs first: the initramfs takes the attempt, clears
the firmware's count (the restart is Lemnos's own), and restarts with `0 tryboot`. That is one extra firmware, kernel and initramfs pass on each trial boot.
It is the price of the guarantee that a trial kernel which can never boot always falls back: the
attempt is taken before the trial's kernel runs, so a power cut cannot leave the trial without
its count. `testing/board-runs/I5f.md` measures it. **Untested on board.**

## Boot history

The history is `lemnos/boots`: one line per boot, kept with the slot records in
`board/slots/boots` on the data partition. When the state directory can't be written, the same
history is kept on the boot partition as `boot-history`, beside `reason.rec`; the reader takes
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

## The crash log

A board that declares `crash_log` (the pstore mount, `/sys/fs/pstore`, [Boards](boards.md#boot-history-system-slots-system-reboot))
keeps the previous boot's crash record. The kernel writes it to pstore while it panics, into a region of memory
the board reserves (ramoops, [Boards: crash-log regions](boards.md#crash-log-regions)). The record survives the
reset that follows, so the next boot can read it.

Each boot, once, at its start, `system-slots` does this in order:

1. It reads the records in the pstore directory before anything is written. The record it stores is the newest
   `dmesg-*` record (else the newest `console-*`).
2. It appends the boot's history entry (`lemnos/boots`, unchanged: seven fields).
3. It writes the record's text to `lemnos/crash/<n>`, where `<n>` is the boot's number, and its index row to
   `lemnos/crashes`. Both are written by replacing the file and fsyncing it. The text is the record's tail, at
   most `crash_bytes` bytes (8192 by default, at most 65536). When bytes were cut, the text's first line says so:
   `cut: N bytes before this not kept`.
4. Only then it removes the records it read. A failure before this step keeps every record on pstore.

The index row is `<n> <boot> <record> <size> <mtime> <kept> stored`: the boot number and the boot id of the boot
that follows the crash, the record's name, size and modification time (its key), the bytes of tail kept, and the
word `stored`. The crash is linked to its boot by both the number and the id. A record already in the index (an
earlier run stored it and could not clear pstore) is cleared without a second copy. The index and the texts keep
the boots the history keeps (`boots_keep`); the rest are removed.

The driver never mounts pstore. Where the mount is missing, the boot goes on and `info` says so in words:
`crash log: /sys/fs/pstore is not mounted`. `info` carries a `crash_log` field for a board that declares the log,
with what the record step did (stored, kept, not cleared, no record). A board with no `crash_log` is not changed
at all: no file, no index row, and no field in `info`.

The kernel must build `CONFIG_PSTORE`, `CONFIG_PSTORE_RAM` and `CONFIG_PSTORE_CONSOLE` in (`docs/kernel-config.md`,
"Crash log: built in"). The QEMU proof is `testing/qemu/run-crash.sh`: a guest panics with sysrq-c, reboots,
and the next boot stores the record for its own boot number and clears pstore. Untested on board.

## The boot source

Everything the boot chain reads or writes on a board is on the boot disk, the one whole disk the
board booted from, and nothing else. A second disk with the same partition labels is never used.
The boot chain states where it booted from; the board file says which of those sources it has, and
where each one's storage controller is in the kernel's device tree. The disk is the one whole disk
under that controller.

Not every block device without a partition file is a disk the boot can be on. Each of these is
set aside by a fact the kernel gives, and the refusals below name each one set aside and why
(`disk::set_aside`):

- **An eMMC's hardware areas.** The kernel's MMC block driver gives the card's boot areas, its
  general-purpose areas and RPMB block devices of their own, `mmcblk<host><area>` (`boot0`,
  `boot1`, `gp0` to `gp3`, `rpmb`), registered as children of the user area `mmcblk<host>`: in sysfs
  each sits inside the user area's directory, as a partition does, but with no partition file. Same
  card, same controller. One is set aside when it has an area's name *and* its sysfs parent is that
  user area's block device. A real disk can't be set aside this way: a disk's parent is a controller
  or a card, never another disk, and no other driver uses the `mmcblk` name. Counted as disks, the
  reference board's eMMC (`boot0` and `boot1`, 4 MiB each) showed three disks under its controller and
  every boot was refused, with nothing written.
- **Hidden** (`hidden` is 1): a path the kernel keeps behind another disk (an NVMe multipath
  controller's own view of a namespace).
- **No medium** (`size` is 0): a card-reader slot with no card, or a removable drive with no medium.
  A medium that arrives later is seen then, within the wait.

Loop, RAM and device-mapper devices sit under `devices/virtual` with no controller, so they never
match one. Two real disks under one controller (two USB disks behind one host controller, two NVMe
namespaces) are refused: the boot doesn't guess between them. An SD card and an eMMC are on separate
hosts, so separate controllers. The test fixture is the reference board's own `/sys/class/block`
shape; QEMU's `emmc` device with boot areas shows the same refusal before and the boot disk after.

The board file's `boot_sources` lists the allowed sources, one `source=kernel path` entry each:

- `rpi-boot-mode-<n>=<path>`: the Raspberry Pi firmware's `/chosen/bootloader/boot-mode`. Code 1
  is the SD card or the eMMC (measured on the reference board, where the eMMC is code 1). Other codes are from
  the firmware documentation and are not verified on a board.
- `<U-Boot path>=<kernel path>`: U-Boot's own device-tree path of the controller it booted from,
  which the boot script names in `/chosen/lemnos,boot-source`. The path is U-Boot's, the kernel path
  is the kernel's (they differ where the two trees name the node differently).

Examples. A board with one medium (the reference board's eMMC):

```toml
boot_sources = ["rpi-boot-mode-1=/soc@107c000000/mmc@fff000"]
```

A board with two media, the SD card and the eMMC of a RK3588 U-Boot board (the kernel paths from the
kernel's device tree; U-Boot's paths are the same nodes, to be confirmed on the board):

```toml
boot_sources = ["/mmc@fe2c0000=/mmc@fe2c0000", "/mmc@fe2e0000=/mmc@fe2e0000"]
```

`boot_sources` is required on every board that boots from a disk (every `record` but `ubi`), and
refused by name when missing or malformed. `boot_device` is removed: it is refused by name.

The boot chain's statement, the table and the disk are checked before anything is written. A
refusal of the board's own facts (no source stated, the source not listed, no disk or two disks
under its controller) restarts the board into rescue with nothing demoted and no slot record
changed. The reason goes to the kernel log, since the boot partition is the disk the refusal is about.

### Waiting for the boot disk

A storage controller's driver adds its disk to sysfs some time after `/init` starts (an eMMC is
initialised in the background, a USB disk after its settle time), so slot-init looks for the disk
again every 100 ms for up to `BOOT_DISK_WAIT`, 10 s, feeding the watchdog as it waits. The bound is
ten times the slowest step a healthy disk takes, the same margin as the data partition's 10 s: an
eMMC answers CMD1 within 1 s (JEDEC), an SD card's ACMD41 loop within 1 s, the kernel retries a slow
card at lower clocks, and usb-storage waits 1 s (`delay_use`) before it scans. The kernel lists a
whole disk a moment before it has read its partition table, so the same wait also covers partition 1
of that disk; a disk with no partition 1 at the bound goes on as it is (a blank disk). Only the
missing disk is waited for: a source the table does not list, or two disks under the controller, is
refused at once, since no wait changes it.

The first miss is logged (`waiting up to 10 s for the boot disk: ...`), and so is the end of the
wait (`the boot disk mmcblk0 showed up after 2900 ms`). At the bound the refusal names the
controller the table expected and every whole disk the kernel listed, each with the controller it
sits under. With no block device at all after the wait, the reason says both readings: the kernel
lacks the storage driver, or the disk is slower than `BOOT_DISK_WAIT`.

Checked in QEMU (an arm64 `virt` guest whose device tree carries the firmware's `boot-mode = <1>`,
with the boot disk a USB stick under `/pcie@10000000` held back by `usb-storage.delay_use`): with a
3 s delay the single look refused the board and went to the rescue, and the wait finds the disk
after about 3 s and goes on to the data partition; with a 12 s delay the wait ends at 10 s with the
refusal above. **Untested on board.**

### A refusal without a console

Every slot-init line goes to the kernel log through `/dev/kmsg`, prefixed `slot-init: `; the lines
that end the boot (the rescue's `no slot can start: ...`, a foreign disk's refusal) are written at
`KERN_ERR`, the rest at the default level. A refusal before the data partition is mounted can't be
kept on the disk (the disk is what is missing), so on a board with no serial console the kernel log
is the only record. Three ways to read it, cheapest first:

- **pstore (ramoops).** A kernel with `CONFIG_PSTORE`, `CONFIG_PSTORE_RAM` and
  `CONFIG_PSTORE_CONSOLE`, and a `ramoops` reserved-memory node in the device tree (or `ramoops.mem_address`
  and `ramoops.mem_size` on the command line), keeps the console in RAM across the rescue's restart.
  The next boot that reaches a root (a RAM boot over USB, or the board once it is fixed) reads it from
  `/sys/fs/pstore/console-ramoops-0`. Nothing in Lemnos is needed for this: slot-init's lines are
  already in the kernel log. It works only where the RAM keeps its contents across a warm restart;
  that is the firmware's and the board's to say, and is untested on board.
- **The kernel log of a RAM boot.** A host-side RAM boot of the same kernel reads nothing of the
  failed boot unless pstore held it; it does show whether the disk appears and how late.
- **The data partition.** Once the boot disk is found and the data partition is writable, a refusal is
  kept in `board/slots/last-failure.log` ("When the slot can't come up").

The boot's command line carries the table as `<boot_args>.src.<source>=<kernel path>` words, one per
entry, so the slot's initramfs finds the same disk. The install refuses a bundle whose command line
does not carry each of them, by name.

### The U-Boot chain (`packaging/uboot/boot.cmd`)

The U-Boot form has no `fdt` line in any label. Before each label starts, the script loads the
slot's `board.dtb` (the fixed contract name) and names the boot source in the tree it hands on:

The script names the boot source once, up front, then loops over its three choices (trial if its
attempt was taken, default, the root copy) with one block of commands:

| boot.cmd step | What it does | Source |
|---|---|---|
| `if test "${devtype}" != "mmc"; then` | a non-mmc boot device cannot be named (scsi and UFS are unsupported: U-Boot's scsi uclass has no sequence alias, so the path form cannot name them) | `packaging/uboot/boot.cmd` |
| `fdt addr -c` | selects U-Boot's control device tree as the working tree | U-Boot `cmd/fdt.c:1118` (`-c`) |
| `fdt get value lemnos_src /aliases ${devtype}${devnum}` | copies the controller's path into `lemnos_src`; the alias `mmc<n>` is the device `mmc <n>` selects (U-Boot's mmc uclass has `DM_UC_FLAG_SEQ_ALIAS`, `drivers/mmc/mmc-uclass.c:550`) | U-Boot `cmd/fdt.c:1132` |
| `if load ... ${lemnos_dtb}` | loads the choice's `board.dtb`; a failed load is reported by name and the next choice follows | `boot.cmd` |
| `fdt addr ${fdt_addr_r}`, `fdt resize 400` | selects the loaded tree and makes room in it: the argument is hex (`hextoul`), the tree shrinks to its size rounded up to 4 KiB plus 0x400 bytes | U-Boot `cmd/fdt.c:775-783`, help `cmd/fdt.c:1129` |
| `if fdt set /chosen lemnos,boot-source "${lemnos_src}"` | states the source in the loaded tree; a failure (no room) is reported and the next choice follows | `boot.cmd` |
| `sysboot ... ${lemnos_conf}` | starts the label; it returns only when it could not start | `boot.cmd` |

**The script never ends at a prompt.** A choice that cannot start is reported by name and the loop
goes on: trial, then default, then the root copy. When none starts, or the boot source cannot be named
at all (a non-mmc device, or no alias: nothing can boot safely then), the script offers the board for
flashing and tries again, for ever. The whole script is one `while`; each pass names the boot source,
runs the three choices, and ends in a 2-second sleep. The first two passes in a row that start
nothing only say so and go round (a card that answered late, a read that failed once); the third
offers download mode; after it returns the next pass tries the slots once before offering it again.
The only ways out are a slot starting (`sysboot` does not return) and a reset.

### Download mode (the end of every pass)

The offer (the third pass in a row that starts nothing) is `fastboot usb <n>` on the OTG port, `n` from `uboot_fastboot_usb` (`lemnos-ctl boot-partition` writes `boot.cmd` into the image with
`setenv lemnos_usb <n>` and, when the board names an LED, `setenv lemnos_led <label>` before the
static script; the image builder compiles that file into `boot.scr`, not `packaging/uboot/boot.cmd`
itself, unless the defaults, USB controller 0 and no LED, are the board's). What U-Boot v2026.10 does, from its sources:

| Question | Answer | Source |
|---|---|---|
| Does `fastboot usb` take a time limit? | **No.** It registers the gadget and waits. It returns when the host sends `fastboot continue` (the gadget triggers the detach), on Ctrl-C (any key with `CONFIG_CMD_FASTBOOT_ABORT_KEYED`, not set), or at once when the board's cable check says there is no cable. `fastboot reboot` resets the board. | `cmd/fastboot.c:96-117`; `drivers/fastboot/fb_command.c:75-78`; `drivers/usb/gadget/f_fastboot.c:566-567`; `drivers/fastboot/fb_common.c:205-209` |
| Is there a cable check? | `g_dnl_board_usb_cable_connected` is weak and returns `-EOPNOTSUPP` (not 0), so the wait goes on; a board that implements it with VBUS detect makes the command return at once without a cable, and the loop goes round | `drivers/usb/gadget/g_dnl.c:180-183`; `cmd/fastboot.c:96-101` |
| Does the watchdog reset a flash? | No. With `CONFIG_WATCHDOG` the gadget loop calls `schedule()`, which runs the watchdog's cyclic servicing, so the watchdog is fed for as long as the loop spins | `cmd/fastboot.c:115`; `common/cyclic.c:118-131`; `drivers/watchdog/wdt-uclass.c:138` |
| Does it catch a hung U-Boot? | Yes: a U-Boot that stops scheduling stops feeding it, and the board resets after `CONFIG_WATCHDOG_TIMEOUT_MSECS` | the same; `drivers/watchdog/Kconfig:3-30` |
| LED | `led <label> on` before the wait and `off` after it, when the board file names `uboot_led` | `cmd/led.c:121-131` |
| What the host sees | The fastboot gadget with `CONFIG_USB_GADGET_VENDOR_NUM:PRODUCT_NUM` (`uboot_usb_vendor`, `uboot_usb_product`; unset, U-Boot's SoC default stays: a placeholder the project replaces) | `drivers/usb/gadget/Kconfig:59`, `:71` |
| Flashing | `CONFIG_FASTBOOT_FLASH` with the MMC provider writes the downloaded image to the MMC device `uboot_env_mmc_index` | `drivers/fastboot/Kconfig:91-133` |

So the offer is bounded by the host, not by a timer: the command has no timeout in v2026.10 and none
is invented here. A host that flashes ends the session with `fastboot continue` (or `reboot`), and
the next pass tries the slots again. With no host attached on a board without VBUS detect, the board
waits in download mode (LED on) until a host does that. A transient failure of the slots is retried by
the two passes before download mode is reached, not while it waits. A board that must leave download mode by
itself needs a U-Boot patch (a cable check or a timed abort); `testing/board-runs/I5i7.md` lists it.
The case the loop cannot cover, accepted: a damaged U-Boot proper behind a valid first stage (SPL):
the script never runs.

U-Boot's hush has no functions, so there is no helper to call; the loop over the three choices is the
helper. `while`, `for ... in ...; do`, `elif` and `if <command>; then` are in `common/cli_hush.c:2378-2388` (the reserved-word table; it has no function keyword), and `break` and `continue` exist but are not written (`common/cli_hush.c:501-503`), so there is one `while` and no early exit from it.

The same block serves the default's and the root copy's labels (`/extlinux/board.dtb`, which the
installer copies beside the root label). The label has no `fdt` line, so U-Boot starts the working
tree the script prepared. A label with an `fdt` line is refused by name at the every-boot check.

### What is verified, and what is not

- Verified by reading the sources (U-Boot v2026.10, commit 5508406): the alias and `fdt` commands
  above, and the mmc uclass's sequence aliases. Not run on a board.
- Not verified on a board: `fdt get value` of the alias on the control tree, `fdt set` on an
  existing `/chosen` surviving to the kernel, a label with no `fdt` line booting the prepared tree.
  The board check is written in `testing/board-runs/I5i5.md`.
- The Pi firmware's boot-mode codes other than 1 are from documentation only.

## The way back on request

A caller can ask for the board's way back without a run of failed boots: `reboot_to(target)` on
`system.reboot/1` (method 2, added: the base hash does not move). It is offered only where the board file
declares the way back, `[boot] loop_return`, the same key slot-init reads; there is one way back per board.
The host gives `loop_return` to every device that serves `system.reboot/` and to no other, and a restart
device that names it in its own config is refused at load. A contract's methods are the same on every
device, so every restart device lists `reboot_to` in its description (the bridge announces it from there),
and the call says where it is not offered. In order, each refusal asking and restarting nothing:

| Answer | When |
| --- | --- |
| `NOT_SUPPORTED` | The board declares no way back, or the record has no firmware to ask (only `tryboot` has one) |
| `INVALID_ARGUMENT` | `target` is not the board's way back (`"rpiboot"` is the only one there is) |
| `NOT_PERMITTED` | The security switch is on |
| `IO` | The restart helper would not take a restart (asked first; its check writes nothing), or the firmware refused the one-shot |

Then the firmware's one-shot is set through the mailbox (`vcio`; `TAG_SET_REBOOT_ORDER`, 3: RPIBOOT), the
caller's request goes to the boot history, and the board restarts through the restart helper, as `reboot`
does. The mailbox is root's on a board (`root:root 0600`), so the restart device's root phase holds it
across the drop, as the slots device's does for the boot count. The one-shot is for the next boot only:
if the restart then fails (`IO`), it stays set, and the next restart, whoever asks for it, goes to the way
back.

### The way back with no daemon

`lemnos-way-back rpiboot [--device NODE]` (`service/way-back`, shipped) sends the same one-shot from anywhere,
an initramfs `/init` before lemnosd exists included: RPIBOOT for the next boot through the mailbox (`/dev/vcio` by
default, which devtmpfs makes; `root:root 0600` on a board, so the caller is root). It needs no board file and no
daemon, and it does not restart: the caller restarts, or the hardware watchdog does. It exits 0 when the reply word
is `0x80000000` and 1 otherwise; each outcome is one line on its own output and one in the kernel log (`<3>` for a
refusal), which reaches the console and a ramoops record.

The order is for the NEXT BOOT, whatever ends this one: set at the top of a RAM-booted image's `/init`, it also turns
that image's own good restart (into the slot it has just written) into RPIBOOT. No request takes it back: no value
that clears it is documented in the sources this was written from, so the tool offers none. Arm it on the paths that
give up (no daemon binary, a board file that cannot be read, a daemon that will not start), or where landing in
RPIBOOT is the wanted end, such as a bench boot of an untried image. The QEMU check is the `way-back` stanza of
`testing/qemu/run.sh` on the mailbox stand-in; the board check is in
[`testing/board-runs/WAY-BACK-TOOL.md`](../testing/board-runs/WAY-BACK-TOOL.md).

## Settings

```toml
[[devices]]
id = "slots"
driver = "system-slots"
config = { data = "/data", boot = "/boot/firmware", record = "tryboot", boot_sources = ["rpi-boot-mode-1=/soc@107c000000/mmc@fff000"], boot_args = "os", reserve_mb = 2048, keep = 3 }
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
comes from) or `controller` (the device-tree path of the storage controller the target is the one whole disk
under; see "The target by its controller" below), `allow_first_flash` (`false` by default: a write is refused unless it is `true`, which
only a flasher's board file sets), `kind` (`block`, `file` for a regular file in tests, or `mtd`
for a raw NAND partition: see "Raw NAND" below),
`max_dict_mb` (8 by default; 1 to 8, the ceiling: a board file may lower it, never raise it),
`keys` (given by the host from `[security] keys`, the board's one trust root: one per line, 64 hex digits, a space, then the owner; a `keys` in the device's own config is refused), and `mountinfo` (the mount table the running-system
check reads, `/proc/self/mountinfo` by default). A board's `[board] id` is given to the driver, and a
manifest for another board is refused first.

### The target by its controller

A board whose kernel may number its disks otherwise (an eMMC beside an SD card or a USB disk) names the target by
its storage controller: `controller = "/soc@107c000000/mmc@fff000"`, the device-tree path the controller's sysfs
`of_node` names, as the slot driver's `boot_sources` do. The target is the one whole disk under that controller,
found the slot driver's way (`lemnos_slot_store::disk::disk_under`): an eMMC's hardware areas (`mmcblk0boot0`,
`mmcblk0boot1`, `rpmb`), hidden paths and empty slots are set aside, so the target is the eMMC's user area and its
boot areas are never written. No disk, or two, under the controller is refused by name (`info` says what was seen).
The disk is looked up again at each `info`, `write` and `verify`, so for a daemon running as root a disk that
registers after it starts (an eMMC's initialisation) is taken once it is there; under its own user, see "The target
under the daemon's user" below. Its node is `/dev/<disk>`, and it must be the device sysfs
names for the disk (its `dev` number), else the write is refused by name. `sysfs` (default `/sys`) and `dev`
(default `/dev`) are where they are looked up: test settings only. A controller names a disk, never a NAND partition
(`kind = "mtd"` is refused with it), and `path` and `controller` together are refused.

### The target under the daemon's user

A target is a disk node, `root:disk 0660` or `root:root 0600`, which the daemon's own user can't open by name. The
driver's root phase (`lemnos_driver_system_flash::root_phase`, one of the daemon's `ROOT_PHASES`) opens it read-write
before the drop: the `path`, or the one whole disk under the `controller` as it is then. The node stays root's; the
open file is kept for the driver (`target::hand_over`), and every later `info`, `write` and `verify` takes a copy of
it. The copies share one file position, so every read and write gives its own offset. The root phase's line in the
daemon's log says which it did: `<node>: handed over read-write`, or `not handed over` with the reason. Not handed
over: a regular-file target (a test's, opened by name); a target the running system is mounted from (its write is
refused anyway); and a target that isn't there when the daemon starts. Under the daemon's user a target can be written
only if it was there at the root phase: one that wasn't is refused at its `info` and its `write` by name ("it is
root's, and the daemon's root phase did not hand it over"), and the job is not started. A flasher image's kernel
registers the eMMC before its init starts the daemon. The partition re-read after a write (`BLKRRPART`) needs
`CAP_SYS_ADMIN`, which the daemon does not keep: under its user, a write that is otherwise whole ends with the
sentence that the kernel did not re-read the table, and the restart the flasher does next makes the new table the one
in use.

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
- Limit: blocks ≤ 8192 per stream. The index lists every block and the decoder keeps them all; a
  stream with more is refused by name.
- Limit: dictionary ≤ 8 MiB per block (`max_dict_mb`).
- The block size sets the block count. For an image of `S` bytes the smallest block size that keeps
  the stream within 8192 blocks is `S / 8192` rounded up: 20 KiB for a 160 MiB image, 1 MiB for an 8 GiB
  image. Build with a larger block size than that, never a smaller one.
- The bytes do not depend on the thread count: `-T2` and any larger `-T` give the same file. `-T1` is a
  different, single-thread encoding (no sized block headers), so the build uses `-T2`.

The build's command, with the 64 MiB block size the threaded encoder is given:

```text
xz -6 --check=crc64 --block-size=64MiB -T2 -k image.img    # image.img.xz, 8 MiB dictionary per block
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
3. The record's key id is looked up in the board's keys (`[security] keys`). A board with no key list has no known keys.
4. The table decides. The size and the raw SHA-256 are enforced during the stream in every row.

| security switch | image | key | result | outcome |
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
the log, and in the write job's result as its `outcome` field: `0` verified, `1` not checked
(unsigned), `2` not checked (unknown key). The sender's UI shows the word and its reason.

In every accepted row the file's own second stream must be the record given at `manifest`, byte for
byte. An empty record and a file with no second stream count as the same. A raw image has no second
stream, so the record check does not apply to it.

### Calls

1. `manifest(json, record)`: accepts the transport JSON and the record (empty when unsigned, which is
   what the table above is about). Nothing is written.
2. `write(size, format)`: a long job. `size` is the stream's bytes as sent (both streams), `format` is
   1 for xz and 0 for raw. Its progress is the stream bytes taken; its result is text in fields,
   `raw_bytes=<the raw image's byte count> outcome=<code>`.
3. `data(offset, bytes)`: the stream in order from offset 0. The last piece ends the stream, and the
   job then checks the image, its record, and ends.
4. `verify()`: optional, a second long job. It reads the target back from its start for the
   manifest's size and checks the hash. Its result is text in fields, `raw_bytes=<the byte count
   checked>`.
5. The flasher restarts the board. The driver does not restart it.

A job's result is text in fields, `name=value` with one space between them, never a number in
binary: a caller, `lemnos-ctl` and the Orion bridge all show and read the same bytes, and nothing
depends on whether a count's bytes happen to be text. The contract crate's `write_result` writes it
and `read_result` reads it; a reader ignores a field it does not know, and fields are only ever added
after these. Through the bridge, `lemnos.<device>.call verify` ends `Succeeded` with the output
`raw_bytes=<n>`, the same form as the commit's detail below.

The job's statuses: `0` with the raw byte count and the outcome code, after the image's hash matches
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
   with the job's status or the refusal. A committed answer's detail is fields, not a sentence:
   `raw_bytes=<n> outcome=<code>`, one space between them, the code the job's (`0` verified, `1` not
   checked: unsigned, `2` not checked: unknown key) or `none` when the job reported none. A reader
   ignores a field it does not know; fields are only ever added after these.

A push that breaks is begun again from zero; a streamed image is not resumed.

**The manifest after a broken push.** The manifest is a file in the device's directory, committed
like any small file (written under a hidden name, renamed into place once its SHA-256 checks). It
stays there until a push of the same name replaces it: a broken image push, the sender's session
ending, the bridge's own Orion session reconnecting and the bridge restarting all leave it. So an
image push begun again, on the same session or a new one, needs no manifest pushed first; pushing
it again is allowed and replaces it. Each begin reads the manifest from the file and gives it to the
device again. A begin while the broken stream is still open ends that stream first: its write job is
cancelled, and the host ends the job in the same driver entry as the cancel, so the device takes the
manifest again without `BUSY`. A manifest that is not the image's (an earlier image's, say) is not a
silent risk: the device checks the stream against it and the push ends `rejected`. The directory is
the board's (`LEMNOS_ORION_FILES`); on a flasher in RAM it goes with a reboot, and the manifest is
then pushed again. A manifest push that itself breaks leaves the manifest that was there.

**How the PC reaches a flasher's node.** A sender reaches `lemnos.flash` and `lemnos.reboot` as a
remote client of the board's comm core, over tcp with Orion's signed handshake and an enrolled
operator, carried on the gadget's CDC NCM function. The FunctionFS link (`orion-link`) is not that
route: Orion's `links.usb` is a link to a device. Its far end says hello as a device and does not
reach the node's actions or file sinks (Orion at `3f60576c`). The flasher's board file needs nothing
beyond its gadget with the default composition: its NCM function gives the board the interface
`lemnos0` with addresses derived from the board's serial (`contracts/usb-link.toml`, `[network]`).
The image gives `lemnos0` its address and runs the comm core with a tcp listener on it, and the
PC gives its own end an address. Those are the image's and the PC's settings, not Lemnos's.


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

### The watchdog in a flasher

The flasher is the first-flash image. Its `/init` is not `lemnos-slot-init` and is not in this tree, so
the chain of "The watchdog chain" holds with one window unknown here. Windows 1 and 2 are the firmware's
and the kernel's, as before. Window 3 (the flasher's own start to its first open) is not checked: the
flasher's init must open the device within the 30 s of the open timeout, or lemnosd's bind has to come
within it, or the kernel stops feeding and the board resets during the flash. Once lemnosd is running
the chain is windows 5 and 6, as on a slot board. The image builder that makes the flasher must state its
init's open (or lemnosd's bind) within 30 s of probe, and the first-flash board run records the probe-to-open
time (`testing/board-runs/WDT-CHAIN.md`).

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
