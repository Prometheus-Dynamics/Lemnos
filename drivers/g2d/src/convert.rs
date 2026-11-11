//! `image.convert/1` jobs, taken from a stream whose buffers are dma-bufs.
//!
//! The consumer names two buffers of the stream in a job. [`Converter::serve`] borrows their
//! handles from the stream, builds the engine's checked [`Job`] over them, runs it, and publishes
//! the output buffer as the job's completion. No pixel is copied or read here.
//!
//! # What is checked
//!
//! Everything in a job is the consumer's, and none of it is trusted:
//!
//! - A buffer number must be one this driver holds. The stream lends a handle for no other.
//! - The frame a job describes (stride, height, format) is checked against the real size of the
//!   dma-buf behind the handle, by [`Surface::new`], not against a size the job or the stream
//!   states. The rectangle the engine is told to read or write is checked against that frame.
//!   The request then names the buffer by descriptor and position only, never by an address.
//!
//! # Buffers
//!
//! The driver holds every buffer the stream has free, so a job can name any of them, and the
//! consumer fills an input while the driver holds it. The output is published, and is the
//! driver's again once the consumer returns it. The engine runs a job to its end before `serve`
//! goes on, so no buffer is published while the engine works on it.
//!
//! When the consumer changes, the buffers held from before have nothing behind them. A job
//! that names one makes the driver give it up and take the stream's free buffers again, which
//! are the new consumer's.
//!
//! A consumer that posts bytes that are no job is broken, as the contract says: the driver
//! faults its stream ([`StreamEndpoint::fault_consumer`]), runs nothing more of its, and gives
//! up every buffer held under it.
//!
//! # Formats
//!
//! The packed RGB formats, by their DRM four-character codes. YUV is left out: a job says
//! nothing of colour space or range, which a conversion to or from YUV has to be told.
//!
//! Hardware status: untested on board, as the rest of this crate. The byte order behind each
//! code is the kernel header's naming and has not been compared on an engine.

use std::io;

use lemnos_contract_image_convert::{ConvertJob, Counters, ImageRef, JOB_BYTES};
use lemnos_core::{Decode, Reader};
use lemnos_driver_g2d_sys::{ColorRange, Gamut, Job, PixelFormat, Rect, Surface};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;

use crate::engine::{Engine, G2d};

/// Buffers of a stream this driver can hold. A job naming one past these is refused.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken in one [`Converter::serve`]; the rest wait for the next.
pub const MAX_JOBS_PER_ROUND: u32 = 64;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a [`ConvertJob`] is taken off the queue and faults its consumer, not left at its head.
const LONGEST_JOB: usize = 4096;

const fn code(name: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*name)
}

/// The formats a job may name, by four-character code.
pub const FORMATS: [(u32, PixelFormat); 11] = [
    (code(b"AR24"), PixelFormat::Argb8888),
    (code(b"AB24"), PixelFormat::Abgr8888),
    (code(b"RA24"), PixelFormat::Rgba8888),
    (code(b"BA24"), PixelFormat::Bgra8888),
    (code(b"XR24"), PixelFormat::Xrgb8888),
    (code(b"XB24"), PixelFormat::Xbgr8888),
    (code(b"RX24"), PixelFormat::Rgbx8888),
    (code(b"BX24"), PixelFormat::Bgrx8888),
    (code(b"RG24"), PixelFormat::Rgb888),
    (code(b"BG24"), PixelFormat::Bgr888),
    (code(b"RG16"), PixelFormat::Rgb565),
];

/// The format a four-character code names, if it is one of [`FORMATS`].
pub fn format(fourcc: u32) -> Option<PixelFormat> {
    FORMATS
        .iter()
        .find(|(code, _)| *code == fourcc)
        .map(|(_, format)| *format)
}

/// Why the last failed job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job. The consumer's stream was faulted.
    Undecodable,
    /// A buffer the driver doesn't hold, or the same buffer twice.
    Buffers,
    /// The input and the output differ in size.
    Size,
    /// A format outside [`FORMATS`], or a stride that isn't whole pixels or is narrower than
    /// the image.
    Format,
    /// The stream had no handle to lend: its buffers aren't dma-bufs, or the consumer went.
    NoHandle,
    /// The frame doesn't fit the dma-buf behind the buffer.
    Surface,
    /// The engine refused the job or failed.
    Engine,
    /// The stream refused the completion.
    Completion,
}

/// One image of a job, checked as far as the job alone allows.
#[derive(Clone, Copy)]
struct Plane {
    buffer: u32,
    format: PixelFormat,
    /// The surface's width in pixels: the stride.
    pitch: u32,
    /// Bytes the frame takes in its buffer.
    bytes: u32,
}

fn plane(image: &ImageRef) -> Result<Plane, Failure> {
    let format = format(image.fourcc).ok_or(Failure::Format)?;
    let pixel = u32::try_from(format.frame_len(1, 1)).map_err(|_| Failure::Format)?;
    if pixel == 0 || !image.stride.is_multiple_of(pixel) || image.stride / pixel < image.width {
        return Err(Failure::Format);
    }
    let bytes = u64::from(image.stride) * u64::from(image.height);
    Ok(Plane {
        buffer: image.buffer,
        format,
        pitch: image.stride / pixel,
        // A frame this long fits no dma-buf the engine takes, so the surface is refused later
        // whatever is put here.
        bytes: u32::try_from(bytes).unwrap_or(u32::MAX),
    })
}

/// Serves `image.convert/1` jobs on the engine.
pub struct Converter<E, F> {
    g2d: G2d<E, F>,
    /// The stream's buffers this driver holds, one bit each.
    held: u64,
    counters: Counters,
    last_failure: Option<Failure>,
    sequence: u64,
}

impl<E, F> std::fmt::Debug for Converter<E, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Converter")
            .field("g2d", &self.g2d)
            .field("held", &self.held)
            .field("counters", &self.counters)
            .field("last_failure", &self.last_failure)
            .finish()
    }
}

impl<E: Engine, F: FnMut() -> io::Result<E>> Converter<E, F> {
    /// A converter on `g2d`, holding nothing yet.
    pub fn new(g2d: G2d<E, F>) -> Self {
        Self {
            g2d,
            held: 0,
            counters: Counters::default(),
            last_failure: None,
            sequence: 0,
        }
    }

    /// The engine.
    pub fn engine(&self) -> &G2d<E, F> {
        &self.g2d
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
                .and_then(|bytes| ConvertJob::decode(&mut Reader::new(bytes)).ok());
            let Some(job) = job else {
                // A broken consumer: it loses the stream, and no job of its is run after.
                self.fault(stream)?;
                break;
            };
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look.
            self.take_free(stream)?;
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
        let input = plane(&job.input)?;
        let output = plane(&job.output)?;
        if job.input.width != job.output.width || job.input.height != job.output.height {
            return Err(Failure::Size);
        }
        if input.buffer == output.buffer || !self.holds(input.buffer) || !self.holds(output.buffer)
        {
            return Err(Failure::Buffers);
        }
        let (Ok(Some(from)), Ok(Some(to))) = (
            stream.buffer_handle(input.buffer),
            stream.buffer_handle(output.buffer),
        ) else {
            return Err(Failure::NoHandle);
        };
        let rect = Rect {
            x: 0,
            y: 0,
            w: job.input.width,
            h: job.input.height,
        };
        // The frames are checked against the dma-bufs' real sizes here.
        let surfaces = (
            Surface::new(from.fd(), input.format, input.pitch, job.input.height),
            Surface::new(to.fd(), output.format, output.pitch, job.output.height),
        );
        let (Ok(src), Ok(dst)) = surfaces else {
            return Err(Failure::Surface);
        };
        let request = Job::blit(&src, rect, &dst, rect, Gamut::Bt601, ColorRange::Full)
            .map_err(|_| Failure::Surface)?;
        // The engine has finished with both buffers when this returns.
        match self.g2d.run(now_ns, &request) {
            Ok(()) => Ok(output.bytes),
            Err(_) => Err(Failure::Engine),
        }
    }
}

#[cfg(test)]
mod tests;
