//! `image.scaler/1` jobs, taken from a stream whose buffers are dma-bufs.
//!
//! The consumer names two buffers of the stream in a job. [`Scaler::serve`] borrows their handles
//! from the stream and gives them to [`Ai2d::resize`], which checks the images against the
//! dma-bufs' real sizes, imports both, runs the job and removes both imports. The output buffer
//! is then published as the job's completion. No pixel is copied or read here.
//!
//! # What is checked
//!
//! Everything in a job is the consumer's, and none of it is trusted:
//!
//! - A buffer number must be one this driver holds, and the two must differ. The stream lends a
//!   handle for no other.
//! - The format must be one the [`Device`] drives the block with, the same on both sides; the
//!   filter one it offers; the sizes inside the limits; the stride at least the row.
//! - Each image, every line at its stride, must lie inside the dma-buf behind its buffer, by the
//!   size the kernel answers for the dma-buf; and the addresses the kernel answers for the two
//!   must leave the images apart. See [`crate::job`].
//!
//! # Buffers
//!
//! As the other image drivers do it: the driver holds every buffer the stream has free; the
//! output is published and is the driver's again once returned; buffers held from before a new
//! consumer are given up and taken again. The block runs a job to its end, and lets go of both
//! dma-bufs, before `serve` goes on.
//!
//! A consumer that posts bytes that are no job is broken, as the contract says: the driver
//! faults its stream ([`StreamEndpoint::fault_consumer`]), runs nothing more of its, and gives
//! up every buffer held under it.

use std::io;

use lemnos_contract_image_scaler::{Counters, ImageRef, JOB_BYTES, ScaleJob};
use lemnos_core::{Decode, Reader};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;

use crate::engine::{Ai2d, Device, Request, RunError, format};
use crate::job::{JobError, Shape};

/// Buffers of a stream this driver can hold. A job naming one past these is refused.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken in one [`Scaler::serve`]; the rest wait for the next.
pub const MAX_JOBS_PER_ROUND: u32 = 64;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a [`ScaleJob`] is taken off the queue and faults its consumer, not left at its head.
const LONGEST_JOB: usize = 4096;

/// Why the last failed job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job. The consumer's stream was faulted.
    Undecodable,
    /// A buffer the driver doesn't hold, or the same buffer twice.
    Buffers,
    /// A format or filter the block isn't driven with, or two formats in one job.
    Format,
    /// An empty image, or one past the limits.
    Size,
    /// A stride narrower than the row.
    Stride,
    /// The stream had no handle to lend: its buffers aren't dma-bufs, or the consumer went.
    NoHandle,
    /// An image doesn't fit the dma-buf behind its buffer, or the two images overlap.
    Frame,
    /// The block failed, or is backing off after a failure.
    Engine,
    /// The stream refused the completion.
    Completion,
}

impl From<JobError> for Failure {
    fn from(error: JobError) -> Self {
        match error {
            JobError::Format => Self::Format,
            JobError::Size => Self::Size,
            JobError::Stride => Self::Stride,
            JobError::Extent { .. } | JobError::Address | JobError::Overlap => Self::Frame,
        }
    }
}

/// Serves `image.scaler/1` jobs on the block.
pub struct Scaler<D, F> {
    ai2d: Ai2d<D, F>,
    /// The stream's buffers this driver holds, one bit each.
    held: u64,
    counters: Counters,
    last_failure: Option<Failure>,
    sequence: u64,
}

impl<D, F> std::fmt::Debug for Scaler<D, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scaler")
            .field("ai2d", &self.ai2d)
            .field("held", &self.held)
            .field("counters", &self.counters)
            .field("last_failure", &self.last_failure)
            .finish()
    }
}

impl<D: Device, F: FnMut() -> io::Result<D>> Scaler<D, F> {
    /// A scaler on `ai2d`, holding nothing yet.
    pub fn new(ai2d: Ai2d<D, F>) -> Self {
        Self {
            ai2d,
            held: 0,
            counters: Counters::default(),
            last_failure: None,
            sequence: 0,
        }
    }

    /// The block.
    pub fn block(&self) -> &Ai2d<D, F> {
        &self.ai2d
    }

    /// Jobs completed and failed since this was made.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Why the last failed job failed.
    pub fn last_failure(&self) -> Option<Failure> {
        self.last_failure
    }

    /// Whether the driver holds `buffer`.
    pub fn holds(&self, buffer: u32) -> bool {
        buffer < MAX_BUFFERS && self.held & (1 << buffer) != 0
    }

    fn set_held(&mut self, buffer: u32, held: bool) {
        if buffer < MAX_BUFFERS {
            if held {
                self.held |= 1 << buffer;
            } else {
                self.held &= !(1 << buffer);
            }
        }
    }

    /// Takes every buffer the stream has free or returned, at most one round of the stream.
    fn take_free<S: StreamEndpoint + ?Sized>(&mut self, stream: &mut S) -> Result<(), S::Error> {
        for _ in 0..stream.buffer_count() {
            let Some(buffer) = stream.next_buffer()? else {
                break;
            };
            self.set_held(buffer, true);
        }
        Ok(())
    }

    /// Takes the stream's free buffers and runs the jobs waiting on it, at `now_ns` on the
    /// caller's clock. Returns how many jobs it read. An error is the stream's, and leaves the
    /// jobs not yet read where they are. Allocates nothing unless a job fails.
    pub fn serve<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        now_ns: u64,
    ) -> Result<u32, S::Error> {
        self.take_free(stream)?;
        let mut encoded = [0u8; LONGEST_JOB];
        let mut read = 0;
        while read < MAX_JOBS_PER_ROUND {
            let Some(len) = stream.next_job(&mut encoded)? else {
                break;
            };
            read += 1;
            let job = encoded
                .get(..len)
                .filter(|bytes| bytes.len() <= JOB_BYTES)
                .and_then(|bytes| ScaleJob::decode(&mut Reader::new(bytes)).ok());
            let Some(job) = job else {
                // A broken consumer: it loses the stream, and no job of its is run after.
                self.fault(stream)?;
                break;
            };
            // Buffers held since before this consumer came have nothing behind them: they are
            // given up and taken again first, so that even a failed job completes to it.
            let renewed =
                self.renew(stream, job.input.buffer)? | self.renew(stream, job.output.buffer)?;
            if renewed {
                self.take_free(stream)?;
            }
            let outcome = self.resize(stream, &job, now_ns);
            self.complete(stream, &job, outcome, now_ns)?;
        }
        Ok(read)
    }

    /// Faults the consumer that posted bytes that are no job. Every buffer held was taken under
    /// it and has nothing behind it now, so each is given up, to be taken again under the next
    /// consumer.
    fn fault<S: StreamEndpoint + ?Sized>(&mut self, stream: &mut S) -> Result<(), S::Error> {
        self.failed(Failure::Undecodable);
        stream.fault_consumer()?;
        for buffer in 0..MAX_BUFFERS {
            if !self.holds(buffer) {
                continue;
            }
            // Nothing is behind it, so this reaches no one.
            stream.publish(StreamFrame {
                buffer,
                bytes: 0,
                sequence: 0,
                identity: 0,
                timestamp_ns: 0,
            })?;
            self.set_held(buffer, false);
        }
        Ok(())
    }

    fn failed(&mut self, failure: Failure) {
        self.counters.errors = self.counters.errors.saturating_add(1);
        self.last_failure = Some(failure);
    }

    /// Publishes the job's output as its completion: `bytes` long when it ran, empty when it
    /// failed. A job whose output isn't a buffer this driver holds has nothing to complete on.
    fn complete<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &ScaleJob,
        outcome: Result<u32, Failure>,
        now_ns: u64,
    ) -> Result<(), S::Error> {
        let frame = StreamFrame {
            buffer: job.output.buffer,
            bytes: 0,
            sequence: self.sequence,
            identity: job.identity,
            timestamp_ns: now_ns,
        };
        let mut outcome = outcome;
        let mut published = Ok(());
        if self.holds(job.output.buffer) && job.output.buffer != job.input.buffer {
            let delivered = match outcome {
                Ok(bytes) => stream.publish(StreamFrame { bytes, ..frame }).is_ok(),
                Err(_) => false,
            };
            if !delivered {
                // A frame longer than the stream lets one be was refused and the buffer is
                // still held: the completion says failed instead.
                outcome = outcome.and(Err(Failure::Completion));
                published = stream.publish(frame);
            }
            if published.is_ok() {
                self.set_held(job.output.buffer, false);
                self.sequence = self.sequence.wrapping_add(1);
            }
        }
        match outcome {
            Ok(_) => self.counters.jobs = self.counters.jobs.saturating_add(1),
            Err(failure) => self.failed(failure),
        }
        published
    }

    /// Gives up a buffer that has nothing behind it, so it can be taken again with something.
    fn renew<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        buffer: u32,
    ) -> Result<bool, S::Error> {
        if !self.holds(buffer) || !matches!(stream.buffer_handle(buffer), Ok(None)) {
            return Ok(false);
        }
        // Nothing is behind it, so this reaches no one.
        stream.publish(StreamFrame {
            buffer,
            bytes: 0,
            sequence: 0,
            identity: 0,
            timestamp_ns: 0,
        })?;
        self.set_held(buffer, false);
        Ok(true)
    }

    /// Runs one job. The bytes written to its output, or why it didn't run.
    fn resize<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &S,
        job: &ScaleJob,
        now_ns: u64,
    ) -> Result<u32, Failure> {
        let shape = |image: &ImageRef| -> Result<Shape, Failure> {
            Ok(Shape {
                width: image.width,
                height: image.height,
                stride: image.stride,
                format: format::<D>(image.fourcc).ok_or(Failure::Format)?,
            })
        };
        let (input, output) = (shape(&job.input)?, shape(&job.output)?);
        let (from, to) = (job.input.buffer, job.output.buffer);
        if from == to || !self.holds(from) || !self.holds(to) {
            return Err(Failure::Buffers);
        }
        let (Ok(Some(from)), Ok(Some(to))) = (stream.buffer_handle(from), stream.buffer_handle(to))
        else {
            return Err(Failure::NoHandle);
        };
        let outcome = self.ai2d.resize(
            now_ns,
            Request {
                dmabuf: from.fd(),
                shape: input,
            },
            Request {
                dmabuf: to.fd(),
                shape: output,
            },
            job.filter,
        );
        match outcome {
            // An output past 4 GiB fits no stream's buffer: the completion is refused.
            Ok(bytes) => Ok(u32::try_from(bytes).unwrap_or(u32::MAX)),
            Err(RunError::Refused(error)) => Err(error.into()),
            Err(RunError::Unsized(_)) => Err(Failure::NoHandle),
            Err(RunError::Device { .. } | RunError::BackingOff { .. }) => Err(Failure::Engine),
        }
    }
}

#[cfg(test)]
mod tests;
