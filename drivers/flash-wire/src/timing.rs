//! The timing contract. A host meets these, and a stage keeps them. They are constants here so both sides use the
//! same numbers, and they are folded into the wire hash: changing one is a wire change.
//!
//! **One clock.** Every limit in a session is counted from the last watchdog feed. The first feed is the instant the
//! watchdog is armed: the stage takes that instant before its own DWC3 and SD bring-up, so the first deadline counts
//! from the arm, not from the start of the session. A reply does not feed, and a bus reset does not feed:
//! neither restarts the clock. So a host that identifies every 29 seconds does not keep a session alive past the idle
//! limit, and a host that resets the bus does not get a fresh enumeration limit beyond what the watchdog allows.
//!
//! **Transfers and rounds.** A transfer (one frame, or one control stage) is bounded by [`TRANSFER_BUDGET_US`]. A
//! write round has four bounded phases, each one transfer or one storage operation: receiving the chunk, writing it,
//! reading it back, and sending the status. A read round has three: reading the storage, sending the reply, and the
//! status of a refused one, which never overlaps. The round budget is the four-phase worst case, [`ROUND_BUDGET_US`].
//!
//! **Feed to feed.** The watchdog must be fed again before its period from the last feed. A feed is recorded when it
//! is made (after the read-back of a write, or after the reply of a read), not after the status is sent. The worst gap
//! between two feeds: a request arrives just before the idle deadline, which is [`IDLE_LIMIT_US`] after the last feed;
//! its write round then runs its chunk, its write and its read-back, the three phases before the feed, at most
//! [`WRITE_TO_FEED_PHASES`] times [`TRANSFER_BUDGET_US`]. [`FEED_TO_FEED_US`] is that sum. It is asserted below to be
//! under [`REFERENCE_WATCHDOG_PERIOD_US`], so a change to any of these numbers that breaks the watchdog fails the build.
//!
//! **Hosts.** A host's bulk timeout is at least [`HOST_BULK_TIMEOUT_US`]. A host must post its IN read within
//! [`TRANSFER_BUDGET_US`] of sending a request, and must not pause longer than the idle limit between one reply and
//! its next request, counted from the last feed.

/// The longest one transfer may take, in microseconds: a transfer is one frame or one control stage.
pub const TRANSFER_BUDGET_US: u64 = 3_500_000;

/// The number of bounded phases in the longest round: the chunk, the storage write, the read-back, and the status.
pub const ROUND_PHASES: u64 = 4;

/// The longest one round may take, in microseconds: each phase at [`TRANSFER_BUDGET_US`].
pub const ROUND_BUDGET_US: u64 = ROUND_PHASES * TRANSFER_BUDGET_US;

/// The longest a host may go without a feed of the watchdog, counted from the last feed or the start of the
/// session, before the stage ends the session with its idle outcome. Also the limit on enumeration:
/// a host that has not configured the device by then has lost the session.
pub const IDLE_LIMIT_US: u64 = 30_000_000;

/// The phases of a write round before its feed: the chunk, the storage write, the read-back.
pub const WRITE_TO_FEED_PHASES: u64 = 3;

/// The worst gap between two feeds: the idle deadline (up to [`IDLE_LIMIT_US`] after the last feed) plus a write
/// round up to its feed. A read round is shorter (storage read, reply), so it does not set the bound.
pub const FEED_TO_FEED_US: u64 = IDLE_LIMIT_US + WRITE_TO_FEED_PHASES * TRANSFER_BUDGET_US;

/// The session budget: what the watchdog must cover from its last feed. The same as [`FEED_TO_FEED_US`].
pub const SESSION_BUDGET_US: u64 = FEED_TO_FEED_US;

/// The period of the reference watchdog's setting TOP 14: 2^30 cycles at 24 MHz, in microseconds (44.739242 s). The
/// stage's period is the smallest setting that covers [`SESSION_BUDGET_US`], which is this one. The setting TOP 15 is
/// 2^31 cycles (89.48 s) and is not used. This is the bound the wire is checked against.
pub const REFERENCE_WATCHDOG_PERIOD_US: u64 = 44_739_242;

/// The host's default bulk timeout this contract is sized against, in microseconds.
pub const HOST_BULK_TIMEOUT_US: u64 = 5_000_000;

/// The stage's init budget, in microseconds: from the instant the watchdog is armed to the instant the device is visible
/// to the host (the controller's run bit). Every wait of the bring-up (the DWC3 soft resets, the endpoint configuration,
/// the SD host and card init) takes the init deadline, `arm + INIT_BUDGET_US`, so the device cannot become visible later
/// than this. Once it is visible, the session's own deadlines apply.
pub const INIT_BUDGET_US: u64 = 10_000_000;

/// The host's first window, in microseconds: the longest a host may take from first seeing the device to its first fed
/// request. The device is visible no later than `INIT_BUDGET_US` after the arm, and the first idle deadline is `IDLE_LIMIT_US`
/// after the arm, so the window is their difference. It is asserted below to be positive.
pub const FIRST_REQUEST_WINDOW_US: u64 = IDLE_LIMIT_US - INIT_BUDGET_US;

/// The pickup allowance, in microseconds: from the host's sending a request until the stage has it in hand. The request's
/// own receive is one transfer, bounded by [`TRANSFER_BUDGET_US`].
pub const REPLY_PICKUP_US: u64 = TRANSFER_BUDGET_US;

/// The margin a host adds to the stage's worst case, in microseconds: its own scheduling of the read after the reply is
/// due. A stated allowance, not a measured one.
pub const REPLY_MARGIN_US: u64 = 1_000_000;

/// The longest a host may take to read one reply, in microseconds, counted from the instant the host SENT its request.
///
/// Worst case, from that instant: the stage picks the request up within [`REPLY_PICKUP_US`]; then its round runs its
/// [`ROUND_PHASES`] phases, each at most [`TRANSFER_BUDGET_US`]; then the reply is posted. Control requests cannot extend a
/// phase (every controller command takes its phase deadline), so no other time is in the worst case. The host adds
/// [`REPLY_MARGIN_US`]. The value is derived from those constants, and asserted below to be at least the worst case and at
/// least 16 s. It is separate from the 5 s bulk-OUT bound ([`HOST_BULK_TIMEOUT_US`]), which is one transfer.
pub const REPLY_READ_BOUND_US: u64 =
    REPLY_PICKUP_US + ROUND_PHASES * TRANSFER_BUDGET_US + REPLY_MARGIN_US;

// The watchdog must cover the worst feed-to-feed gap, and the phase count must be the write round's: checked at build.
const _: () = assert!(
    FEED_TO_FEED_US < REFERENCE_WATCHDOG_PERIOD_US,
    "the feed-to-feed bound must be under the reference watchdog period"
);
// The reply-read bound is at least the stage's worst case from the request's send, computed from the same constants, and
// at least 16 s, and longer than one OUT bound.
const _: () = assert!(REPLY_READ_BOUND_US >= REPLY_PICKUP_US + ROUND_PHASES * TRANSFER_BUDGET_US);
const _: () = assert!(REPLY_READ_BOUND_US >= 16_000_000);
const _: () = assert!(REPLY_READ_BOUND_US > HOST_BULK_TIMEOUT_US);
// The init budget leaves the host a positive window, and the window is the one the first deadline gives.
const _: () = assert!(INIT_BUDGET_US < IDLE_LIMIT_US);
const _: () = assert!(FIRST_REQUEST_WINDOW_US == IDLE_LIMIT_US - INIT_BUDGET_US);
// The idle limit must expire before the watchdog would fire, so the distinct idle outcome comes first.
const _: () = assert!(IDLE_LIMIT_US < REFERENCE_WATCHDOG_PERIOD_US);
