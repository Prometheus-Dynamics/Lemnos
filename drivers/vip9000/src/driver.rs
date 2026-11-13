//! The vendor kernels' node: what serves it, whether a job there could be confined, and the
//! `npu/1` contract answered from that. The block in `lemnos.ko` is served by [`crate::claim`]
//! instead; this is what a board that names the vendor's node gets.
//!
//! # Failures
//!
//! [`Vip9000::check`] asks the [`Device`] which kernel driver serves the node and what the
//! block's table maps while no job runs. When the device fails, the check backs off: until it is
//! due, `check` answers [`State::BackingOff`] without touching the device. The delay doubles from
//! 250 ms and is capped at 8 s, clears when a check gets an answer, and the driver never gives up.
//!
//! # What the contract gets
//!
//! [`Npu::info`] reports the largest program the block's command buffer takes and room for no
//! program. [`Npu::load`] checks its arguments and then answers `NOT_SUPPORTED`: on the vendor's
//! kernel drivers a job could reach past its own buffers (see [`crate::confine`]), and the run
//! path is only through `lemnos.ko` ([`crate::claim`]). So no program loads, nothing is attached
//! and no job stream is named; [`Npu::job_stream`] answers `NOT_SUPPORTED` and the other calls
//! `NOT_FOUND`. [`Vip9000::state`] says why.
//!
//! # Allocation
//!
//! Nothing here allocates: the table is read into a fixed array.

use std::io;

use lemnos_contract_npu::{Counters, Npu, NpuInfo};
use lemnos_core::{Result, Status};

use crate::confine::{self, Facts, Gaps, Range, check_reach};
use crate::uapi::{APPEND_COMMAND_SIZE, MAX_CMD_SIZE};

/// Ranges of the block's table read at one check. A table holding more is a gap by itself.
pub const MAX_MAPPED: usize = 64;

/// First delay before a failed check is tried again.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay after `failures` failures in a row (from 1), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(failures: u32) -> u64 {
    let attempt = if failures == 0 { 0 } else { failures - 1 };
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// The largest program the block takes: a command buffer's commands, leaving room for the
/// kernel's append, in whole 8-byte command words.
pub const MAX_PROGRAM_BYTES: u32 = (MAX_CMD_SIZE - APPEND_COMMAND_SIZE) / 8 * 8;

/// The kernel driver that serves the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kernel {
    /// The vendor's `vipcore`.
    Vipcore,
    /// The vendor's `galcore`.
    Galcore,
    /// `lemnos.ko`, with the block named in its `vip9000` parameter.
    Lemnos,
    /// Another driver, or none this crate knows.
    Other,
}

impl Kernel {
    /// What the driver does, as read from its source; `None` for one not read.
    pub const fn facts(self) -> Option<Facts> {
        match self {
            Self::Vipcore => Some(confine::VIPCORE),
            Self::Galcore => Some(confine::GALCORE),
            Self::Lemnos => Some(confine::LEMNOS),
            Self::Other => None,
        }
    }
}

/// The system side: the node and what the kernel says about the block.
pub trait Device {
    /// Which kernel driver serves the node (for example from the bound platform driver's name).
    fn kernel(&mut self) -> io::Result<Kernel>;

    /// What the block's address translation maps now, as ranges into `into`, returning how many.
    /// `Ok(None)` when the kernel cannot say, or holds more than `into` takes.
    fn mapped(&mut self, into: &mut [Range]) -> io::Result<Option<usize>>;
}

/// What the last check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not checked yet.
    Unchecked,
    /// The device failed; the next check may touch it from `retry_at_ns`.
    BackingOff {
        /// When, on the caller's clock.
        retry_at_ns: u64,
    },
    /// A job could reach past its own buffers, in these ways.
    Unconfined(Gaps),
    /// The kernel confines a job to its buffers, and nothing is mapped between jobs.
    Confined,
}

/// The block, through `D`.
pub struct Vip9000<D: Device> {
    device: D,
    state: State,
    failures: u32,
    retry_at_ns: u64,
    table: [Range; MAX_MAPPED],
}

impl<D: Device> Vip9000<D> {
    /// The block on `device`. Nothing is touched until [`Self::check`].
    pub fn new(device: D) -> Self {
        Self {
            device,
            state: State::Unchecked,
            failures: 0,
            retry_at_ns: 0,
            table: [Range::default(); MAX_MAPPED],
        }
    }

    /// The device.
    pub fn device(&self) -> &D {
        &self.device
    }

    /// The device, to change.
    pub fn device_mut(&mut self) -> &mut D {
        &mut self.device
    }

    /// What the last check found.
    pub fn state(&self) -> State {
        self.state
    }

    /// Finds which kernel driver serves the node and whether a job there could be confined: the
    /// driver's facts, then the block's table read while no job runs, which must map nothing.
    /// `now_ns` is the caller's clock, for the backoff.
    pub fn check(&mut self, now_ns: u64) -> State {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return State::BackingOff {
                retry_at_ns: self.retry_at_ns,
            };
        }
        self.state = match self.assess() {
            Ok(gaps) if gaps.is_empty() => State::Confined,
            Ok(gaps) => State::Unconfined(gaps),
            Err(_) => {
                self.failures = self.failures.saturating_add(1);
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
                return State::BackingOff {
                    retry_at_ns: self.retry_at_ns,
                };
            }
        };
        self.failures = 0;
        self.state
    }

    fn assess(&mut self) -> io::Result<Gaps> {
        let mut gaps = match self.device.kernel()?.facts() {
            Some(facts) => facts.gaps(),
            None => Gaps::UNKNOWN_KERNEL,
        };
        // No job runs, so the job's own buffers are none: anything mapped is reach.
        let idle = match self.device.mapped(&mut self.table)? {
            Some(n) if n <= MAX_MAPPED => check_reach(&self.table[..n], &[]).is_ok(),
            _ => false,
        };
        if !idle {
            gaps = gaps.with(Gaps::REACH);
        }
        Ok(gaps)
    }
}

impl<D: Device> Npu for Vip9000<D> {
    fn info(&mut self) -> Result<NpuInfo> {
        Ok(NpuInfo {
            max_program_bytes: MAX_PROGRAM_BYTES,
            max_programs: 0,
            max_attached: 0,
        })
    }

    fn job_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn load(&mut self, _buffer: u32, bytes: u32) -> Result<u32> {
        if bytes == 0 || bytes > MAX_PROGRAM_BYTES {
            return Err(Status::INVALID_ARGUMENT);
        }
        Err(Status::NOT_SUPPORTED)
    }

    fn unload(&mut self, _program: u32) -> Result<()> {
        Err(Status::NOT_FOUND)
    }

    fn attach(&mut self, _program: u32, _slot: u16, _buffer: u32, _bytes: u32) -> Result<()> {
        Err(Status::NOT_FOUND)
    }

    fn detach(&mut self, _program: u32, _slot: u16) -> Result<()> {
        Err(Status::NOT_FOUND)
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters::default())
    }
}
