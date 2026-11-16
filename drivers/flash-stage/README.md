# lemnos-flash-stage

The rules every on-chip flash stage shares, for a chip's stage to build on. no_std, no `unsafe`. The wire is
`lemnos-flash-wire`, whose README is the contract a host follows: the frames, the status bytes, the session rules,
the timing and the compare rule. This README is about the stage side of that contract. Nothing here names a chip.

## Pinning

**What a git pin brings.** A git pin of this crate reads the repository's workspace manifest (the root `Cargo.toml`) for the inherited package fields: version, edition, license, repository and homepage, and `rust-version` (the minimum Rust version, 1.94). The lint table is inherited the same way. The wire crate has no dependencies of its own; the stage crate depends only on the wire crate, by path in this repository. Edition 2024 and rust-version 1.94 are required, as the workspace states them.

## What is shared

- **Entry contract** (`guard`): the stage arms the watchdog before anything, reads the arming back, then lowers the
  period to the smallest one that covers the session budget (`guard::start`). A watchdog that does not read back as
  armed stops the stage before it runs.
- **Cause before reset** (`guard::finish`): a session that ends in a failure hands its `Cause` to a `Report`, then
  parks. The park never feeds the watchdog. A chip with no channel that survives a reset implements `Report` as a
  no-op and says so in its own record.
- **Bounded waits** (`wait`): every wait has a deadline. A budget at or above the longest period is refused before any
  poll.
- **Feed rule** (`chunk`): the watchdog is fed once per verified round and only then. A write round is verified when
  its CRC matches, the region admits it, and the read-back equals the payload; the feed comes after that, and the
  status reply after the feed. A read round feeds after its reply has been taken by the link. A refused, cut, or
  failed round is not fed. A lost reply cannot hide a fault: the round was verified first, and a session that then
  ends parks and is reset.
- **Serve loop** (`serve`): one request per step, over a `Link`. What it does with each failure is the wire's session
  rule: a refused request (bad CRC, bad length, unknown op, reserved bit, a chunk of the wrong length) gets its
  status and the session goes on; a storage failure gets its status and the session goes on; an END ends it with its
  reset kind; an idle host ends it at the idle limit; a bus reset ends it with `IoError::Reset` so the device side
  enumerates again; a cut ends it with its timeout (the only silent end).
- **Device side** (`usb`): the control endpoint's standard requests, the descriptors, the enumeration loop, and the bulk
  pair as a `Link`. A chip implements `usb::Controller` (poll, set the address and the configuration, the control
  packets, stalls, and the bulk transfers), and nothing else. The control endpoint's state is `Ep0`. Its descriptors
  are built from the wire's USB identity, so the host and the device read the same numbers.

## The order rules, enforced by the stage

These are new in the stage and do not depend on the PC side doing the right thing.

1. **A write needs a verified payload** (`region::verify`, `region::gate`). The storage write is given the payload of
   an `Admitted` value, which only `gate` builds, and `gate` takes only a `Verified` value, which only `verify` builds.
   A chunk whose CRC does not match is never admitted, so a header or boot-record write with a bad payload cannot reach
   storage.
2. **A boot area or a header needs the compared mark.** `region::classify` returns the strictest class of any region a
   write overlaps, so a straddling write is a boot-area write. Without the reserved bit (`RESERVED_COMPARED`) a boot-area
   or header write is refused with status 4 and nothing is written.

The stage checks the payload's CRC and the gating. It does not check that the host compared anything: the mark is the
host's word, and the compare rule is in the wire's README.

## Storage state

A write that fails in storage (status 2) or fails its read-back (status 3) may leave the range partly written. The
stage does not roll it back: it has no copy to roll back to. The status says so, and the host must read the range back
and verify it before it relies on it. A read that fails (status 2 alone) changes nothing.

## Simulated device, and the conformance list

`sim` (feature `sim`) is a card, a queue link and the real serve step, with the frame-level `sim::Transport` a host
implements over its own link.

- `Transport::send_frame` sends one frame (one transfer). `Transport::take_reply` runs the stage as far as it needs to
  and returns the next reply, or `None` when the stage has nothing more to say until the host sends again. A host can
  act on each reply before it asks for the next. `Transport::feeds` reports the watchdog's count where the transport can
  see it (the simulated device can; a board cannot).
- `sim::SCENARIOS` is the conformance list: 17 scenarios, each run with `sim::run(&scenario, &mut transport)` against a
  fresh stage. A scenario reads the stage's identity record for its sectors, so the list follows the stage's regions.
  The list is: identify (with a build id); read (with its CRC, one feed); a plain write (stored, one feed); a write with
  a bad CRC (status 1, nothing stored, no feed); a cut before the payload (a stop); a boot-area write without the mark
  (status 4); a boot-area write with the mark (stored); a header write with a bad CRC and the mark (status 1, nothing
  stored, no feed); a header write without the mark (status 4); a header write with the mark (stored); a straddle
  without the mark (status 4); a chunk of the wrong length (status 5, the session goes on); END with maskrom (ok, then
  `Ended(Maskrom)`); a request with a bad CRC (status 1, the session goes on); a reserved bit (status 5, the session goes
  on); an empty queue (idle, not a stop); and the published regions (a digest that is not zero).
- `sim::model` is a scripted device controller. It drives the same `usb` code the chip runs, for the enumeration and
  bulk tests: a full enumeration, the descriptors, a host that never configures, a bus reset during enumeration, a
  disconnect, a control request that is stalled, a control request in the middle of a bulk session, a bus reset and an
  unconfiguration while a frame waits.

## What a chip's stage must still show on its own

- The `Guard` over its watchdog registers, with a read-back of the arming.
- The `usb::Controller` over its device controller, with every wait bounded on its own clock and its cache maintenance.
  Its own tests must show the controller's register sequences (the event buffer, the control endpoints, the transfer
  resources before the transfers, the device address write) and its event decoding.
- The `Link` (the device side provides it; a chip supplies only the controller) and the `Storage` over its controller.
- Its region table, from the card's layout, and its build id.
- Its `Report` (or the no-op, with the reason).
- Board runs. Nothing in this crate has run on a board.
