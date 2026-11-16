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
| identify | the identity record (below): all `IDENTITY_LEN` = 104 bytes, unused region slots zero | |
| refused request | `1` | the status for the refusal (below) |

**Status bytes**:

| Byte | Name | Meaning |
|---|---|---|
| 0 | ok | done |
| 1 | crc | the request's CRC or the chunk's CRC did not match. Nothing was written |
| 2 | storage | the storage failed, or ran past its budget, or its read-back failed in storage. **The range may be partly written, and nothing outside the range is touched.** The host reads the range back and verifies it before it relies on it. (The simulated card decides whether a sector fits before it writes it, so a sector torn in the middle cannot appear there; a real card can tear one, and the read-back is what shows it.) |
| 3 | verify | the read-back of a write did not equal what was written. **The range may be partly written**, as for 2 |
| 4 | not compared | the write went to a boot area or header without the compared mark. Nothing was written |
| 5 | bad request | the frame was not a request this build understands (a wrong length, an unknown op, a count outside 1 to 128, a reserved bit the op does not define), a chunk had the wrong length, a chunk was not whole sectors, or a write crosses a region boundary. Nothing was done: no storage call was made |

**Nothing is written for statuses 1, 4 and 5, and for no other status.** Each of the three is decided before the first storage call of its round: the length, the class (`wholly_inside_one_class`), the CRC, and then the gate that needs the compared mark (`region::gate`) all come before `storage.write`. Statuses 2 and 3 may leave part of the range written, as the table says.

## One clock: the session's limits

Every limit in a session is one deadline: the **idle deadline**, `IDLE_LIMIT_US` (30 s) after the last watchdog
feed, or after the start of the session. The start counts as a feed, because the watchdog is armed then.

- **A feed moves the deadline.** A verified write or a read that was sent moves it to the feed plus 30 s.
- **A reply does not.** An identify, a refused request, a status: none moves the deadline. A host that identifies every
  29 seconds reaches the idle deadline at 30 seconds after the last feed, exactly as a host that never asks does.
- **A bus reset does not.** Re-enumeration after a reset shares the deadline. It does not get a fresh 30 seconds, so a
  host that resets the bus cannot keep the watchdog from firing.
- **Control requests take the phase's deadline.** Every controller command a phase issues, including the control data,
  status, stall and endpoint configuration of a SETUP that arrives during it, takes the phase's deadline and cannot run
  past it (R4-B1). A command that cannot finish by it is a timeout, and the session ends with its idle outcome.
- **A frame counts only if it completed by the deadline.** The completion is observed where the stage takes the frame, and
  that observation instant is what is compared with the deadline. Because every controller command returns by the
  deadline, a frame that completed before it is observed before it, and one that completed after it is observed after it.
- **Hosts should not send control requests during a session.** After configuration a host SHOULD send no control request
  (an OS backend may issue its own). This is **not** a requirement the stage relies on: the stage keeps its bound either way.
- **The first window is anchored at send-end.** The host's first fed request must be **completely sent** within
  `FIRST_REQUEST_WINDOW_US` (24 s) of send-end: the instant the host finished sending the stage to the boot ROM. The ROM runs
  the stage only after its last byte, so the stage's arm is at or after send-end, and the first idle deadline
  (`arm + IDLE_LIMIT_US`) is never earlier than `send-end + 30 s`. The host's window is that deadline less the request's own OUT
  bound and the stated margin (the derivation is in the timing section). A stage started without a host download has no
  send-end, and no first window applies to it. This revision has no such path that the contract covers.
- **Phase deadlines are not capped at the idle deadline.** A phase's deadline is its own start plus its budget. A chunk that
  never arrives after a header received late in the idle window ends the session at its phase deadline, which can be up to
  `TRANSFER_BUDGET_US` (3.5 s) past the idle deadline. That end is `Link(Timeout)`, not `Idle`, and it is inside the 40.5 s
  feed-to-feed bound (the last feed is the arm or a prior feed, and no write round is fed). Sim test:
  `a_chunk_that_never_arrives_ends_the_session_as_a_link_timeout_at_its_phase_deadline_past_the_idle_deadline` (it ends at
  32.506 s, not at 30 s).
- **The header receive is armed before the host sends.** After a reply is sent, the stage's next header receive is armed at the
  start of the next receive, before any poll or wait (`usb::BulkLink::recv`, whose first statement is the arm). The host cannot
  send its next request before it has the reply, so the stage has the receive armed before the host sends. Sim test:
  `after_a_reply_is_sent_the_next_header_receive_is_armed_before_any_wait`. The header receive itself is bounded by the idle
  deadline, not by a transfer budget: it waits for the host, as it should.
- **A repeated SET_CONFIGURATION is a no-op.** A SET_CONFIGURATION 1 to a device already configured is answered with its status
  stage, and the controller is not asked to configure its endpoints again (that would touch a bulk transfer that may be armed).
  No deadline moves: the repeat is a reply, and a reply does not feed. Sim test:
  `a_repeated_set_configuration_mid_session_moves_no_deadline_and_the_session_ends_idle_at_30_seconds`.
- **The configuration is recorded after success.** A SET_CONFIGURATION the controller could not make leaves the device
  unconfigured (sim test `a_failed_configuration_leaves_the_device_unconfigured`).
- **The idle outcome comes first.** The idle deadline is earlier than the watchdog's period from the same feed, so the
  session ends with its own outcome (`Serve(Idle)` on the stage, or the host's idle end) before the watchdog resets the
  stage. A host sees no difference except the outcome's name.

The idle deadline is an instant on one clock, counted from the watchdog arm for the first deadline and from the last feed
after it. A request's phase is bounded by the deadline: if its controller command would run past the deadline, the phase
ends at the deadline and the request is not served (sim test `a_request_armed_one_second_before_the_deadline_is_not_accepted_when_the_command_takes_one_second`).

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
| `TRANSFER_BUDGET_US` | 3.5 s | the longest one transfer may take: one frame, one control stage, one storage phase |
| `ROUND_PHASES` | 4 | a write round's phases: receive the chunk, write, read back, send the status |
| `ROUND_BUDGET_US` | 14 s | the whole round: `ROUND_PHASES` times `TRANSFER_BUDGET_US` |
| `WRITE_TO_FEED_PHASES` | 3 | the phases before a write's feed: receive the chunk, write, read back |
| `IDLE_LIMIT_US` | 30 s | the idle deadline, from the last feed or the arm |
| `FEED_TO_FEED_US` | 40.5 s | the worst gap between two feeds: `IDLE_LIMIT_US` plus `WRITE_TO_FEED_PHASES` times `TRANSFER_BUDGET_US` |
| `REFERENCE_WATCHDOG_PERIOD_US` | 44.739 s | the period of the reference watchdog's setting TOP 14: 2^30 cycles at 24 MHz |
| `HOST_BULK_TIMEOUT_US` | 5 s | the host's default bulk timeout the contract is sized against (one OUT transfer) |
| `INIT_BUDGET_US` | 10 s | the stage's init budget: from the arm to the device being visible. Every bring-up wait takes the init deadline `arm + INIT_BUDGET_US` |
| `FIRST_REQUEST_WINDOW_US` | 24 s | the host's first window, **anchored at send-end**: the first fed request must be completely sent within it. `IDLE_LIMIT_US - HOST_BULK_TIMEOUT_US - REPLY_MARGIN_US`, asserted |
| `REPLY_MARGIN_US` | 1 s | the margin a host adds to the stage's worst case: a stated allowance for its own scheduling, used in both derivations |
| `REPLY_READ_BOUND_US` | 20 s | the longest a host may take to read one reply, **anchored at the instant the host STARTED sending its request**: `HOST_BULK_TIMEOUT_US + ROUND_PHASES * TRANSFER_BUDGET_US + REPLY_MARGIN_US`. Asserted at least its worst case (`HOST_BULK_TIMEOUT_US + ROUND_PHASES * TRANSFER_BUDGET_US`, 19 s), at least 16 s, and above one OUT bound |

**The anchors, in one place.** Two anchors, and nothing else is used:

- **Send-end** (the host finished sending the stage to the boot ROM) anchors the **first window**. The stage's arm is at or
  after it, because the ROM runs the stage only after its last byte.
- **The start of the host's send of a request** anchors the **reply-read bound**.

**The first window, derived.** `FIRST_REQUEST_WINDOW_US` = `IDLE_LIMIT_US - HOST_BULK_TIMEOUT_US - REPLY_MARGIN_US` = 24 s. The
host's first fed request must be completely sent within it of send-end. The stage's first deadline is at or after
`send-end + IDLE_LIMIT_US`, so a request completed inside the window (with the request's OUT bound and the margin still to
spare) is in the stage before that deadline.

**The reply-read bound, derived.** `REPLY_READ_BOUND_US` = `HOST_BULK_TIMEOUT_US + ROUND_PHASES * TRANSFER_BUDGET_US +
REPLY_MARGIN_US` = 20 s, from the start of the host's send. The request's OUT completes within `HOST_BULK_TIMEOUT_US` (the
host's own bound; the stage does not bound the header, which waits for the host inside the idle deadline). The stage's round
then runs its `ROUND_PHASES` phases, each at most `TRANSFER_BUDGET_US`, and posts the reply. Control requests cannot extend a
phase (R4-B1). The host adds `REPLY_MARGIN_US`.

**Bench checks (not measured).** The gap from send-end to the arm (the ROM's jump and the stage's entry), and the bring-up to
visibility (at most `INIT_BUDGET_US` after the arm), are bench checks. So is whether enumeration fits in what is left before the
first idle deadline. Until they are measured, the first window and the init budget are assumptions the simulation supports and
the board does not yet show. The list is in `testing/board-runs/FL1.md`.

**The arithmetic, in one place.** A feed is recorded at the feed: after the write's read-back, or after a read's reply is
taken. It is not recorded after the status is sent. The watchdog must be fed again before its period from that instant.
The longest gap between two feeds: a request arrives just before the idle deadline, which is `IDLE_LIMIT_US` (30 s) after
the last feed; its write round then reaches its feed in its three phases before the feed, at most
`WRITE_TO_FEED_PHASES * TRANSFER_BUDGET_US` (10.5 s). That is `FEED_TO_FEED_US` = 40.5 s. A read round is shorter
(a storage read and a reply, 7 s), so it does not set the bound. The status after a feed is not in the gap: the next
idle deadline counts from the feed, so it runs during the idle window. The build checks `FEED_TO_FEED_US <
REFERENCE_WATCHDOG_PERIOD_US` at compile time (`timing.rs`), so a change that breaks the watchdog fails the build.

**The watchdog period** is the smallest the stage's watchdog offers that covers `SESSION_BUDGET_US` (which is
`FEED_TO_FEED_US`, 40.5 s). On the reference watchdog that is TOP 14, 44.739 s. The idle deadline (30 s) falls before the
watchdog's period (44.739 s) from the same feed, so the idle outcome comes first.

**Storage.** A storage phase is bounded by `TRANSFER_BUDGET_US` for the whole call, not for each block: 3.5 s for the
write of a whole chunk. Bench check: 64 KiB on a slow card must fit that budget. A call that cannot finish in it ends with
storage status 2, and the watchdog is not fed.

A storage write that is cut off by its budget is status 2 with the part already written left in place: sectors that fit
the budget are written, the rest are not, and nothing outside the range is touched (sim test
`a_write_cut_off_by_its_deadline_is_status_two_with_part_of_the_range_written_and_nothing_outside_it`).

Every bound is a deadline on the stage's clock. No wait spins without one.

## Feed and reply order

The stage feeds the watchdog after the work of a round is verified (for a write: the chunk's CRC matches, the region admits
it, and the read-back equals the payload; for a read: the reply has been taken by the link), and records the instant of
the feed at that point. The status is sent after. A lost status cannot hide a fault: the round was verified before it, and
a session that then ends parks and is reset. A fault inside the round is never fed.

## Regions, and the compare rule

The stage polices a card through a table of regions, published in its identity record. A region is a range of sectors
`first..end` (end exclusive) and a class:

- **plain**: a verified payload is enough.
- **boot area**: a write needs the compared mark.
- **header**: a write needs the compared mark too, and its payload verified first. The boot record (sector 64) is a
  header.

The Rockchip stage publishes two regions: the **boot area, sectors 64 to 32767** (the whole loader area before the uboot-env
partition at sector 32768), and the **header, sector 64**. The host plan's `loader_sectors` (64 in the old tests) is how
many sectors of loader image a plan writes; the stage protects all of the area, because it cannot know the image's length.

**One class per chunk.** A chunk must lie wholly inside one class: no start or end of any region may fall strictly inside
the write (`region::wholly_inside_one_class`). A chunk that crosses a boundary (a start or an end of any region, for
example 63 to 64, 64 to 65, or 32767 to 32768) is refused with status 5 (bad request), and nothing is written or fed. The
host splits its writes at the boundaries it reads from the region table in the identity record. A boundary exactly at the
first or the last sector of a write does not cross it: a write may start at 64 or end at 32768. A write is then classed by
its one class, so there is no straddle case and no straddle compare.

**Whole sectors.** A chunk is exactly `count * 512 + 2` bytes (the payload, then its CRC). Any other length is refused with
status 5. The stage never pads and never trims. A host with a partial last sector pads it itself (next paragraph).

**Padding.** A host pads a partial last sector with `PAD_BYTE` (`0x00`, `lemnos_flash_wire::frame::PAD_BYTE`). The reason:
the stage cannot tell a pad from image data, so it compares and writes the pad bytes as image bytes. A fixed pad byte makes
the same image compare equal whichever host wrote it. The stage does not enforce the pad byte; the host must use the
constant. The conformance scenarios pad nothing (every chunk is whole sectors); if one pads, it uses this constant.

**The compare rule.** The stage checks the payload's CRC and the region gating. It cannot know that the host compared
anything: the compared mark is the host's word. The host decides each write with `region::decide`, which is this rule, in
code. The write lies wholly inside one class (the host has split it at the boundaries):

1. **Plain write**: the host writes it without the mark. It compares nothing, and the stage does not ask it to.
2. **Boot-area or header write**: the host compares **every sector of the write**, byte for byte: the card's read of the range
   (`card`, `count * 512` bytes) against the image bytes the host means to write for the same range (`image`, `count * 512`
   bytes). The two lengths are the length of the write by construction. If the two are equal the decision is
   `Decision::Skip`: nothing is written and no mark is set. If any byte differs it is `Decision::Write { mark: true }`: the
   write is made with the mark.

The stage's own read-back of a write (after the write, before the feed) has two failures, with different statuses:

- the read itself fails in storage: status 2 (storage), and the status says the range may be partly written;
- the read succeeds and any byte differs from the payload: status 3 (verify), and the range may be partly written.

In both cases nothing is fed. Nothing is written before a status 1 or 5 refusal: those refusals come before any storage call.

The baseline is the host's image, never the card. A read of the card is never a baseline for itself.

**Validity.** A comparison is valid until the next write or the next reset, whichever comes first. A write changes the
card, so every comparison before it is stale; the host reads and compares again before its next boot-area or header write.

Tests: `region.rs` has one per decision (plain, boot-area equal, boot-area differs, header equal, header differs, a
multi-sector write with one differing byte) and one per boundary: a write crossing the start of the boot area (63 to 64),
starting at 64, crossing the header end (64 to 66), starting at the header end (65), crossing the boot-area end (32767 to
32768), ending at the boot-area end (32767), and starting at it (32768). `serve.rs` has the stage's refusals for the start and
the end boundaries, for a chunk that is not whole sectors, for a bad CRC (status 1), for a read-back that fails in storage
(status 2), for a read-back that differs (status 3), for 128 sectors served and 129 refused (status 5), and for a write cut off
by its budget (status 2, with the range partly written). Every refusal test checks that the only effect is the status: no
storage call and no feed.

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
| wire hash | `0x642D_1B44_FB2E_5496` |
| wire short | `53457` |
| frame family (1) | `0x274F_4492_2B2A_47C4` |
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

## Host requirements

A host that talks to the stage must:

1. **Send each frame as exactly one transfer of exactly its length.** A request is 12 bytes; a chunk is `count * 512 + 2`
   bytes, a whole number of sectors (padded with `PAD_BYTE`). A longer transfer is a host error. The stage's behaviour for a
   longer transfer is **undefined** until the drain of the surplus is shown on the bench: the surplus may be read as the next
   request's header. **Open:** this is not closed by any test in this revision (`testing/board-runs/FL1.md`, check 12).
2. **Read each reply with a buffer for the largest reply its request allows**, and accept only the two lengths in the table
   above (the full reply with status 0, or exactly one byte). An identify reply is 104 bytes (`IDENTITY_LEN`). Any other length
   is a failure on the host's side.
3. **Stop the session on an unexpected status or length.** After an unexpected status 1 (crc), 4 (not compared) or 5 (bad
   request), or a reply of an unexpected length, the host stops that session. It does not reissue the request blindly, and
   it sends nothing more on that session. **What the stop does in the stage:** releasing the interface does nothing to the
   stage. A port reset makes the stage re-enumerate (`session::run`), and that enumeration shares the unchanged idle
   deadline, so it does not get a fresh limit. If the host does not configure the device before that deadline, the session
   ends `Enumerate(NotConfigured)`, and the watchdog then resets the stage, which is a normal reset and not the maskrom path.
   A surplus transfer may still be in the stage's buffers when the host stops. The host re-verifies the card before it
   starts again from enumeration.
4. **Post its IN read within `TRANSFER_BUDGET_US` (3.5 s)** of sending a request, and read each reply within
   `REPLY_READ_BOUND_US` (20 s), counted from the instant it **started sending** the request.
5. **Meet the timing contract.** The first window: the host's first fed request must be completely sent within
   `FIRST_REQUEST_WINDOW_US` (24 s) of **send-end** (the instant it finished sending the stage to the boot ROM). Thereafter the
   stage's idle deadline is 30 s after its last feed. The host counts its own idle window
   from the **send time of the last request the stage fed** (a verified write, or a read that was sent), which is never
   later than the stage's feed, so the host's window ends first. The feed-to-feed bound is 40.5 s.
6. **Treat a write that did not complete as undefined.** A write round's status 2 or 3 may leave part of the range written,
   and nothing outside the range. After status 2 or 3 the host re-reads or rewrites the range within the idle budget. A
   piece whose compare read failed was never written: it is **unverified**, not left undefined, and the host reports it so.

What bounds the stage when a host breaks these: the stage ends the session at the idle deadline, **at most 30 s after the
last feed**, with the idle outcome. If the stage cannot finish, the watchdog resets it, **at most 44.7 s after the last
feed**. Neither outcome needs the host's cooperation; the host re-verifies the card after either.

## A conforming host, step by step

This is the sequence a host follows against the stage. Each line is a rule the stage relies on; the PC side implements the
same rule in code, and a line that disagrees with the code is a bug in the code or in this list.

1. **Identity.** Read the identity record, and run `identity::check` on it. A `Skew` other than none stops the host.
   The host's regions are `Peer::regions()` from that check, not a copy of its own table.
2. **Split.** Split each write at the start and the end of every region that lies strictly inside it, and at 128
   sectors. A boundary at the first or last sector of a write does not split it. Reads are split at 128 sectors only.
3. **Pad.** A partial last sector is padded with `PAD_BYTE` (`0x00`). The stage does not pad or trim.
4. **Plain pieces.** A piece in plain data is written with no mark. No read and no compare.
5. **Boot-area and header pieces.** The host reads the card's range, compares it with the image for the same range, and
   decides with `region::decide`. Equal is `Decision::Skip`: nothing is sent. Different is written with the compared mark
   set. The read is repeated before each piece; a comparison is valid only until the next write.
6. **Read failure.** If the host's read of the card fails, it writes nothing for that piece and stops (rule 3). The piece
   is reported as **unverified**: it was never written, so it is neither written nor left undefined.
7. **One transfer per frame.** The request is one bulk-out of 12 bytes; a chunk is one bulk-out of its exact length.
8. **Reply read.** The IN read is posted at once after the request, within `TRANSFER_BUDGET_US`, and completes within
   `REPLY_READ_BOUND_US` (20 s), counted from the instant the request's send **started**.
9. **Idle.** The host's idle window runs from the send time of the last fed request, and is at most 30 s. Its first window
   runs from send-end (rule 5), not from first seeing the device.
10. **Stops.** On status 1, 4 or 5, or a reply of an unexpected length, the host stops the session (rule 3). Nothing was
    written for those statuses. On status 2 or 3 the range may be partly written: the host treats it as undefined and
    re-reads it (rule 6).

**Negotiated speed.** After the host configures the device, the stage reads the negotiated speed from the controller.
Anything but high speed undoes the configuration and the enumeration fails (`EnumError::NotHighSpeed`): the device does not
serve on a full-speed link. Bench check: the controller's speed field (DWC3's DSTS.CONNECTSPD, 0 is high speed).

## Bench checks

Everything in this document's timing was checked in simulation, on a simulated clock. **The timing has never been measured on a
board.** Each of the following is a bench check and is listed as a step in `testing/board-runs/FL1.md`:

- the init budget (`INIT_BUDGET_US`: arm to visible), and enumeration fitting in what is left before the first idle deadline;
- the send-end to arm gap (the ROM's jump and the stage's entry), which the first window's anchor assumes is non-negative;
- the controller command times, each against its phase deadline;
- address-before-status (`Controller::address_before_status`, the Rockchip controller's answer);
- the speed register's encoding (DSTS.CONNECTSPD, 0 is high speed);
- the stop procedure (release, port reset, re-enumeration under the unchanged deadline, then a normal watchdog reset);
- the storage budget: a 64 KiB write and its read-back on a slow card must fit `TRANSFER_BUDGET_US`.

**Open:** the surplus bytes of an overlong transfer. The stage's behaviour is undefined until the drain's semantics are shown on
the bench, and no test here closes it.

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
- `src/region.rs`: the classifier, one test per case of the compare rule, a length and a straddle refusal of `decide`.
- `src/timing.rs`: the reply-read bound and the first window, each derived from its constants and checked at compile time, and the
  init budget.
- `src/identity.rs`: the identity record's length, 104 bytes, checked at compile time.
- `src/timing.rs`: the watchdog bound, checked at compile time.
- `src/layout.rs`: the pinned hashes.
- `src/hash.rs`: the shared FNV vectors (also checked in `engine/wire`).
