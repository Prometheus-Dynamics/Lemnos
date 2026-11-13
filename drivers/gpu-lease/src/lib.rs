//! The advisory GPU lease (`gpu-lease/1`) as a driver. One device is one GPU: a board names it
//! as a device, and callers that agree to share it ask this device for leases.
//!
//! The driver reads and writes no hardware and no sysfs. Its whole state is in memory and is
//! gone when the device is dropped, which ends every lease, the same as a service restart.
//!
//! Time is read from the driver's own monotonic clock on each call. Nothing runs between calls:
//! a lease's overdue and yield flags and a waiter's grant are all worked out when the
//! next call comes in.
//!
//! Failure rule: nothing here fails for lack of hardware. A call that cannot be served
//! (`BUSY` for a full queue, `NOT_FOUND` for a lease that has ended) answers that call, and the
//! next call tries again. The driver never switches itself off.

mod queue;

#[cfg(test)]
mod tests;

use lemnos_contract_gpu_lease::{GpuLease, GpuState, LeaseState, Ticket};
use lemnos_core::{
    CallerId, Config, Description, Item, Lifecycle, MethodId, Result, Status, driver,
};
use queue::Queue;
use std::time::Instant;

/// The longest hang detail kept. Longer text is cut at a character boundary.
pub const DETAIL_MAX: usize = 128;

/// The `state` method in the `gpu-lease/1` contract.
const STATE: MethodId = MethodId {
    contract: 0,
    method: 5,
};

#[driver("gpu-lease", provides = [GpuLease])]
pub struct GpuLeaseDevice {
    queue: Queue,
    /// The driver's clock starts here. Times are milliseconds since then.
    epoch: Instant,
    hangs: u32,
    /// Milliseconds since `epoch` of the last hang, if any.
    last_hang_ms: Option<u64>,
    detail: [u8; DETAIL_MAX],
    detail_len: usize,
}

impl Lifecycle for GpuLeaseDevice {
    type Needs = GpuLeaseDeviceNeeds;
    const TRACKS_CALLERS: bool = true;

    fn start(_needs: GpuLeaseDeviceNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            queue: Queue::new(),
            epoch: Instant::now(),
            hangs: 0,
            last_hang_ms: None,
            detail: [0; DETAIL_MAX],
            detail_len: 0,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("state", STATE).dimensionless());
    }

    fn caller(&mut self, caller: CallerId) {
        self.queue.set_caller(caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        let now = self.now_ms();
        self.queue.caller_gone(caller, now);
    }
}

impl GpuLeaseDevice {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The longest prefix of `text` that is at most [`DETAIL_MAX`] bytes and ends on a character.
fn clip(text: &str) -> &str {
    let mut end = text.len().min(DETAIL_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl GpuLease for GpuLeaseDevice {
    fn acquire(
        &mut self,
        shared: bool,
        priority: u8,
        wait: bool,
        max_hold_ms: u32,
    ) -> Result<Ticket> {
        let now = self.now_ms();
        self.queue.acquire(shared, priority, wait, max_hold_ms, now)
    }

    fn poll(&mut self, lease: u32) -> Result<LeaseState> {
        let now = self.now_ms();
        self.queue.poll(lease, now)
    }

    fn release(&mut self, lease: u32) -> Result<()> {
        let now = self.now_ms();
        self.queue.release(lease, now)
    }

    fn hang(&mut self, lease: u32, detail: &str) -> Result<()> {
        if !self.queue.contains(lease) {
            return Err(Status::NOT_FOUND);
        }
        let text = clip(detail);
        self.detail[..text.len()].copy_from_slice(text.as_bytes());
        self.detail_len = text.len();
        self.hangs = self.hangs.saturating_add(1);
        self.last_hang_ms = Some(self.now_ms());
        Ok(())
    }

    fn state(&mut self) -> Result<GpuState> {
        let now = self.now_ms();
        let (holders, waiters) = self.queue.counts(now);
        Ok(GpuState {
            holders,
            waiters,
            hangs: self.hangs,
            last_hang_ms: self.last_hang_ms.map(|at| now.saturating_sub(at)),
        })
    }

    fn last_hang(&mut self, into: &mut [u8]) -> Result<usize> {
        let detail = &self.detail[..self.detail_len];
        let out = into
            .get_mut(..detail.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?;
        out.copy_from_slice(detail);
        Ok(detail.len())
    }
}
