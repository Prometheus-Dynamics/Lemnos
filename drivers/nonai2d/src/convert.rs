//! `image.convert/1` jobs, taken from a stream whose buffers are dma-bufs.
//!
//! The consumer names two buffers of the stream in a job. [`Converter::serve`] borrows their
//! handles from the stream, builds a checked [`Frame`] over each, runs the conversion, and
//! publishes the output buffer as the job's completion. No pixel is copied or read here.
//!
//! # What is checked
//!
//! Everything in a job is the consumer's, and none of it is trusted:
//!
//! - A buffer number must be one this driver holds. The stream lends a handle for no other.
//! - The format must be one of [`FORMATS`], the size one the engine keeps ([`Layout::new`]), and
//!   the stride exactly the row the engine reads or writes: the driver works its strides out
//!   from the width and ignores any it is given, so a padded row can't be described to it.
//! - The frame is checked against the real size of the dma-buf behind the handle, by
//!   [`Frame::new`], not against a size the job or the stream states. The engine is then given
//!   the dma-buf by descriptor, never an address.
//!
//! # Buffers
//!
//! The driver holds every buffer the stream has free, so a job can name any of them, and the
//! consumer fills an input while the driver holds it. The output is published, and is the
//! driver's again once the consumer returns it. The engine runs a conversion to its end before
//! `serve` goes on, so no buffer is published while the engine works on it.
//!
//! When the consumer changes, the buffers held from before have nothing behind them. A job
//! that names one makes the driver give it up and take the stream's free buffers again, which
//! are the new consumer's.
//!
//! # Formats
//!
//! The formats of one plane, by their DRM four-character codes, the codes other image drivers
//! take, so a consumer names a format the same way whichever block serves it. The 4:2:0 formats
//! are left out: the driver takes each of their planes from the start of its own dma-buf, and a
//! job names one buffer.
//!
//! Hardware status: untested on board, as the rest of this crate.

use std::io;

use lemnos_contract_image_convert::{ConversionPair, ConvertJob, Counters, ImageRef, JOB_BYTES};
use lemnos_core::{Decode, Reader};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;

use crate::engine::{Engine, Nonai2d, RunError};
use crate::frame::Frame;
use crate::layout::{Layout, PixelFormat};

/// Buffers of a stream this driver can hold. A job naming one past these is refused.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken in one [`Converter::serve`]; the rest wait for the next.
pub const MAX_JOBS_PER_ROUND: u32 = 64;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a [`ConvertJob`] is taken off the queue and counted, not left at its head.
const LONGEST_JOB: usize = 4096;

const fn code(name: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*name)
}

/// The formats a job may name, by DRM four-character code, and the engine's format each is.
/// The DRM code gives the layout of a little-endian word, the V4L2 name the order in memory, so
/// DRM `AR24` (`ARGB8888`) is V4L2 `AR24` (`ABGR32`): B, G, R, A in memory.
pub const FORMATS: [(u32, PixelFormat); 7] = [
    (code(b"AR24"), PixelFormat::Abgr32),
    (code(b"XR24"), PixelFormat::Xbgr32),
    (code(b"BA24"), PixelFormat::Argb32),
    (code(b"BX24"), PixelFormat::Xrgb32),
    (code(b"BG24"), PixelFormat::Rgb24),
    (code(b"RG24"), PixelFormat::Bgr24),
    (code(b"RG16"), PixelFormat::Rgb565),
];

/// The format a four-character code names, if it is one of [`FORMATS`].
pub fn format(fourcc: u32) -> Option<PixelFormat> {
    FORMATS
        .iter()
        .find(|(code, _)| *code == fourcc)
        .map(|(_, format)| *format)
}

/// Conversion `index` of the ones a job can ask for: every pair of [`FORMATS`], the same format
/// to itself included (a copy). `None` past the last.
pub fn conversion(index: u16) -> Option<ConversionPair> {
    let n = FORMATS.len();
    let index = usize::from(index);
    if index >= n * n {
        return None;
    }
    Some(ConversionPair {
        from: FORMATS[index / n].0,
        to: FORMATS[index % n].0,
    })
}

/// How many conversions [`conversion`] answers.
pub const CONVERSIONS: u16 = (FORMATS.len() * FORMATS.len()) as u16;

/// Why the last failed job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job.
    Undecodable,
    /// A buffer the driver doesn't hold, or the same buffer twice.
    Buffers,
    /// The input and the output differ in size, or the size is one the engine doesn't keep.
    Size,
    /// A format outside [`FORMATS`], or a stride other than the image's row.
    Format,
    /// The stream had no handle to lend: its buffers aren't dma-bufs, or the consumer went.
    NoHandle,
    /// The frame doesn't fit the dma-buf behind the buffer.
    Frame,
    /// The device refused the conversion.
    Refused,
    /// The engine failed, or is backing off after a failure.
    Engine,
    /// The stream refused the completion.
    Completion,
}

/// The layout of one image of a job, checked as far as the job alone allows.
fn layout(image: &ImageRef) -> Result<Layout, Failure> {
    let format = format(image.fourcc).ok_or(Failure::Format)?;
    let layout = Layout::new(format, image.width, image.height).map_err(|_| Failure::Size)?;
    match layout.planes().first() {
        Some(plane) if plane.stride == image.stride => Ok(layout),
        _ => Err(Failure::Format),
    }
}

/// Serves `image.convert/1` jobs on the engine.
pub struct Converter<E, F> {
    engine: Nonai2d<E, F>,
    /// The stream's buffers this driver holds, one bit each.
    held: u64,
    counters: Counters,
    last_failure: Option<Failure>,
    sequence: u64,
}

impl<E: Engine, F> std::fmt::Debug for Converter<E, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Converter")
            .field("engine", &self.engine)
            .field("held", &self.held)
            .field("counters", &self.counters)
            .field("last_failure", &self.last_failure)
            .finish()
    }
}

impl<E: Engine, F: FnMut(Layout, Layout) -> io::Result<E>> Converter<E, F> {
    /// A converter on `engine`, holding nothing yet.
    pub fn new(engine: Nonai2d<E, F>) -> Self {
        Self {
            engine,
            held: 0,
            counters: Counters::default(),
            last_failure: None,
            sequence: 0,
        }
    }

    /// The engine.
    pub fn engine(&self) -> &Nonai2d<E, F> {
        &self.engine
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
    /// jobs not yet read where they are. Allocates nothing unless a job fails or needs the
    /// engine opened for other layouts.
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
                .and_then(|bytes| ConvertJob::decode(&mut Reader::new(bytes)).ok());
            let Some(job) = job else {
                self.failed(Failure::Undecodable);
                continue;
            };
            // Buffers held since before this consumer came have nothing behind them: they are
            // given up and taken again first, so that even a failed job completes to it.
            let renewed =
                self.renew(stream, job.input.buffer)? | self.renew(stream, job.output.buffer)?;
            if renewed {
                self.take_free(stream)?;
            }
            let outcome = self.convert(stream, &job, now_ns);
            self.complete(stream, &job, outcome, now_ns)?;
        }
        Ok(read)
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
        job: &ConvertJob,
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
    fn convert<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &S,
        job: &ConvertJob,
        now_ns: u64,
    ) -> Result<u32, Failure> {
        let input = layout(&job.input)?;
        let output = layout(&job.output)?;
        if input.size() != output.size() {
            return Err(Failure::Size);
        }
        let (from, to) = (job.input.buffer, job.output.buffer);
        if from == to || !self.holds(from) || !self.holds(to) {
            return Err(Failure::Buffers);
        }
        let (Ok(Some(from)), Ok(Some(to))) = (stream.buffer_handle(from), stream.buffer_handle(to))
        else {
            return Err(Failure::NoHandle);
        };
        // The frames are checked against the dma-bufs' real sizes here.
        let frames = (
            Frame::new(input, &[from.fd()]),
            Frame::new(output, &[to.fd()]),
        );
        let (Ok(src), Ok(dst)) = frames else {
            return Err(Failure::Frame);
        };
        // The engine has finished with both buffers when this returns.
        match self.engine.run(now_ns, &src, &dst) {
            Ok(()) => Ok(output.bytes()),
            Err(RunError::Refused(_)) => Err(Failure::Refused),
            Err(_) => Err(Failure::Engine),
        }
    }
}

#[cfg(test)]
mod tests;
