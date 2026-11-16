# lemnos-flash-wire

The wire of the flash link: what a PC-side flasher sends to an on-chip stage, what the stage sends back, the
USB identity the device presents, the timing a host must meet, the regions the stage polices, and the identity
record a stage answers with. It is shared by both sides. No dependencies, no `unsafe`, no_std.

The PC side is another project. It pins this crate by a locked revision (see "How the PC side pins it"). Nothing
here knows which chip is flashed.

## What a host claims on the bench

The device enumerates as:

| | |
|---|---|
| USB vendor:product | `1209:0001`. **This is the pid.codes test pair. It is for tests only and is to be replaced before a release.** |
| Product string | `Lemnos flash stage` (string index 2) |
| Configuration | 1, bus powered, 100 mA |
| Interface | 0, class `0xFF` (vendor specific), subclass 0, protocol 0, two endpoints |
| Bulk OUT | endpoint `0x01`, high speed, 512-byte packets. The host's frames go here |
| Bulk IN | endpoint `0x81`, high speed, 512-byte packets. The replies come from here |
| Control endpoint | packet size 64, the standard requests only (below) |

A host claims interface 0 after SET_CONFIGURATION 1. It then uses `0x01` and `0x81` and nothing else.

Standard requests the device answers: GET_STATUS (device), GET_DESCRIPTOR (device 1, configuration 2, strings
0 and index 2 in US English), GET_CONFIGURATION, SET_ADDRESS (1 to 127), SET_CONFIGURATION (0 or 1). Every other
request is stalled. A stall does not end the session.

## Frames, and what a transfer is

**A frame is a transfer.** A host sends one frame per bulk transfer and never splits a frame across two, and
never ends a transfer inside a frame. The stage posts a receive of exactly the length its request names, and a
short packet is a failure of that transfer: the stage answers it with a bad-request status (below) and nothing is
written. A host must read each reply with a buffer as large as the reply the request allows, and accept only
two lengths: the full reply (status ok) or exactly one byte (a status other than ok). Any other length is a
failure on the host's side.

**Request**, 12 bytes:

| Offset | Size | Field |
|---|---|---|
| 0 | 2 | magic `LM` |
| 2 | 1 | op: 1 read, 2 write, 3 end, 4 identify |
| 3 | 1 | reserved, meaning per op (below) |
| 4 | 2 | count, 1 to 128 sectors |
| 6 | 4 | lba, first sector |
| 10 | 2 be | CRC-16/CCITT-FALSE over bytes 0 to 9 |

Reserved byte, by op: read and identify take 0. Write takes `1` (the compared mark) or 0; any other bit is refused.
End takes the reset kind: 0 normal, 3 maskrom. Any other END value is a normal reset, as the first stage read it.

**Chunk** (host to stage, one transfer, after a write request): exactly `count * 512 + 2` bytes: the payload, then
CRC-16 (big-endian) over the payload. A chunk of any other length is a bad-request status, and nothing is written.

**Replies**, all one transfer each:

| Reply to | Bytes | Meaning |
|---|---|---|
| read | `1` status alone, if not ok | the read failed in storage; nothing follows |
| read | `1 + count*512 + 2` on ok | status 0, the sectors, then CRC-16 (big-endian) over the status and the sectors |
| write | `1` | the status of the write (below) |
| end | `1` | status 0; the stage then resets with the kind the END named |
| identify | the identity record (below) | |
| refused request | `1` | the status for the refusal (below) |

**Status bytes**:

| Byte | Name | Meaning |
|---|---|---|
| 0 | ok | done |
| 1 | crc | the request's CRC or the chunk's CRC did not match. Nothing was written |
| 2 | storage | the storage failed. **The range may be partly written.** The host reads the range back and verifies it before it relies on it |
| 3 | verify | the read-back of a write did not equal what was written. **The range may be partly written**, as for 2 |
| 4 | not compared | the write went to a boot area or header without the compared mark. Nothing was written |
| 5 | bad request | the frame was not a request this build understands (a wrong length, an unknown op, a count outside 1 to 128, a reserved bit the op does not define), or a chunk had the wrong length. Nothing was done |

## Session rules: what ends it, and what does not

- **A refused request** (status 1 or 5) and **a failed storage operation** (status 2 or 3) do not end the session.
  The host's next request is served.
- **An END** replies status 0 and resets the device with the kind it named.
- **The idle limit.** A host that sends no request within `IDLE_LIMIT_US` of its last reply loses the session. The
  stage ends it and the watchdog's reset is the exit. The host starts again.
- **A bus reset or an unconfiguration** (SET_CONFIGURATION 0, or a reset from the host) ends the bulk session and
  sends the stage back to enumeration. The host enumerates again and reissues the request it was on. A chunk that
  was in flight is not written; the host re-verifies whatever it was writing.
- **A link cut** is a transfer that started and never finished. No reply is possible, so the session ends on its
  timeout. This is the only silent failure, and it is silent because nothing can be sent.
- **A host that never configures the device** is ended after `ENUMERATION_LIMIT_US`. The host starts again.

## Timing contract

The numbers are constants in `src/timing.rs`, folded into the wire hash. A host uses the same numbers.

| Constant | Value | Meaning and why |
|---|---|---|
| `TRANSFER_BUDGET_US` | 3.5 s | the longest one transfer may take: one frame, or one control stage. Chosen so that three transfers fit in one round, with margin, and a host's 5 s default bulk timeout is longer than it. |
| `ROUND_BUDGET_US` | 10.5 s | one round, from a request's chunk to its reply: the chunk transfer, the storage operation and the reply transfer, each of `TRANSFER_BUDGET_US`. |
| `IDLE_LIMIT_US` | 30 s | the longest a host may pause between a reply and its next request. Six times the host's 5 s default timeout, so a slow hub or a retried transfer does not lose the session. The earlier 1 s limit lost sessions to a pause just over a second; this one does not. |
| `ENUMERATION_LIMIT_US` | 30 s | the longest the stage waits for a configuration after its start or a bus reset. |
| `SESSION_BUDGET_US` | 40.5 s | the idle limit plus one round: what the watchdog must cover from its last feed. |

**The watchdog.** The stage's period is the smallest one that covers `SESSION_BUDGET_US`: on the reference watchdog,
periods run from 2.7 ms to 89.5 s, so the period is 44.7 s. A stage that hangs inside a round is reset within that
period. An idle host is not a hang: the stage ends the session at the idle limit, which is inside the period.
The watchdog is fed only after a verified round (see the stage's README), so enumeration, which has no round, is
bounded by `ENUMERATION_LIMIT_US` and by the watchdog together. A reset during a session returns the board to a
normal boot, not to maskrom. The host must treat any stop as "re-verify before you rely on the card".

Every bound is a deadline on the stage's clock. No wait spins without one.

## Feed and reply order

The stage feeds the watchdog after the work of a round is verified (for a write: the chunk's CRC matches, the
region admits it, and the read-back equals the payload; for a read: the reply has been taken by the link), and
then sends the status. A lost status cannot hide a fault: the round was verified before it, and a session that
then ends parks and is reset. A fault inside the round is never fed.

## Regions, and the compare rule

The stage polices a card through a table of regions, published in its identity record. A region is a range of
sectors `first..end` (end exclusive) and a class:

- **plain**: a verified payload is enough.
- **boot area**: a write needs the compared mark.
- **header**: a write needs the compared mark too, and its payload verified first. The boot record (sector 64)
  is a header.

A write is classed by the strictest region any of its sectors falls in. A write that straddles into the boot
area is a boot-area write.

The Rockchip stage publishes two regions: the **boot area, sectors 64 to 32767** (the whole loader area before the
uboot-env partition at sector 32768), and the **header, sector 64**. The earlier table in the tests (sectors 64
to 32768 with a header at 64) and the host plan's `loader_sectors` of 64 are different things: the plan's 64 is
how many sectors of loader image a plan writes, and the stage protects all of the area, because it cannot know
the image's length.

**The compare rule.** The stage checks the payload's CRC and the region gating. It cannot know that the host
compared anything: the compared mark is the host's word. The host must not set the mark on a write until it has:

1. **Read the range the write covers, in full, from the card.** For a write to a boot area or a header, that is
   every sector of the write, and for a multi-sector write, every sector of it that is in a boot area or a header.
2. **Compared that read with the baseline**, which is the host's own copy of what the range must hold: the image it
   means to write for the range. The baseline is the host's, not the card's. A read of the card is never a baseline
   for itself.
3. **Decided from the comparison.** If the range already equals the image, the host does not write it. If it
   differs, the host sets the mark only for the write it has decided to make. The mark is never set on a range the
   host has not compared.

The comparison is valid until the next write or the next reset, whichever comes first. A write to any region
changes the card, so every compare before it is stale, and the host reads and compares again before its next
compared write. A host that reads twice before a write (once to plan, once to act) must still compare the second
read.

## Identity record

A stage answers an identify with its identity record. The record carries:

- the magic `LI`, its length, and one entry per family (family id and its hash, below);
- the stage's **build id**: a `u64` set at build time from the image's inputs. Zero means unset. A host's plan should
  also carry the image's sha256, because the stage cannot know its own file hash, and the build id alone does not say
  which file was loaded;
- the **region table**: a count, then up to four regions (first, end, class). A host predicts a "not compared"
  refusal from these, rather than from the stage's source;
- trailing bytes a reader does not know, skipped (the length covers them).

Families this build lists are checked by hash. A family listed twice is refused. A missing family is refused.

## Hash

Each family has a 64-bit FNV-1a, computed at compile time (`src/layout.rs`) from the declarations in the code. A
change to any declared value moves the hash of its family, and so the hash of the wire. `WIRE_HASH` folds the family
ids and hashes in order. `WIRE_SHORT` is a 16-bit number derived from `WIRE_HASH`, for naming an image or a log
line; it identifies nothing on its own.

The families: frame (1: magic, sizes, CRC, reserved bits, the timing contract), ops (2), status (3), reset (4),
identity (5: the record's head and layout), usb (6: the identity above).

The values this build computes:

| Name | Value |
|---|---|
| wire hash | ``0xEF7C_B550_316C_1D7A`` |
| wire short | ``30266`` |
| frame family (1) | ``0x883E_42AC_7085_675A`` |
| ops family (2) | ``0xC170_D815_DF17_757F`` |
| status family (3) | ``0xE2B5_4BEB_21DA_BF76`` |
| reset family (4) | ``0x40FE_F3FE_A154_445F`` |
| identity family (5) | ``0xDA93_D5B6_F43E_8100`` |
| usb family (6) | ``0x4580_FE1F_F9B1_A6EB`` |

The tests pin these values (`src/layout.rs`), so a layout change fails the build until the pins change in the
same commit. Each was recomputed outside this crate by a separate FNV implementation over the same byte walk; the
hash function's own copy is checked against the vectors in `src/hash.rs`, which are the same numbers `lemnos-wire`
(in `engine/wire`) checks for its copy.

The hash function is carried in this crate by design. The crate has no dependencies, so a PC side can pin it alone.

## Rules

- **Compatibility is structural.** A version number is never compared. A peer agrees with this crate when it lists
  each family with the same hash.
- **Additive changes.** A reader skips trailing bytes it does not know. A family a peer lists that this build does not
  know is ignored, so a peer with an extra family still connects.
- **Skew is a typed refusal.** `identity::check` returns `Skew::Malformed`, `Skew::Duplicate`, `Skew::Missing` or
  `Skew::Hash`. Nothing is guessed from a mismatch.
- **Fixed-size frames do not grow.** A request is 12 bytes and every reply has one of the shapes above. A new op, a new
  status, a new reserved bit or a new timing number changes its family's declaration and so the hash: a skew, not a
  silent extension.
- **CRC.** CRC-16/CCITT-FALSE: polynomial 0x1021, initial 0xFFFF, no reflection, no final XOR. Check value `0x29B1`
  for `"123456789"`.

## How the PC side pins it

**What a git pin brings.** A git pin of this crate reads the repository's workspace manifest (the root `Cargo.toml`) for the inherited package fields: version, edition, license, repository and homepage, and `rust-version` (the minimum Rust version, 1.94). The lint table is inherited the same way. The wire crate has no dependencies of its own; the stage crate depends only on the wire crate, by path in this repository. Edition 2024 and rust-version 1.94 are required, as the workspace states them.

Depend on this crate at a locked revision of the repository that holds it, never on a path into a checkout that
moves. In the PC side's code, assert the wire hash at start:

```rust
assert_eq!(lemnos_flash_wire::WIRE_HASH, EXPECTED_WIRE_HASH);
```

On connect, read the identity record and call `lemnos_flash_wire::identity::check(&record)`. Treat any `Skew` as a
refusal that names the family. The returned `Peer` gives the build id and the region table. Do not compare a revision
string or any version number.

## Tests

- `tests/frames.rs`: a vector for every request op and its refusals; the CRC check value; the reset kinds. The expected
  bytes were computed with a separate CRC implementation.
- `src/identity.rs`: the record's check: a hash mismatch, a missing family, a duplicate family, trailing bytes, a foreign
  family, the build id and the region table, and malformed bytes.
- `src/layout.rs`: the pinned hashes.
- `src/hash.rs`: the shared FNV vectors.
