//! The lease queue: which leases hold the GPU and which wait, in the order they are granted.
//!
//! Pure state. It reads no clock and allocates nothing: every method is given the time it is at,
//! and whether a holder is overdue is worked out then, when the next call comes in. A lease is held in
//! `entries[..holders]` and waits in `entries[holders..len]`, in grant order.

use lemnos_contract_gpu_lease::{LeaseState, Ticket};
use lemnos_core::Status;

/// Leases held or waiting at once. A full queue refuses with `BUSY` until one ends.
pub const MAX_LEASES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    lease: u32,
    shared: bool,
    priority: u8,
    max_hold_ms: u32,
    /// Granted at (a holder) or queued at (a waiter).
    since_ms: u64,
    yield_asked: bool,
    overdue: bool,
}

impl Entry {
    const EMPTY: Self = Self {
        lease: 0,
        shared: false,
        priority: 0,
        max_hold_ms: 0,
        since_ms: 0,
        yield_asked: false,
        overdue: false,
    };
}

/// The GPU's leases, held and waiting.
#[derive(Debug)]
pub struct Queue {
    entries: [Entry; MAX_LEASES],
    /// Entries in use.
    len: usize,
    /// How many of them are held: the first `holders`.
    holders: usize,
    /// The next lease number. Never 0.
    next: u32,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub const fn new() -> Self {
        Self {
            entries: [Entry::EMPTY; MAX_LEASES],
            len: 0,
            holders: 0,
            next: 1,
        }
    }

    /// Whether a lease of the given sharing could be held beside the current holders.
    fn fits(&self, shared: bool) -> bool {
        self.holders == 0 || (shared && self.entries[..self.holders].iter().all(|e| e.shared))
    }

    /// Brings the queue up to `now_ms`: grants waiters in order while the first one fits, asks
    /// holders below the first waiter's priority to yield, and marks holders past their time.
    fn settle(&mut self, now_ms: u64) {
        while self.len > self.holders && self.fits(self.entries[self.holders].shared) {
            self.entries[self.holders].since_ms = now_ms;
            self.holders += 1;
        }
        if self.len > self.holders {
            let priority = self.entries[self.holders].priority;
            for e in &mut self.entries[..self.holders] {
                if e.priority < priority {
                    e.yield_asked = true;
                }
            }
        }
        for e in &mut self.entries[..self.holders] {
            if !e.overdue
                && e.max_hold_ms > 0
                && now_ms >= e.since_ms.saturating_add(u64::from(e.max_hold_ms))
            {
                e.overdue = true;
            }
        }
    }

    fn find(&self, lease: u32) -> Option<usize> {
        self.entries[..self.len]
            .iter()
            .position(|e| e.lease == lease)
    }

    fn take_number(&mut self) -> u32 {
        let lease = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        lease
    }

    /// Asks for a lease. Held now if it fits and no one is ahead; else queued behind the holders
    /// and any waiter of equal or higher priority, when `wait` is set; else `BUSY`.
    pub fn acquire(
        &mut self,
        shared: bool,
        priority: u8,
        wait: bool,
        max_hold_ms: u32,
        now_ms: u64,
    ) -> Result<Ticket, Status> {
        self.settle(now_ms);
        if self.len == MAX_LEASES {
            return Err(Status::BUSY);
        }
        let waiting = &self.entries[self.holders..self.len];
        let at = waiting
            .iter()
            .position(|w| w.priority < priority)
            .unwrap_or(waiting.len());
        let now = at == 0 && self.fits(shared);
        if !now && !wait {
            return Err(Status::BUSY);
        }
        let lease = self.take_number();
        let index = self.holders + at;
        self.entries.copy_within(index..self.len, index + 1);
        self.entries[index] = Entry {
            lease,
            shared,
            priority,
            max_hold_ms,
            since_ms: now_ms,
            yield_asked: false,
            overdue: false,
        };
        self.len += 1;
        self.settle(now_ms);
        let held = index < self.holders;
        Ok(Ticket {
            lease,
            held,
            ahead: if held {
                0
            } else {
                u16::try_from(index).unwrap_or(u16::MAX)
            },
        })
    }

    /// Where `lease` stands now.
    pub fn poll(&mut self, lease: u32, now_ms: u64) -> Result<LeaseState, Status> {
        self.settle(now_ms);
        let index = self.find(lease).ok_or(Status::NOT_FOUND)?;
        let e = self.entries[index];
        let held = index < self.holders;
        Ok(LeaseState {
            held,
            ahead: if held {
                0
            } else {
                u16::try_from(index).unwrap_or(u16::MAX)
            },
            for_ms: now_ms.saturating_sub(e.since_ms),
            max_hold_ms: e.max_hold_ms,
            yield_asked: e.yield_asked,
            overdue: e.overdue,
        })
    }

    /// Ends `lease`, held or waiting, and grants whoever it was holding back.
    pub fn release(&mut self, lease: u32, now_ms: u64) -> Result<(), Status> {
        self.settle(now_ms);
        let index = self.find(lease).ok_or(Status::NOT_FOUND)?;
        self.entries.copy_within(index + 1..self.len, index);
        self.len -= 1;
        if index < self.holders {
            self.holders -= 1;
        }
        self.settle(now_ms);
        Ok(())
    }

    /// Whether `lease` is held or waiting.
    pub fn contains(&self, lease: u32) -> bool {
        self.find(lease).is_some()
    }

    /// The holder and waiter counts, as of `now_ms`.
    pub fn counts(&mut self, now_ms: u64) -> (u16, u16) {
        self.settle(now_ms);
        (
            u16::try_from(self.holders).unwrap_or(u16::MAX),
            u16::try_from(self.len - self.holders).unwrap_or(u16::MAX),
        )
    }
}
