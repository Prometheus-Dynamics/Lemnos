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
- **Session loop** (`session::run`): from reset, enumerate the device; serve the bulk pair until an END; on a bus reset
  or an unconfiguration during the session, enumerate again and serve again. It runs on **one clock**: the idle deadline
  is `IDLE_LIMIT_US` after the last watchdog feed, or after the **arm**: the instant the chip arms the watchdog, taken before
  its own DWC3 and SD bring-up and passed in as `Setup::armed_at_us`. The loop records the instant of each feed (`Watchdog::feed_at`,
  read at the feed, before the status is sent) and moves the deadline only on a feed. Enumeration, before the first request and after every bus reset, shares that deadline, so a
  reset grants no fresh limit. It returns the END's reset kind, or why it stopped: `SessionStop::Enumerate` (no
  configuration before the deadline), or `SessionStop::Serve(ServeError::Idle)` (no request before the deadline; a
  distinct outcome from a link timeout), or `SessionStop::Serve(Link(..))` (a link that failed for good). The chip's entry
  calls this one function; the tests drive the same function through the model controller from reset. Enumeration can also
  end with `EnumError::NotHighSpeed`: the host configured the device on a link slower than high speed, and the configuration
  was undone.
- **Address timing** is the controller's decision: `Controller::address_before_status` says whether `set_address` runs
  before or after the status stage of SET_ADDRESS. The shared code does not assume either. Each chip records its
  controller's behaviour as a bench check.
- **Device side** (`usb`): the control endpoint's standard requests, the descriptors, the enumeration loop, and the bulk
  pair as a `Link`. `BulkLink::recv` takes a completed frame first, then checks the deadline, then services one control
  event, so a control request that lands near the deadline does not drop the frame behind it. A chip implements `usb::Controller` (poll, set the address and the configuration, the control
  packets, stalls, and the bulk transfers), and nothing else. The control endpoint's state is `Ep0`. Its descriptors
  are built from the wire's USB identity, so the host and the device read the same numbers.

## The order rules, enforced by the stage

These are new in the stage and do not depend on the PC side doing the right thing.

1. **A write needs a verified payload** (`region::verify`, `region::gate`). The storage write is given the payload of
   an `Admitted` value, which only `gate` builds, and `gate` takes only a `Verified` value, which only `verify` builds.
   A chunk whose CRC does not match is never admitted, so a header or boot-record write with a bad payload cannot reach
   storage.
2. **A chunk lies wholly inside one class, and a boot area or a header needs the compared mark.** A chunk that crosses a
   region boundary (a start or an end of any region strictly inside it) is refused with status 5 before anything is written
   (`region::wholly_inside_one_class`). A chunk inside one class is classed by `region::classify`; without the reserved bit
   (`RESERVED_COMPARED`) a boot-area or header write is refused with status 4 and nothing is written.

The stage checks the payload's CRC and the gating. It does not check that the host compared anything: the mark is the
host's word, and the compare rule is in the wire's README.

## Storage state

A write may leave part of its range written, and nothing outside the range is touched. Two cases:

- **Status 2 (storage)**: the storage call failed, or ran past its budget, or the read-back of the write failed in storage.
  Sectors that were written before the failure stay written; the rest are not. A write cut off by its deadline is the
  same case: the sectors that fit the budget are written, the others are not. The simulated card decides whether each
  sector fits before it writes it, so a sector torn in the middle cannot appear in the simulation; a real card can tear
  one, and the read-back is what shows it (sim test
  `a_write_cut_off_by_its_deadline_is_status_two_with_part_of_the_range_written_and_nothing_outside_it`).
- **Status 3 (verify)**: the read-back succeeded and differs from the payload. The range may be partly written.

The stage does not roll either back: it has no copy to roll back to. After either status the host must read the range
back and verify it before it relies on it. **Nothing is written** only for the refusals that come before any storage call:
status 1 (crc) and status 5 (bad request). A read that fails (status 2 alone) changes nothing.

**The storage budget.** Each storage phase has `TRANSFER_BUDGET_US` (3.5 s) for the whole call, a 64 KiB write included: the
chip's storage checks its deadline across the sector loop and inside every device wait, and a call that cannot finish in its
budget ends with a timeout, which the round reports as storage status 2. Nothing is fed. Bench check: a 64 KiB write on the
real card must fit that budget; the simulated card has a slow mode (`Card::slow`) that exercises the same path.

## Simulated device, and the conformance list

`sim` (feature `sim`) is a stage **from reset**: the device side over a scripted controller (`sim::model`), the stage's
real serve step, and a sparse card. **The simulated device starts at reset.** Every scenario starts with
`Transport::connect`, which runs the host's enumeration (descriptors, SET_ADDRESS, SET_CONFIGURATION) before the
scenario's first request. So a host that passes the list has also enumerated the device. A board transport performs the
same enumeration over its own USB stack. The frame-level `sim::Transport` is what a host implements over its link.

- `Transport::connect` runs the host's enumeration from reset. It is required: a transport that does not enumerate
  its device cannot run the suite.
- `Transport::send_frame` sends one frame (one transfer). `Transport::take_reply` runs the stage as far as it needs to
  and returns the next reply, or `None` when the stage has nothing more to say until the host sends again. A host can
  act on each reply before it asks for the next. `Transport::feeds` reports the watchdog's count where the transport can
  see it (the simulated device can; a board cannot).
- `sim::SCENARIOS` is the conformance list: 17 scenarios, each run with `sim::run(&scenario, &mut transport,
  ScratchCard::acknowledge())`, which connects first, against a fresh stage. The list is public behind the `sim` feature,
  so another crate can call it.
- **Scratch card only.** The suite writes real sectors: the header at sector 64 (`header_with_mark_stored`), a boot-area
  sector at 100 (`boot_area_with_mark_stored`), and plain sectors past the boot area (the write at the region end,
  32768). Running it against any transport takes a `ScratchCard`, which only `ScratchCard::acknowledge()` makes: the
  caller states the card is a scratch card. It is a typed argument, not an environment variable, so a run on a card that
  matters does not compile without that statement.
- **A check that could not be made is not a pass.** When the transport cannot count feeds, the feed checks are recorded as
  not checked, and `run` returns `Verdict::NotChecked(names)`, never `Verdict::Passed`. A transport failure is
  `TransportError::Transport(message)`, a host-side fault and not a verdict on the stage. A scenario reads the stage's identity record for its sectors, so the list follows the stage's regions.
  The list is: identify (with a build id); read (with its CRC, one feed); a plain write (stored, one feed); a write with
  a bad CRC (status 1, nothing stored, no feed); a cut before the payload (a stop); a boot-area write without the mark
  (status 4); a boot-area write with the mark (stored); a header write with a bad CRC and the mark (status 1, nothing
  stored, no feed); a header write without the mark (status 4); a header write with the mark (stored); a chunk crossing a boundary
  (status 5, nothing stored); a chunk of the wrong length (status 5, the session goes on); END with maskrom (ok, then
  `Ended(Maskrom)`); a request with a bad CRC (status 1, the session goes on); a reserved bit (status 5, the session goes
  on); an empty queue (idle, not a stop); and the published regions (a digest that is not zero).
- `sim::model` is a scripted device controller: an ordered script of control events and bulk frames, so a reset can
  land between two requests. It starts at reset. It drives the same `usb` and `session` code the chip runs. Its tests
  cover the enumeration, the descriptors, a host that never configures, a bus reset during enumeration, a disconnect, a
  stalled control request, a control request in a session, a reset and an unconfiguration while a frame waits.
- `session` tests run the whole session from reset through the model: (a) a cold start (enumerate, then the first request
  answered); (b) a bus reset between two requests (re-enumerate, serve again); (c) a host that never configures (the
  enumeration limit is the stop, and nothing is fed); (d) an unsupported control request (stalled; the session goes on);
  plus a reset with a chunk in flight (the chunk is not written) and an idle host (the idle limit is the stop).
  The clock is exact: each controller pass takes 1 ms (`sim::model::SimClock`), and the tests assert the instants the
  code reaches, not a tolerance. They cover the first deadline from the arm, a bus reset at 25 s and at 20 s, an identify
  every 29 s, refusals that leave the deadline unmoved, a write that moves it, a control request one pass before the
  deadline (the frame behind it is still served), a request armed one second before the deadline whose command takes one
  second (not accepted) and half a second (accepted), a slow card (status 2), and a write cut off by its budget (status 2,
  part written).
  Rev 5 (R4-B1): a GET_STATUS, a SET_CONFIGURATION and an unsupported request (a stall) that start before the deadline and run
  past it with slow controller commands (`Costs`) are timeouts at the deadline, with nothing completed and no frame taken
  behind them (`a_control_request_started_before_the_deadline_that_runs_past_it_...`,
  `a_set_configuration_that_runs_past_the_deadline_...`, `an_unsupported_request_whose_stall_runs_past_the_deadline_...`);
  a slow control request that finishes in time does not reject the frame behind it.

## What a chip's stage must still show on its own

- The `Guard` over its watchdog registers, with a read-back of the arming.
- The `Controller::address_before_status` answer, with its controller source cited, and marked as a bench check.
- The end-transfer on a bus reset (`io::end_transfer`): the resource id is zero; a bench check.
- The surplus question: a host's frame longer than its receive transfer leaves its surplus on the endpoint, where the
  next header receive would read it. **Not closed.** The exact-length receive does not expose the surplus on the DWC3
  path; the stage refuses nothing it cannot see. The fix is a drain receive whose semantics must be shown on the bench,
  and it is not in this revision.
- The `usb::Controller` over its device controller, with every wait bounded on its own clock and its cache maintenance.
  Its own tests must show the controller's register sequences (the event buffer, the control endpoints, the transfer
  resources before the transfers, the device address write) and its event decoding.
- The `Link` (the device side provides it; a chip supplies only the controller) and the `Storage` over its controller.
- Its region table, from the card's layout, and its build id.
- Its `Report` (or the no-op, with the reason).
- Board runs. Nothing in this crate has run on a board.

## Waits and deadlines

Every wait in the stage, with the deadline it takes. Rule R4-B1: nothing a session phase calls may wait past the phase's
deadline, and no fixed wait is reachable under a session phase. The table is the audit; a new wait must be added to it.

| Wait (where) | Deadline it takes | Phase |
|---|---|---|
| `session::run`, idle limit | `last feed + IDLE_LIMIT_US`, or the arm for the first | session |
| `serve::step` header receive (`link.recv(header, header_budget)`) | the idle deadline's remaining time, `deadline - now` (`session.rs`): the header waits for the host inside the idle window, not for a transfer budget | idle |
| Header receive re-arm (`usb::BulkLink::recv`, first statement) | the same receive deadline; armed before any wait, so before the host can send its next request | receive |
| `usb::enumerate` → `Ep0::handle` and `Controller::poll` (the enumeration, before and after a bus reset) | the session's idle deadline, passed in by `session::run` | enumeration |
| `serve` identify reply (`link.send(ctx.identity, …)`) and END status (`link.send(&[STATUS_OK], …)`) | `TRANSFER_BUDGET_US` at the call: 104 bytes for the identify reply | send |
| `usb::BulkLink::recv` (receive) | `now + budget` at entry, the chunk's `TRANSFER_BUDGET_US` | receive |
| `usb::BulkLink::recv` → `Controller::bulk_arm`, `bulk_poll`, `poll` | that receive deadline | receive |
| `usb::Ep0::handle` (every control reply, stall, SET_ADDRESS, SET_CONFIGURATION) | the receive deadline of the phase that got the SETUP | receive |
| `usb::enumerate` (the enumeration's own wait and its controller commands) | the session's idle deadline | enumeration |
| `chunk::write_round` storage write and read-back, `chunk::read_round` storage read | `TRANSFER_BUDGET_US` at the call (`Storage` budget) | storage |
| `chunk::write_round`, `chunk::read_round`, `serve` status sends (`link.send`) | `TRANSFER_BUDGET_US` at the call | send |
| Init: `sd::sd_init` ACMD41 loop (`ACMD41_POLLS` = 1000 iterations, two commands each) | count-bounded, and each command takes the init deadline, so the loop ends by `INIT_BUDGET_US` at the latest | init |
| `wait::wait_for` (every bounded poll) | the budget or deadline its caller passes; a budget at or above the longest period is refused | any |
| Rockchip `Dwc3Dev::poll` → `arm_setup` (SETUP re-arm) | the poll's deadline | receive |
| Rockchip `Dwc3Dev::poll` → USBRST `end_transfer` (BULK_OUT, BULK_IN) | the poll's deadline | bus reset |
| Rockchip `Dwc3Dev::bulk_arm` → `issue` → `start_transfer` | the arm's deadline (the receive's) | receive |
| Rockchip `Dwc3Dev::await_trb` (control data, control status, bulk IN) | the caller's deadline (control) or the send's phase deadline (bulk IN) | control / send |
| Rockchip `Dwc3Dev::control_in`, `status` (`ep0_in`, `ep0_in_status`, `ep0_out_status`) | the deadline the caller passes (the receive's) | control |
| Rockchip `Dwc3Dev::ep0_stall` → `set_stall` ×2 | the deadline the caller passes | control |
| Rockchip `Dwc3Dev::set_configured(true)` → `configure_endpoint` ×2 (BULK_OUT, BULK_IN) | the deadline the caller passes | control |
| Rockchip `Dwc3Dev::bulk_send` (`phase_deadline(budget)`) | the send's own budget, from the call | send |
| Rockchip `SdStorage::write` / `read` → `sd_write_block` / `sd_read_block` → `wait_not_busy`, `wait_int` | the storage budget, from the call (`dwc3link.rs`) | storage |
| Rockchip `end_transfer` (USBRST) and `wait_depcmd_until` | as their callers pass | bus reset |
| **Init, pre-visibility (after the arm, before the device runs):** `dwc3_start_device` (`wait_clear` ×2) | `arm + INIT_BUDGET_US` (`entry472::session`) | init |
| Init: `sd_init` → `dw_mmc_reset` (`wait_clear` ×2) and every `sd_command_response` | the same init deadline | init |
| Init: `Dwc3Dev::start` → `start_control_endpoints`, `configure_endpoint` ×2 (EP0) | the same init deadline | init |
| `guard::start` (arm, read-back, period) | no wait: it is an instant read and a register write | pre-arm |

**Constants that bound a wait and are not timeouts:** `LONGEST_PERIOD_US` (`wdt.rs`) is the longest period the
`wait_for` poll accepts, not a deadline. `HOST_BULK_TIMEOUT_US` is the host's bound, not a stage wait.

**Removed in this revision:** `io::IO_WAIT_US` (the fixed 1 s command wait), `io::wait_trb` (no caller outside its test),
`bind::REG_WAIT_US` (the fixed 1 s register wait), and the endpoint helper's own 1 s wait. Each is now the caller's deadline.

**The fixed-wait check.** `grep -n "IO_WAIT\|REG_WAIT\|wait_trb\|now_us().saturating_add\|phase_deadline(" drivers/*/src/*.rs`
shows only the init deadline, the phase budgets at a call (`phase_deadline` in `bulk_send` and `SdStorage`), and the
`wait_for` period bound. No control transfer or endpoint command takes a fresh deadline.

**Sim costs.** `sim::model::Costs` charges time to each controller operation against the deadline it is given: control
data and status (`control_us`), endpoint configuration (`configure_us`), stall (`stall_us`), SET_ADDRESS (`address_us`) and
the bulk arm (`command_us`). A cost past the deadline advances the clock to it and is a timeout. The trait has no
end-transfer operation: the Rockchip controller ends its bulk transfers inside its own bus-reset handling, so the simulation
has no equivalent cost. `Step::Costs` changes the costs part-way through a script.

## API changes in this revision

Every change a caller of the stage sees, rev 4 and rev 5 together, so a chip's stage and the host's code can be checked
against it:

- `session::Setup` gains `armed_at_us` (rev 4): the instant the watchdog was armed, on the session's clock.
- `guard::Watchdog::feed_at(now_us)` (rev 4): new, with a default of `feed`. The session records each feed through it.
- `usb::Controller` (rev 5): every waiting method takes the caller's `deadline_us` and returns `UsbError::Timeout` if it cannot
  finish by it. Changed: `set_address(address, deadline_us)`, `set_configured(on, deadline_us)`, `ep0_in(data, deadline_us)`,
  `ep0_in_status(deadline_us)`, `ep0_out_status(deadline_us)`, `ep0_stall(deadline_us)` (now returns `Result`).
  `poll(deadline_us)` and `bulk_arm(len, deadline_us)` (rev 4) take the deadline too.
- `usb::Ep0::handle(ctrl, bytes, deadline_us)` (rev 5): takes the phase deadline; `data_in` takes it too.
- `usb::BulkLink::recv` (rev 5): a frame is accepted only if its observed completion is at or before the deadline; the
  completion is observed at the take, and that instant is compared.
- `usb::enumerate(ctrl, clock, ep0, deadline_us)` (rev 4) takes the deadline; a control command that times out there is
  `EnumError::NotConfigured`. `enumerate_from_now` starts one at the clock's reading (sim and tests only).
- `serve::ServeError::Idle` (rev 4): the distinct idle outcome, separate from a link timeout.
- `region::decide(regions, lba, count, card, image)` (rev 4) returns `Result<Decision, DecideError>`; `Length` and
  `Straddle` are checked in every build.
- Storage budget (rev 3, stated here): each storage call takes `TRANSFER_BUDGET_US` for the whole call, not per block.
- Status 5 for a boundary (rev 3): a chunk that crosses a region boundary is refused with status 5, not 4.
- Wire constants (rev 4, 5 and 6): `REPLY_READ_BOUND_US` (20 s, anchored at the start of the host's send, derived from
  `HOST_BULK_TIMEOUT_US`, `ROUND_PHASES * TRANSFER_BUDGET_US` and `REPLY_MARGIN_US`), `FIRST_REQUEST_WINDOW_US` (24 s, anchored at
  send-end), `INIT_BUDGET_US` (10 s), `REPLY_MARGIN_US` (1 s). `REPLY_PICKUP_US` is removed in rev 6. All are in the frame hash.
- Rockchip stage (`lemnos-rockchip-maskrom-stage`, not a consumer crate): `io::configure_endpoint`, `io::configure_bulk_endpoint`,
  `io::set_stall`, `io::start_control_endpoints` take a deadline; `io::start_transfer`, `io::end_transfer`,
  `io::sd_read_block`, `io::sd_write_block`, `io::sd_command_response` take a deadline; `io::until` is public.
  `bind::dwc3_start_device`, `bind::dw_mmc_reset`, `sd::sd_init` and `usb::Dwc3Dev::start` take the init deadline.
  Removed: `io::IO_WAIT_US`, `io::wait_trb`, `bind::REG_WAIT_US`.

### Rev 6 (the landing rev): what changed in the API and the behaviour

- `usb::Event::Fault` (new variant): the controller could not finish a reset it had to make (a bus reset's end-transfer
  timed out). The receive ends with a link failure (`IoError::Failed`); enumeration ends with `EnumError::Controller(Failed)`.
  The Rockchip reset handler returns it instead of discarding the result (R5-F5).
- `usb::Ep0::handle`: SET_CONFIGURATION records the configuration only after the controller has made it (R5-F6). A repeated
  SET_CONFIGURATION to the state the device already has is answered with its status stage and nothing else (R5-F7).
- `wire::timing`: `REPLY_PICKUP_US` is removed. `REPLY_READ_BOUND_US` is `HOST_BULK_TIMEOUT_US + ROUND_PHASES * TRANSFER_BUDGET_US
  + REPLY_MARGIN_US` (20 s), anchored at the start of the host's send. `FIRST_REQUEST_WINDOW_US` is `IDLE_LIMIT_US -
  HOST_BULK_TIMEOUT_US - REPLY_MARGIN_US` (24 s), anchored at send-end. The frame hash moved.

## Phase deadlines, stated once

The idle deadline caps the header receive. A phase after a header is not capped at it: a chunk's phase deadline is its own
start plus `TRANSFER_BUDGET_US`, so a chunk that never arrives ends the session at up to 3.5 s past the idle deadline, as
`Link(Timeout)` and not `Idle` (sim test `a_chunk_that_never_arrives_ends_the_session_as_a_link_timeout_at_its_phase_deadline_past_the_idle_deadline`).
That end is inside the 40.5 s feed-to-feed bound.

The stage has its header receive armed before the host sends the next request: the reply's send is followed by the next
receive's arm, before any poll or wait (`BulkLink::recv`'s first statement; sim test
`after_a_reply_is_sent_the_next_header_receive_is_armed_before_any_wait`).

## Bench checks, and what is open

The timing in these documents was checked in simulation, on a simulated clock. **It has never been measured on a board.** The
following are bench checks, listed as steps in `testing/board-runs/FL1.md`:

- the init budget (`INIT_BUDGET_US`, arm to visible), and enumeration fitting in what is left before the first idle deadline;
- the send-end to arm gap (the ROM's jump and the stage's entry), which the first window's anchor assumes is non-negative;
- the controller command times, each against its phase deadline (the sim's `Costs` knobs model them as free unless a test sets them);
- address-before-status (`Controller::address_before_status`);
- the speed register's encoding (DSTS.CONNECTSPD, 0 is high speed);
- the stop procedure (release, port reset, re-enumeration under the unchanged deadline, then a normal watchdog reset);
- the storage budget: a 64 KiB write and its read-back on a slow card must fit `TRANSFER_BUDGET_US`.

**Open:** the surplus bytes of an overlong transfer are not closed by any test. The stage's behaviour is undefined until the
drain's semantics are shown on the bench.

