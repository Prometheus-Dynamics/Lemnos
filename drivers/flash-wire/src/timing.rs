//! The timing contract. A host meets these, and a stage keeps them. They are constants here so both sides use the
//! same numbers, and they are folded into the wire hash: changing one is a wire change.
//!
//! **One clock.** Every limit in a session is counted from the last watchdog feed, or from the start of the session
//! (the watchdog is armed then, so the start counts as a feed). A reply does not feed, and a bus reset does not feed:
//! neither restarts the clock. So a host that identifies every 29 seconds does not keep a session alive past the idle
//! limit, and a host that resets the bus does not get a fresh enumeration limit beyond what the watchdog allows.
//!
//! **Transfers and rounds.** A transfer (one frame, or one control stage) is bounded by [`TRANSFER_BUDGET_US`]. A
//! write round has four bounded phases, each one transfer or one storage operation: receiving the chunk, writing it,
//! reading it back, and sending the status. A read round has three: reading the storage, sending the reply, and the
//! status of a refused one, which never overlaps. The round budget is the four-phase worst case, [`ROUND_BUDGET_US`].
//!
//! **Feed to feed.** The watchdog must be fed again before its period from the last feed. The worst case between two
//! feeds is: the idle limit runs out just after a feed, a new round then starts and reaches its feed (the chunk, the
//! write and the read-back: three phases; the status is sent after the feed and is inside the next idle window).
//! [`FEED_TO_FEED_US`] is that bound, taken as the idle limit plus the full round, which is conservative. It is
//! asserted below to be under [`REFERENCE_WATCHDOG_PERIOD_US`], so a change to any of these numbers that breaks the
//! watchdog fails the build.
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

/// The worst gap between two feeds: the idle limit, then the phases of a round up to its feed. Taken as the
/// idle limit plus the whole round, which is conservative by one status phase.
pub const FEED_TO_FEED_US: u64 = IDLE_LIMIT_US + ROUND_BUDGET_US;

/// The session budget: what the watchdog must cover from its last feed. The same as [`FEED_TO_FEED_US`].
pub const SESSION_BUDGET_US: u64 = FEED_TO_FEED_US;

/// The period of the reference watchdog's longest setting: 2^31 cycles at 24 MHz, in microseconds. A stage whose
/// watchdog offers a longer period covers the budget with a shorter one; this is the bound the wire is checked
/// against.
pub const REFERENCE_WATCHDOG_PERIOD_US: u64 = 44_739_242;

/// The host's default bulk timeout this contract is sized against, in microseconds.
pub const HOST_BULK_TIMEOUT_US: u64 = 5_000_000;

// The watchdog must cover the worst feed-to-feed gap: checked when the crate is built.
const _: () = assert!(
    FEED_TO_FEED_US < REFERENCE_WATCHDOG_PERIOD_US,
    "the feed-to-feed bound must be under the reference watchdog period"
);
// The idle limit must expire before the watchdog would fire, so the distinct idle outcome comes first.
const _: () = assert!(IDLE_LIMIT_US < REFERENCE_WATCHDOG_PERIOD_US);
