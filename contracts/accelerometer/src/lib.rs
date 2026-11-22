//! The `accelerometer/1` contract.
//!
//! `read` answers the newest sample. `read_batch` answers the samples since the last batch, each
//! with the time it was taken, and is separate from `read`: a batch does not change what `read`
//! returns, and a `read` does not take samples out of a batch.
//!
//! A batch is records of [`RECORD_LEN`] bytes, oldest first: the stamp as a little-endian `u64` in
//! `at_ns` (the host's clock, the scale `tick` uses), then X, Y and Z as little-endian `f32` in
//! m/s². A record is a [`Sample`]: decode it with `Sample::decode` over the record's bytes. A batch
//! holds whole records only. A sample the driver could not stamp, or that was overwritten before a
//! batch read it, is not in the batch: the gap shows as two stamps more than one period apart.
//!
//! `at_ns` is always a plain time, with no flag bits in it. Each device's stamps are monotonic
//! within its batches whichever way they were made, so a reader that uses only `at_ns` gets a
//! plausible time in every mode. How a stamp was made (the host's tick time, or the device's sample
//! clock) is said by `read_batch_stamped`, which takes the same samples as `read_batch`: a batch is
//! read with one of the two.
//!
//! Compatibility. `read_batch_stamped` is an addition (method 3, after every base method), so the
//! base hash does not change, and `read_batch` and its records do not change. A consumer built
//! before the addition never calls method 3 and reads what it read before. A device without the
//! method answers `NOT_SUPPORTED`.

#![no_std]

use lemnos_core::{Decode, Encode, Reader, Result, Status, Vector3, contract};

/// One sample and the time it was taken, `at_ns` in the host's `now_ns`.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Default)]
pub struct Sample {
    pub at_ns: u64,
    pub value: Vector3,
}

/// Bytes of one record of a batch: a block length (4), the stamp (8), then X, Y and Z as their
/// own block (4 + 3 × 4).
pub const RECORD_LEN: usize = 4 + 8 + (4 + 3 * 4);

/// How a record's stamp was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stamp {
    /// From the device's sample clock's model, once it has converged.
    #[default]
    Model,
    /// From the host's tick time, less the sample periods back from the newest sample. Used while the
    /// sample clock has not converged, for a drain whose clock brackets were too wide to use, and after
    /// a restart (an overflow, a reset, a jump) until the clock has converged again.
    TickTime,
}

/// Bytes of one record of a stamped batch (`read_batch_stamped`): the stamp's mode (1), then a batch
/// record.
pub const STAMPED_RECORD_LEN: usize = 1 + RECORD_LEN;

/// One record of a stamped batch: how its stamp was made, and the record itself.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stamped {
    pub stamp: Stamp,
    pub sample: Sample,
}

impl Decode<'_> for Stamped {
    fn decode(from: &mut Reader<'_>) -> Result<Self> {
        let stamp = match u8::decode(from)? {
            0 => Stamp::Model,
            1 => Stamp::TickTime,
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        Ok(Self {
            stamp,
            sample: Sample::decode(from)?,
        })
    }
}

#[contract("accelerometer", version = 1, kind = "sensor")]
pub trait Accelerometer {
    /// Acceleration in m/s²..
    #[method(1, reading = three_axes)]
    fn read(&mut self) -> Result<Vector3>;
    /// The samples since the last batch, oldest first, as records into `into` (see the module
    /// docs). Returns the bytes written: 0 when there is no new sample. Samples that do not fit
    /// wait for the next batch. Without the FIFO there are no batches: `NOT_SUPPORTED`.
    #[method(2)]
    fn read_batch(&mut self, #[out(bytes)] into: &mut [u8]) -> Result<usize>;
    /// The same samples as `read_batch`, as [`STAMPED_RECORD_LEN`]-byte records of [`Stamped`],
    /// oldest first: each record's mode, then its batch record. It takes from the same queue as
    /// `read_batch`, so a batch is read with one of the two. Returns the bytes written.
    #[method(3)]
    #[added]
    fn read_batch_stamped(&mut self, #[out(bytes)] into: &mut [u8]) -> Result<usize>;
}
