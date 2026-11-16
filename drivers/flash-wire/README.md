# lemnos-flash-wire

The wire of the flash link: what a PC-side flasher sends to an on-chip stage, what the stage sends back, the USB
identity the device presents, the timing a host must meet, the regions the stage polices, the compare rule a host
follows before it sets a mark, and the identity record a stage answers with. It is shared by both sides. No
dependencies, no `unsafe`, no_std.

The PC side is another project. It pins this crate by a locked revision (see "How the PC side pins it"). Nothing
here knows which chip is flashed.

## What a host claims on the bench

The device enumerates as:

| | |
|---|---|
| USB vendor:product | `1209:0001`. **This is the pid.codes test pair. It is for tests only and is to be replaced before a release.** |
| Product string (index 2) | `Lemnos flash stage` |
| Interface string (index 3, the interface's `iInterface`) | `lemnos flash wire XXXX`, where `XXXX` is the wire's short hash (`WIRE_SHORT`) as four upper-case hexadecimal digits. Exactly this text is `lemnos_flash_wire::usb::INTERFACE_NAME`. A host checks it before it claims the interface: it names the wire and the revision. |
| Configuration | 1, bus powered, 100 mA |
| Interface | 0, class `0xFF` (vendor specific), subclass 0, protocol 0, two endpoints |
| Bulk OUT | endpoint `0x01`, bulk, 512-byte packets |
| Bulk IN | endpoint `0x81`, bulk, 512-byte packets |
| Control endpoint | 64-byte packets |

**High-speed only.** The bulk endpoints are 512 bytes, which is the high-speed bulk packet size. A full-speed port
cannot configure them: the device does not enumerate on a full-speed port, and a host that sees it on one must
treat it as a failed enumeration, not retry at full speed.

A host claims interface 0 after SET_CONFIGURATION 1, then uses `0x01` and `0x81` and nothing else.

Standard requests the device answers: GET_STATUS (device, interface 0, and endpoints `0x00`, `0x01`, `0x81`, each
as two zero bytes), GET_DESCRIPTOR (device; configuration; strings 0, 2 and 3 in US English), GET_CONFIGURATION,
SET_ADDRESS (1 to 127), SET_CONFIGURATION (0 or 1). Every other request is stalled. A stall does not end the session.

**SET_ADDRESS timing** is the controller's, not the protocol's: the shared control code calls the controller's
`set_address` on the side that `address_before_status` says. The DWC3 stage says "before the status stage", from
the mainline set-address handler (ep0.c:598-626). This is a bench check.

## Frames, and what a transfer is

**A frame is a transfer.** A host sends one frame per bulk transfer and never splits a frame across two, and never
ends a transfer inside a frame. The stage posts a receive of exactly the length its request names, and a short packet
is a failure of that transfer: the stage answers it with a bad-request status (below) and nothing is written. A host
must read each reply with a buffer as large as the reply the request allows, and accept only two lengths: the full
reply (status ok) or exactly one byte (a status other than ok). Any other length is a failure on the host's side.

**A host posts its IN read within `TRANSFER_BUDGET_US` (3.5 s) of sending a request.** A reply the host does not take
in that time ends the session: the stage's send times out, and that is a link failure, not a refusal.

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

## One clock: the session's limits

Every limit in a session is one deadline: the **idle deadline**, `IDLE_LIMIT_US` (30 s) after the last watchdog
feed, or after the start of the session. The start counts as a feed, because the watchdog is armed then.

- **A feed moves the deadline.** A verified write or a read that was sent moves it to the feed plus 30 s.
- **A reply does not.** An identify, a refused request, a status: none moves the deadline. A host that identifies every
  29 seconds reaches the idle deadline at 30 seconds after the last feed, exactly as a host that never asks does.
- **A bus reset does not.** Re-enumeration after a reset shares the deadline. It does not get a fresh 30 seconds, so a
  host that resets the bus cannot keep the watchdog from firing.
- **The idle outcome comes first.** The idle deadline is earlier than the watchdog's period from the same feed, so the
  session ends with its own outcome (`Serve(Idle)` on the stage, or the host's idle end) before the watchdog resets the
  stage. A host sees no difference except the outcome's name.

## Session rules: what ends it, and what does not

- **A refused request** (status 1 or 5) and **a failed storage operation** (status 2 or 3) do not end the session.
  The host's next request is served.
- **An END** replies status 0 and resets the device with the kind it named.
- **The idle deadline** ends the session with its idle outcome (above).
- **A bus reset or an unconfiguration** (SET_CONFIGURATION 0, or a reset from the host) sends the stage back to
  enumeration, on the same deadline. The host enumerates again and reissues the request it was on. A chunk that was in
  flight is not written; the host re-verifies whatever it was writing.
- **A link cut** is a transfer that started and never finished. No reply is possible, so the session ends on its
  timeout. This is the only silent failure, and it is silent because nothing can be sent.

## Timing contract

The numbers are constants in `src/timing.rs`, folded into the wire hash. A host uses the same numbers.

| Constant | Value | Meaning |
|---|---|---|
| `TRANSFER_BUDGET_US` | 3.5 s | the longest one transfer may take: one frame, or one control stage |
| `ROUND_PHASES` | 4 | a write round's bounded phases: receive the chunk, write, read back, send the status |
| `ROUND_BUDGET_US` | 14 s | the round: `ROUND_PHASES` times `TRANSFER_BUDGET_US` |
| `IDLE_LIMIT_US` | 30 s | the idle deadline, from the last feed or the start |
| `FEED_TO_FEED_US` | 44 s | the worst gap between two feeds: `IDLE_LIMIT_US` plus `ROUND_BUDGET_US` |
| `REFERENCE_WATCHDOG_PERIOD_US` | 44.739 s | the longest period of the reference watchdog that covers its 2^31 cycles at 24 MHz |
| `HOST_BULK_TIMEOUT_US` | 5 s | the host's default bulk timeout the contract is sized against |

**The arithmetic, in one place.** The watchdog must be fed again before its period from the last feed. The longest
gap between two feeds is: the idle deadline falls 30 s after a feed; a request can arrive just before it; its round
then runs to its feed (the chunk, the write, the read-back: three of the four phases; the status follows the feed).
That is at most 30 s plus 10.5 s, below `FEED_TO_FEED_US`, which is taken as 30 s plus the full 14 s round. The build
checks `FEED_TO_FEED_US < REFERENCE_WATCHDOG_PERIOD_US` at compile time (`timing.rs`), so a change that breaks the
watchdog fails the build. The reference watchdog's period that covers 44 s is the one with `TOP` 14 (44.739 s).

**The watchdog period** is the smallest the stage's watchdog offers that covers `SESSION_BUDGET_US` (which is
`FEED_TO_FEED_US`). On the reference watchdog that is the 44.7 s period. A stage whose watchdog offers a longer
period still uses this one.

Every bound is a deadline on the stage's clock. No wait spins without one.

## Feed and reply order

The stage feeds the watchdog after the work of a round is verified (for a write: the chunk's CRC matches, the region
admits it, and the read-back equals the payload; for a read: the reply has been taken by the link), and then sends the
status. A lost status cannot hide a fault: the round was verified before it, and a session that then ends parks and is
reset. A fault inside the round is never fed.

## Regions, and the compare rule

The stage polices a card through a table of regions, published in its identity record. A region is a range of sectors
`first..end` (end exclusive) and a class:

- **plain**: a verified payload is enough.
- **boot area**: a write needs the compared mark.
- **header**: a write needs the compared mark too, and its payload verified first. The boot record (sector 64) is a
  header.

A write is classed by the strictest region any of its sectors falls in (`region::classify`). A write that straddles
into the boot area is a boot-area write.

The Rockchip stage publishes two regions: the **boot area, sectors 64 to 32767** (the whole loader area before the
uboot-env partition at sector 32768), and the **header, sector 64**. The host plan's `loader_sectors` (64 in the old
tests) is how many sectors of loader image a plan writes; the stage protects all of the area, because it cannot know the
image's length.

**The compare rule.** The stage checks the payload's CRC and the region gating. It cannot know that the host compared
anything: the compared mark is the host's word. The host decides each write with `region::decide`, which is this rule,
in code:

1. **Plain write** (the write's sectors are in no boot area and no header): the host writes it without the mark. It
   compares nothing, and the stage does not ask it to.
2. **Boot-area or header write** (the write lies in a boot area or a header, or straddles into one): the host compares
   **every sector of the write**, from `lba` to `lba + count`, **byte for byte**. The card side is a fresh read of that
   range from the card; the baseline side is the image bytes the host means to write for the same range. Two outcomes:
   - **All bytes equal**: `Decision::Skip`. Nothing is written and no mark is set. The write is not needed.
   - **Any byte differs**: `Decision::Write { mark: true }`. The write is made with the mark. Only the range that was
     compared is written.
   A read that fails, or a card side or image side of the wrong length, is `DecideError::Length`: nothing is written.

The straddle case is one case, not two: the whole write is compared, so a difference in its plain part alone still sets
the mark, and an equal boot part with a differing plain part is marked too. There is no comparison of only the boot
sectors.

The baseline is the host's image, never the card. A read of the card is never a baseline for itself.

**Validity.** A comparison is valid until the next write or the next reset, whichever comes first. A write changes the
card, so every comparison before it is stale; the host reads and compares again before its next boot-area or header
write. A host that reads twice before a write (once to plan, once to act) must still compare the second read.

Tests: one per case in `region.rs` (`a_plain_write...`, `a_boot_area_write_whose_range_equals...`, `..._differs...`, the
header pair, the straddle pair, the length refusal, and the agreement of stage and host on the classification).

## Identity record

A stage answers an identify with its identity record. The record carries:

- the magic `LI`, its length, and one entry per family (family id and its hash, below);
- the stage's **build id**: a `u64` set at build time. Zero means unset, and **identify requires a non-zero build id**
  (the scenarios check it). The Rockchip stage takes it from `LEMNOS_ROCKCHIP_STAGE_BUILD_ID` (decimal). **The bench
  image must set that variable.** A host's plan should also carry the image's sha256, because the stage cannot know its
  own file hash, and the build id alone does not say which file was loaded;
- the **region table**: a count, then up to `MAX_REGIONS` (4) regions (first, end, class). A host predicts a "not
  compared" refusal from these, rather than from the stage's source;
- trailing bytes a reader does not know, skipped (the length covers them).

Families this build lists are checked by hash. A family listed twice is refused (`Skew::Duplicate`), not resolved. A
missing family is refused. The table bounds (`MAX_REGIONS`, `MAX_PEER_ENTRIES`) are folded into the identity family's
hash, so a host built against other bounds does not agree with this record.

## Hash

Each family has a 64-bit FNV-1a, computed at compile time (`src/layout.rs`) from the declarations in the code. A change
to any declared value moves the hash of its family, and so the hash of the wire. `WIRE_HASH` folds the family ids and
hashes in order. `WIRE_SHORT` is a 16-bit number derived from `WIRE_HASH`, for naming an image or a log line; it
identifies nothing on its own.

The families: frame (1: magic, sizes, CRC, reserved bits, the timing contract), ops (2), status (3), reset (4), identity
(5: the record's head, layout and bounds), usb (6: the identity above, including the interface string's prefix).

The values this build computes:

| Name | Value |
|---|---|
| wire hash | `0x12B1_412A_AA81_ACE8` |
| wire short | `22002` |
| frame family (1) | `0xE0D8_DB7C_F61E_3D06` |
| ops family (2) | `0xC170_D815_DF17_757F` |
| status family (3) | `0xE2B5_4BEB_21DA_BF76` |
| reset family (4) | `0x40FE_F3FE_A154_445F` |
| identity family (5) | `0x2065_1D23_2C11_F17D` |
| usb family (6) | `0x8F59_A043_B5C3_0419` |

The tests pin these values (`src/layout.rs`), so a layout change fails the build until the pins change in the same
commit. Each was recomputed outside this crate by a separate FNV implementation over the same byte walk; the hash
function's own copy is checked against the vectors in `src/hash.rs`, which are the same numbers `lemnos-wire` (in
`engine/wire`) checks for its copy, in a test on each side.

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
- `src/region.rs`: the classifier and one test per case of the compare rule.
- `src/timing.rs`: the watchdog bound, checked at compile time.
- `src/layout.rs`: the pinned hashes.
- `src/hash.rs`: the shared FNV vectors (also checked in `engine/wire`).
