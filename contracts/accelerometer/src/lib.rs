//! The `accelerometer/1` contract.
//!
//! `read` answers the newest sample. `read_batch` answers the samples since the last batch, each
//! with the time it was taken, and is separate from `read`: a batch does not change what `read`
//! returns, and a `read` does not take samples out of a batch.
//!
//! A batch is records of [`RECORD_LEN`] bytes, oldest first: the stamp as a little-endian `u64` in
//! `now_ns` (the host's clock, the scale `tick` uses), then X, Y and Z as little-endian `f32` in
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

/// Bytes of one record of a batch: a block length (4), the stamp (8), then X, Y and Z as their
/// own block (4 + 3 × 4).
pub const RECORD_LEN: usize = 4 + 8 + (4 + 3 * 4);

#[contract("accelerometer", version = 1)]
pub trait Accelerometer {
    /// Acceleration in m/s².
    #[method(1)]
    fn read(&mut self) -> Result<Vector3>;
    /// The samples since the last batch, oldest first, as records into `into` (see the module
    /// docs). Returns the bytes written: 0 when there is no new sample. Samples that do not fit
    /// wait for the next batch. Without the FIFO there are no batches: `NOT_SUPPORTED`.
    #[method(2)]
    fn read_batch(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}
