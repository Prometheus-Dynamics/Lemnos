# lemnos-flash-stage

The rules every on-chip flash stage shares, for a chip's stage to build on. no_std, no `unsafe`. The
wire is `lemnos-flash-wire`. Nothing here names a chip.

## What is shared

- **Entry contract** (`guard`): the stage arms the watchdog before anything, reads the arming back, then
  lowers the period to the smallest one that covers the session budget (`guard::start`). A stage that
  reads back "not armed" does not run.
- **Cause before reset** (`guard::finish`): a session that ends in a failure hands its `Cause` to a
  `Report` before it parks. The park never feeds the watchdog. A chip with no channel that survives a
  reset implements `Report` as a no-op and records that in its own README.
- **Bounded waits** (`wait`): every wait has a deadline, and a budget at or above the longest period is
  refused before any poll.
- **Feed rule** (`chunk`, `serve`): the watchdog is fed only after a verified round: a verified write
  (stored, read back, equal, acknowledged) or a read reply that was handed to the link. A refused, cut or
  failed round is not fed.
- **Serve loop** (`serve`): one request at a time over a `Link`. A bad CRC is refused with its status and
  nothing is written; a cut or a bad frame ends the loop; an identify is answered with the identity
  record; an END replies its status and returns the reset kind.

## The order rules, enforced by the stage

These are new in this crate. They do not depend on the PC side doing the right thing.

1. **A write needs a verified payload** (`region::verify`, `region::gate`). Every write is given an
   `Admitted` value, which only `gate` builds, and the storage write is given only the payload of an
   `Admitted` value. `gate` takes only a `Verified` value, which only `verify` builds. A chunk whose CRC does not match is never admitted, so a header or boot-record write
   with a bad payload cannot reach storage. Tests: `region::tests`, and the scenario "a header write with a
   bad CRC is refused before any write even with the mark" in `sim`.
2. **A boot area or a header needs the compared mark.** `region::classify` returns the strictest class of
   any region a write overlaps, so a write that straddles into the boot area is a boot-area write. A
   boot-area or header write without the reserved bit (`RESERVED_COMPARED`) is refused with status 4
   (`STATUS_NOT_COMPARED`) and nothing is written. Tests: `region::tests`, and the scenarios "boot area
   without the mark", "header without the mark", "straddle needs the mark" in `sim`.

Structurally, the payload check is the chunk's CRC-16 (the wire's only payload check). A stronger digest
would be an additive wire field and is not in this crate.

## Simulated device

`sim` (feature `sim`, and always in this crate's tests) is a card, a link queue and the real serve loop.
A PC side can run its host against it: `Device::send` queues a frame, `Device::run` serves until an END,
a stop, or an empty queue, `Device::replies` returns what the stage sent, and `Device::card` and
`Device::feeds` show the storage and the watchdog. The scenarios in `sim::scenarios` are the conformance
list a host must pass against the stage's behaviour:

- identify answers this build's identity and feeds nothing
- read returns the sectors with their CRC and feeds once
- a plain write stores the verified chunk and feeds once
- a write with a bad CRC: status 1, nothing stored, no feed
- a cut before the payload stops the session, nothing stored, no feed
- a boot-area write without the mark: status 4, nothing stored
- a boot-area write with the mark is stored
- a header write with a bad CRC is refused before any write, even with the mark
- a header write without the mark: status 4
- a header write with the mark and a good CRC is stored
- a write that straddles into the boot area needs the mark
- a chunk that is not the length its request makes it stops the session, nothing stored
- END with the maskrom reset returns maskrom after an OK status
- a request with a bad CRC ends the session with a CRC frame error
- a reserved bit a read does not define ends the session
- an empty queue is idle, not a stop

## What a chip's stage must still show on its own

- The `Guard` over its watchdog registers, with a read-back of the arming.
- The `Link` and `Storage` over its controllers, with its own bounded waits and cache maintenance.
- Its region table (the boot area, the headers), from the card's layout.
- Its own timing (`wait::Clock`), and its `Report` (or the no-op, with the reason).
- Board runs: nothing here has run on a board.
