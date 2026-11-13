//! The `gpu-lease/1` contract: an advisory lease on one GPU.
//!
//! Nothing here is enforced. The GPU's userspace driver opens the render node itself, so a lease
//! orders the callers that ask for it. A caller that never asks keeps working as before.
//!
//! A lease is held or waiting. Held leases share the GPU only when all of them are shared. Waiters
//! are granted in order: higher priority first, then arrival, and nothing overtakes a waiter ahead
//! of it. A waiter is granted by calling [`GpuLease::poll`] and seeing it held: nothing is pushed.
//!
//! The lease number is the only identity. Whoever has a number may poll, release or report a hang
//! under it. A lease ends when it is released. Nothing else ends one: a holder that dies or drops
//! its connection without releasing keeps the GPU until the device is dropped.
//!
//! `max_hold_ms` is advisory. Once it has passed, `poll` reports `overdue` for that lease. Nothing
//! is taken back.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// What `acquire` answers: the new lease, and whether it is held.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ticket {
    pub lease: u32,
    /// Held now. `false`: queued, behind `ahead` leases.
    pub held: bool,
    /// Leases ahead in the queue when `held` is `false`. Zero when held.
    pub ahead: u16,
}

/// Where a lease stands, as of the call.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaseState {
    pub held: bool,
    /// Leases ahead in the queue when `held` is `false`.
    pub ahead: u16,
    /// Milliseconds since it was granted (held) or queued (waiting).
    pub for_ms: u64,
    /// The holder's expected hold in milliseconds, or 0 for no limit.
    pub max_hold_ms: u32,
    /// A waiter of higher priority is held back by this lease. The holder decides whether to
    /// release. Stays set until the lease ends.
    pub yield_asked: bool,
    /// Held past `max_hold_ms`. Stays set until the lease ends.
    pub overdue: bool,
}

/// The GPU's lease state, as of the call.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuState {
    pub holders: u16,
    pub waiters: u16,
    /// Hangs reported since the device was started.
    pub hangs: u32,
    /// Milliseconds since the last hang was reported, or `None` if none was.
    pub last_hang_ms: Option<u64>,
}

#[contract("gpu-lease", version = 1, kind = "compute")]
pub trait GpuLease {
    /// Asks for the GPU. `shared` leases may be held together, and exclusive ones are held alone.
    /// A lease that cannot be held now is queued when `wait` is set, and otherwise refused with
    /// `BUSY`. A full queue also answers `BUSY`: try again later.
    #[method(1)]
    fn acquire(
        &mut self,
        shared: bool,
        priority: u8,
        wait: bool,
        max_hold_ms: u32,
    ) -> Result<Ticket>;
    /// Where `lease` stands now. Waiters are granted here, so a waiter polls until it is held.
    /// `NOT_FOUND` if the lease has ended or never existed.
    #[method(2)]
    fn poll(&mut self, lease: u32) -> Result<LeaseState>;
    /// Ends `lease`, held or waiting. A waiter that gives up its place is released the same way.
    /// `NOT_FOUND` if the lease has ended or never existed.
    #[method(3)]
    fn release(&mut self, lease: u32) -> Result<()>;
    /// Records a hang the caller saw under `lease`. `detail` is kept up to a fixed length.
    /// `NOT_FOUND` if the lease has ended or never existed.
    #[method(4)]
    fn hang(&mut self, lease: u32, detail: &str) -> Result<()>;
    /// The GPU's counts and hang count.
    #[method(5)]
    fn state(&mut self) -> Result<GpuState>;
    /// The detail of the last hang into `into`, returning its length. Empty if none was reported.
    /// At most 128 bytes (the driver's `DETAIL_MAX`); `into` holds that.
    #[method(6)]
    fn last_hang(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}
