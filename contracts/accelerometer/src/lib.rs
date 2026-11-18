//! The `accelerometer/1` contract.
//!
//! `read` answers the newest sample. `read_batch` answers the samples since the last batch, each
//! with the time it was taken, and is separate from `read`: a batch does not change what `read`
//! returns, and a `read` does not take samples out of a batch.
//!
//! A batch is records of [`RECORD_LEN`] bytes, oldest first: the stamp as a little-endian `u64` in
//! `now_ns` (the host's clock, the scale `tick` uses; bit 63 marks a tick-time stamp, see
//! [`TICK_TIME`]), then X, Y and Z as little-endian `f32` in
//! m/s². A record is a [`Sample`]: decode it with `Sample::decode` over the record's bytes. A
//! batch holds whole records only. A sample the driver could not stamp, or that was overwritten
//! before a batch read it, is not in the batch: the gap shows as two stamps more than one period
//! apart.

#![no_std]

use lemnos_core::{Decode, Encode, Result, Vector3, contract};

/// One sample and the time it was taken, `at_ns` in the host's `now_ns`.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Default)]
pub struct Sample {
    pub at_ns: u64,
    pub value: Vector3,
}

/// The top bit of `at_ns` marks a record stamped from the host's tick time: the driver's sample clock
/// had not converged, or the brackets it reads its own clock with were too wide to use, so the stamp
/// is the tick's time less the sample periods (within the tick's jitter and up to a period more).
/// A record without the bit is stamped from the sample clock's model. A stamp is a host time, far
/// below 2^63 ns (292 years), so the bit is spare, and a record's layout, its length and the contract's
/// hash do not change. A reader masks the bit off to get the time: [`Sample::time_ns`].
pub const TICK_TIME: u64 = 1 << 63;

impl Sample {
    /// The time the sample was taken, in the host's `now_ns`, without the mode bit.
    pub const fn time_ns(&self) -> u64 {
        self.at_ns & !TICK_TIME
    }

    /// Whether the sample was stamped from the tick's time (see [`TICK_TIME`]).
    pub const fn from_tick_time(&self) -> bool {
        self.at_ns & TICK_TIME != 0
    }
}

/// Bytes of one record of a batch: a block length (4), the stamp (8), then X, Y and Z as their
/// own block (4 + 3 × 4).
pub const RECORD_LEN: usize = 4 + 8 + (4 + 3 * 4);

#[contract("accelerometer", version = 1, kind = "sensor")]
pub trait Accelerometer {
    /// Acceleration in m/s².
    #[method(1, reading = three_axes)]
    fn read(&mut self) -> Result<Vector3>;
    /// The samples since the last batch, oldest first, as records into `into` (see the module
    /// docs). Returns the bytes written: 0 when there is no new sample. Samples that do not fit
    /// wait for the next batch. Without the FIFO there are no batches: `NOT_SUPPORTED`.
    #[method(2)]
    fn read_batch(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}
