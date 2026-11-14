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
| the set of boot files | `boot.files` | exactly the five role files the boot partition stores (`kernel8.img`, `initramfs.cpio`, `board.dtb`, `cmdline.txt`, `overlays/lemnos-board.dtbo`), on every form: an entry that is not one of them, or a role file the list leaves out, is refused by name at the decision, before the first payload byte (see "Manifest and members") |
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
record = "/var/lib/lemnos/security.rec"   # absolute: the switch's truth
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
  to off replies empty. With `orion = true` there is no such line, and a change to on stays refused by
  name until Orion can be asked.
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

**A change** (`set`, `lemnos-ctl security DEVICE on|off`) runs: the stakeholders' prechecks; Orion's
answer when Orion is on the board; for an off on a board with a trust root, the authorization (below);
the record; the render. Refused by name when a fact stops it. Refused in either direction while an
install or a trial is running, or a write or verify is running on first flash. An off has no other
precondition. On needs:

- a trust root: the board's `[security] keys` list has a valid line (one list, seen by each stakeholder
  and by the switch);
- on slots, the default slot and the running slot `verified` under that list;
- Orion's answer, when `orion = true`. Orion's precheck is not at Orion's current revision, so ON is
  refused with that reason until it is.

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

### The Orion precheck (designed, not wired)

With `orion = true` on the board, turning the switch on is refused by name, because Orion cannot yet be
asked (the marker `ORION-R3` in `drivers/system-security/src/flow.rs`). This section is the contact point
Orion's side builds to. Nothing here is implemented yet.

- **Where.** The Orion bridge's device serves the contract `system.security-precondition/1`, as the
  slot and first-flash drivers already do. The board binds it in the `system-security` device:
  `uses = { orion = "<the bridge device's id>" }`. The driver asks it through the same path as the
  other stakeholders (`ask("orion", …)`), so its lines are named `orion: …` in the refusal.
- **What the driver calls.** Method 1, `precondition(on: bool, into: &mut [u8])`, with `on` the value
  the switch would take and a buffer of `TEXT_MAX` (1024) bytes. The method is the same one the slots
  and flash answer; Orion's `SecurityPrecheck` is its implementation behind the bridge.
- **Answer shapes.**
  - *Yes*: the text has no `missing:` or `busy:` line (empty is the usual form). The driver takes it.
  - *No, with a reason*: one or more `missing: <reason>` lines (a fact that is not in place), or
    `busy: <reason>` (work in progress, refused in either direction). The reason is shown in `state`.
  - *Cannot answer*: any status error from the call (no such method, `NOT_SUPPORTED`, `BUSY` as a status,
    an I/O failure). The driver refuses with `missing: it did not answer (<status>)`.
- **Timeout and failure.** The call runs under the host's deadline (`call_deadline_ms`, default 2000 ms;
  `docs/boards.md`). A call that does not return by then is `TIMED_OUT` and is a refusal, as a status
  error is. The driver does not retry the precheck on a timer and never assumes Orion's answer: a
  refusal leaves the switch as it was. A change is asked again by the operator. The switch is never
  turned on by a timeout, and never turned off by one either.
- **Until it is bound.** With `orion = true` and no `orion` need, ON is refused by name, as now. With the
  need bound, the answer above replaces that refusal; `orion = false` boards are not asked at all.

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
is newer), and copy one on a tie (`env/common.c:484-551`, `env_check_redund`).

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

A slot's boot files (kernel, initramfs, device tree, command line, board overlay) are not written under
the slot's name. The boot partition holds **three bay sets**, `slots/bay0/` to `slots/bay2/`, each a
pre-created directory of fixed-size, contiguous files (the layout contract, `contracts/slot-layout.toml`
version 5): `kernel8.img`, `initramfs.cpio`, `board.dtb`, `cmdline.txt`, `overlays/lemnos-board.dtbo`,
`overlays/README`, `bayinfo.rec` (the set's record: the slot in the set and each file's stored length and
SHA-256), and for the U-Boot form `extlinux/extlinux.conf`. Three sets are the default, the previous, and
the one being installed or on trial; no other slot has boot files on the partition, so the partition does
not grow with the keep count. Each file's size is the board's: `[boot] bay_kernel_bytes`,
`bay_initramfs_bytes`, `bay_device_tree_bytes`, `bay_cmdline_bytes` and `bay_overlay_bytes`
([Boards](boards.md)), with no default. **Each is the size of the STORED file**: the bytes of the file as
the slot bundle carries it, written into the bay as they are, then padded with zeros to the bay's size.
Nothing is decompressed or recompressed on the way in. So on the Pi firmware form `kernel8.img` is what
the firmware loads: a gzip-compressed kernel as the bundle ships it (the firmware decompresses it), and
`bay_kernel_bytes` is that compressed size, not the size of the raw `Image`. On the U-Boot form the
kernel is stored raw (a compressed one is refused by name: `booti` needs per-slot decompression
addresses), so there `bay_kernel_bytes` is the raw size. The rule for choosing a size: a bay must hold the
largest file a future slot may bring for that role, so take the largest file of the role over every slot
you expect to install, add margin (the board file's comment says 10 %), and round up. A bay cannot grow
after the image is built. A bundle whose file exceeds its bay is refused by name at install, before
anything is written and with the default still booting: `<slot>: the kernel payload is <n> bytes but its
bay holds <m>: nothing was written, the default keeps booting; rebuild the image with a larger kernel bay
or a smaller payload` (and `lemnos-ctl boot-partition --payload DIR` names the role at build, `the payload
of kernel is bigger than its bay: rebuild with a larger bay or a smaller payload`). The image builder. The image builder writes the whole volume (`lemnos-ctl
boot-partition`): FAT16 with two FAT copies and the smallest power-of-two cluster of at least 512 bytes
that gives fewer than 65,525 clusters, the firmware text, the pointers, the records, and the first slot in
`bay0`. `--min-size` prints the smallest `--size` for the board's bays, one line of digits (with `--payload DIR`,
that line first, then one line per role, `role stored_bytes bay_bytes`, and a non-zero exit naming a role
whose payload is bigger than its bay; the formats of every mode are in [Building](building.md)). `--verify FILE` reads an image or a dump of a real partition and prints one `ok`/`BAD` line per
finding. A payload that does not fit its bay is refused by name at build and at install; nothing is
written.

**Install.** Nothing here creates, renames or deletes a file: every write is a sector inside a file that
already exists. The caller says which bays must not be touched (`bays::Protected`: the default's, the
previous's, the trial's, and a pending trial with attempts left); the Pi form takes them from the pointers
and the trial record, the U-Boot form from the environment (`default_bay`, `previous_bay`, `trial_bay`,
`trial`, `trial_attempts`). The order is: refuse a slot that is on trial, a set a pointer names and a
payload bigger than its bay, all before any write; clear the set's old record if it holds one; write each
file padded to its fixed size; read each back and compare its hash; commit `bayinfo.rec`; read it back.
Switching the default or the trial is a separate step that moves a pointer (Pi) or environment variables
(U-Boot) and no payload. A power cut at any moment, including during an install, leaves the default
bootable and the boot partition booting: the bay being written is never one a pointer names, and a set
whose record is not committed is not a slot. A removal clears a set's record and nothing else.

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
`board.dtb` and names the boot source in it. The install rewrites that one fixed-size file before the set's
record. A compressed kernel is refused by name on this form (`booti` needs `kernel_comp_addr_r` and
`kernel_comp_size` set per slot); the Pi firmware form takes a gzip kernel.

**Records on the volume.** The Pi form has `trial.rec`, `reason.rec` and `episodes.rec`; the U-Boot form
has `reason.rec` only (its trial and default are in the environment, and
`lemnos_slot_store::boot_raw::open_records` opens whichever the image has).

**The every-boot check** (lemnosd's `boot_guard` and slot-init's) finds a slot's bay through the set
records and hashes only the stored bytes of each file, not its padding.

**Not built: raw NAND.** Payload bays on NAND (UBI volumes) are parked. The `ubi` record keeps its files
under `slots/<id>` on its UBIFS boot volume as before, and its install is refused by name (the message
says UBI bays are parked); a board that asks for bays on NAND gets that refusal, not a partial install.
The `file` record has no bay path either: its install writes nothing to the boot partition (its loader reads one fixed kernel).

Board checks that cannot be made on this PC (how the firmware reads a pointer file with a torn sector,
whether `os_prefix=slots/bayN/` applies to every file it reads, U-Boot reading `slots/<bay>/` through
`sysboot`) are in `testing/board-runs/I5i8.md`, each marked untested on board.

### Previous before flashing mode

When the board's loader cannot start the default, the previous slot is tried before the board offers
itself for flashing. The two boot forms do this differently, and only the U-Boot form decides it at the
moment the default's files fail to load.

| | U-Boot form (`packaging/uboot/boot.cmd`) | Pi firmware form (generated `config.txt`) |
|---|---|---|
| Order | in each pass: the trial (while it has an attempt), the default, the previous slot; three passes with a 2 s pause, then download mode (`fastboot usb`), and after each return the slots are tried once more before it is offered again | `config.txt`: `include default.txt`, then `[boot_count&4]` / `include previous.txt`, then `[all]` and the slot lines; `tryboot.txt` includes `trial.txt` and has no count filter |
| What picks the previous | the script, in the same pass, when the default's files did not load or did not start | the firmware's boot count: bit 2 set (counts 4 to 7, 12 to 15, and so on) reads `previous.txt` |
| What the previous is started as | a trial (see below) | the previous slot, without any trial marking; a boot that lands on it by the count is reported by lemnosd as described under "Boot count" |
| Flashing mode | download mode, after three passes that started nothing | RPIBOOT, requested by slot-init after 8 restarts without a good boot, and only from a refusal slot-init itself reaches (see "Boot count"); a firmware that cannot read the default's files never runs slot-init, so Lemnos asks for nothing there and the firmware's own `BOOT_ORDER` applies |
| Exists in generated files today | yes: `boot.cmd` (written into the image by `lemnos-ctl boot-partition`) | yes, the count filter is in `config.txt` (`lemnos_slot_store::firmware::CONFIG_TXT`); there is no Pi step that detects an unreadable default |
| Tested | structurally on this PC (`drivers/system-slots/src/uboot/tests.rs`); the script is not run by any U-Boot here; slot-init's side is run on image files | on this PC the text is only compared; the firmware's behaviour is **untested on board** (`testing/board-runs/I5i6.md`, rows 3 and 15, and `I5i8.md`) |

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
evaluates the script's conditions with a small evaluator, not U-Boot: the script's evaluation by a real U-Boot
is **untested on board** (`testing/board-runs/I5i8.md`, row U6). The step
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

The fixed `config.txt` (`lemnos_slot_store::firmware::CONFIG_TXT`) reads `default.txt` unless the
firmware's `boot_count` has bit 2 set (`[boot_count&4]`), when it reads `previous.txt`. Boots 0 to 3
run the default, 4 to 7 the previous, and so on: the count wraps at 256, a multiple of 8, so the
pattern never breaks. `tryboot.txt` has no count filter. The filter, the `kernel_watchdog_timeout`
keys and the count's read and clear are verified from the Raspberry Pi documentation
(`conditional.adoc`, `config_txt_boot.adoc`), not yet on a board: `testing/board-runs/I5i6.md`.

- The count is read from `/proc/device-tree/chosen/bootloader/count` and the reset status from
  `rsts` (four big-endian bytes each; `device_tree` names the directory). It is cleared through the
  property mailbox on `/dev/vcio` (`vcio`), tag `0x0003808d` with the value zero
  (`lemnos_driver_rpi_firmware_sys`). Both settings are the `system-slots` device's; the defaults
  are the paths above.
- The count is cleared by lemnosd, never by slot-init, in two places: at a boot that is good (a
  confirmed trial, or a default boot that has run its uptime with its boot files matching the
  manifest) and before each restart Lemnos makes. The restart check runs first: a refused check
  makes no restart. A count that can't be cleared does not refuse the restart the board was asked for:
  the state names it ("this boot is counted") and the indicator shows failed.
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
- **Refusals that repeat go to the firmware's flashing mode.** A refusal no slot can answer (the
  boot disk or table is wrong, the data volume is missing, a kernel piece is absent) goes to the
  rescue: slot-init keeps the reason in the kernel log, waits 30 s and restarts. The same refusal
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

### The slot layout (Pi firmware form)

`contracts/slot-layout.toml` (version 5) lists every file of a slot's bay set (`slots/bayN/`) on the
boot partition for the `rpi-tryboot` form, with its role, whether it is required, and its `source`:
the kernel (`kernel8.img`), the initramfs (`initramfs.cpio`), the device tree the slot's kernel built
(`board.dtb`) and `cmdline.txt` are `build` (the image builder supplies them), and `overlays/lemnos-board.dtbo`
is `build` too (the compiled board overlay, see [the board overlay file](boards.md#the-board-overlay-file)).
`overlays/README` is `generated`: it makes `os_prefix` apply to overlays, and `lemnos-ctl boot-partition`
writes it into every bay set, one fixed line.
The generated `config.txt` and `tryboot.txt` name these with the same spelling, and the constants in
`drivers/slot-store/src/layout.rs` are tested against the file. `boot-partition` writes a copy as
`slot-layout.toml` and checks it against the code before it builds an image. The reader refuses any
version but 5 (4 and 6 included), any unknown key, and a `source` other than `build` or `generated`. What
version 5 changed from 4 is stated in the file's header.

A slot's manifest may carry `boot.overlay` (`{"board": ..., "sha256": ...}`): the board's overlay
digest, which the live marker must equal (`docs/boards.md`). `lemnos-ctl tree BOARD --manifest-boot FILE`
writes that object for the board, as the whole `boot` object the generator is the authority for
(`docs/boards.md`, "The board overlay file"). The installer and the image builder
check that a `rpi-tryboot` slot has every required file (`layout::rpi_tryboot_missing`).

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

**The extra pass on tryboot.** The firmware keeps no counter, so on the `tryboot` form every trial
boot runs the default slot's kernel and initramfs first: the initramfs takes the attempt and
restarts with `0 tryboot`. That is one extra firmware, kernel and initramfs pass on each trial boot.
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

## The boot source

Everything the boot chain reads or writes on a board is on the boot disk, the one whole disk the
board booted from, and nothing else. A second disk with the same partition labels is never used.
The boot chain states where it booted from; the board file says which of those sources it has, and
where each one's storage controller is in the kernel's device tree. The disk is the one whole disk
under that controller.

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
comes from), `allow_first_flash` (`false` by default: a write is refused unless it is `true`, which
only a flasher's board file sets), `kind` (`block`, `file` for a regular file in tests, or `mtd`
for a raw NAND partition: see "Raw NAND" below),
`max_dict_mb` (8 by default; 1 to 8, the ceiling: a board file may lower it, never raise it),
`keys` (given by the host from `[security] keys`, the board's one trust root: one per line, 64 hex digits, a space, then the owner; a `keys` in the device's own config is refused), and `mountinfo` (the mount table the running-system
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
