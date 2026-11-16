//! The timing contract. A host meets these, and a stage keeps them. They are constants here so both
//! sides use the same numbers, and they are folded into the wire hash: changing one is a wire change.
//!
//! The stage's own bounds, from the watchdog's period downwards:
//!
//! - A **transfer** (one frame over the bulk pair, or one control stage) is bounded by
//!   [`TRANSFER_BUDGET_US`]. A host's bulk timeout must be longer than this, and a host must not
//!   time out a read it has posted before this much time has passed.
//! - A **round** (one request from its chunk to its reply) has three bounded phases: the chunk
//!   transfer, the storage operation, and the reply transfer. Each gets [`TRANSFER_BUDGET_US`], so the
//!   round is [`ROUND_BUDGET_US`].
//! - The **idle limit** is how long a host may pause between a reply and the next request before the
//!   stage ends the session. It must cover a host's default bulk timeout (5 s) with retries and a slow
//!   hub, so [`IDLE_LIMIT_US`] is 30 s: six times the default timeout. A host that pauses longer than
//!   this loses the session and must re-enumerate.
//! - The **enumeration limit** is how long a stage waits, from its start or a bus reset, for a host to
//!   set a configuration ([`ENUMERATION_LIMIT_US`]). Past it the stage ends and the watchdog's reset is
//!   the exit; the host starts again.
//! - The **session budget** is what the watchdog must cover from its last feed: the idle limit plus
//!   one round, [`SESSION_BUDGET_US`]. The watchdog's period is the smallest one that covers it (44.7 s
//!   on the reference watchdog, which has periods of 2.7 ms to 89.5 s).
//!
//! No bound is a spin: every wait is a deadline on the stage's clock.

/// The longest one transfer may take, in microseconds: a transfer is one frame or one control stage.
pub const TRANSFER_BUDGET_US: u64 = 3_500_000;

/// The longest one round may take: its chunk transfer, its storage operation and its reply transfer,
/// each of which gets [`TRANSFER_BUDGET_US`].
pub const ROUND_BUDGET_US: u64 = 3 * TRANSFER_BUDGET_US;

/// The longest a host may pause between a reply and its next request, in microseconds. The stage ends the
/// session after it.
pub const IDLE_LIMIT_US: u64 = 30_000_000;

/// The longest a stage waits for a configuration, from its start or a bus reset.
pub const ENUMERATION_LIMIT_US: u64 = 30_000_000;

/// What the watchdog must cover from its last feed: the idle limit and one round.
pub const SESSION_BUDGET_US: u64 = IDLE_LIMIT_US + ROUND_BUDGET_US;

/// The host's default bulk timeout this contract is sized against, in microseconds.
pub const HOST_BULK_TIMEOUT_US: u64 = 5_000_000;
