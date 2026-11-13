//! `image.scaler/1` and `image.convert/1` jobs, each taken from its own stream whose buffers are
//! dma-bufs, run on the GPU.
//!
//! The consumer names buffers of the stream in a job. [`Lane::serve`] borrows their handles
//! from the stream, builds a checked [`Frame`] over each, runs the operation on the [`Gpu`],
//! and publishes the output buffer as the job's completion. No pixel is read or computed here:
//! the GPU's staging copies (see the crate docs) are the only CPU work on the pixels, and they
//! are what the kernel's interface leaves.
//!
//! # What is checked
//!
//! Everything in a job is the consumer's, and none of it is trusted. Each path by which a job's
//! bytes become something the GPU or the CPU's copy uses, and its check:
//!
//! - **Buffer numbers** become dma-buf descriptors: a number must be one this lane holds
//!   (taken from the stream with `next_buffer` and not yet published), the two must differ, and
//!   the descriptor is the one the stream lends for that number (`buffer_handle`), never one
//!   the job names.
//! - **Width, height and stride** become the extent the copy into and out of staging reads and
//!   writes, and the GPU's size words: the fourcc must be one of [`FORMATS`], each side 1 to
//!   [`MAX_SIDE`](crate::MAX_SIDE), the stride at least a row, and the whole frame
//!   (`(height - 1) * stride + row`, in 64-bit arithmetic from 16- and 32-bit factors, so it
//!   cannot overflow) inside the dma-buf's real size, asked of the kernel (`lseek` to its end),
//!   not taken from the job or the stream. [`Vglite`](crate::Vglite) checks it again when it
//!   maps the dma-buf, mapping only that many bytes.
//! - **The staging buffers** the GPU reads and writes are sized from the checked image by the
//!   GPU's own allocator, and the command stream checks every address and extent it names
//!   against them (`lemnos-driver-vglite-sys`). No address from a job reaches the GPU.
//! - **The operation**: a scale keeps the format and changes the size (the area filter only
//!   by exactly 2); a conversion keeps the size and changes the format among
//!   [`CONVERTIBLE`](crate::CONVERTIBLE). Anything else completes as failed.
//!
//! # Buffers
//!
//! The lane holds every buffer the stream has free, so a job can name any of them, and the
//! consumer fills an input while the lane holds it. The output is published, and is the
//! lane's again once the consumer returns it. The GPU runs a job to its end before `serve` goes
//! on, so no buffer is published while the GPU works on it. When the consumer changes, buffers
//! held from before have nothing behind them: a job that names one makes the lane give it up
//! and take the stream's free buffers again. A consumer that posts bytes that are no job loses
//! its stream ([`StreamEndpoint::fault_consumer`]), and every buffer held under it is given up.

use std::os::fd::BorrowedFd;

use lemnos_contract_image_convert::{self as convert, ConvertJob};
use lemnos_contract_image_scaler::{self as scaler, ScaleJob};
use lemnos_core::{Decode, Reader};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_linux_sys::dma_buf;

use crate::gpu::{RunError, State, Vglite};
use crate::plan::{Filter, Format, Image};
use crate::surface::{Frame, Transfer};

/// Buffers of a stream a lane can hold. A job naming one past these is refused.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken from one stream in one [`Lane::serve`]; the rest wait for the next.
pub const MAX_JOBS_PER_ROUND: u32 = 64;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a job is taken off the queue and faults its consumer, not left at its head.
const LONGEST_JOB: usize = 4096;

const fn code(name: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*name)
}

/// The formats a job may name, by DRM four-character code, and the GPU's format each is. The
/// DRM code gives the layout of a little-endian word, the GPU's name the order in memory, so
/// DRM `AB24` (`ABGR8888`) is R, G, B, A in memory: [`Format::Rgba8888`].
pub const FORMATS: [(u32, Format); 4] = [
    (code(b"AB24"), Format::Rgba8888),
    (code(b"AR24"), Format::Bgra8888),
    (code(b"XB24"), Format::Rgbx8888),
    (code(b"R8  "), Format::L8),
];

/// The format a four-character code names, if it is one of [`FORMATS`].
pub fn format(fourcc: u32) -> Option<Format> {
    FORMATS
        .iter()
        .find(|(code, _)| *code == fourcc)
        .map(|(_, format)| *format)
}

/// The four-character code of a format of [`FORMATS`].
pub fn fourcc(format: Format) -> Option<u32> {
    FORMATS
        .iter()
        .find(|(_, f)| *f == format)
        .map(|(code, _)| *code)
}

/// The conversions a job can ask for, in the order `image.convert/1` lists them: every pair of
/// [`CONVERTIBLE`](crate::CONVERTIBLE), then each other format of [`FORMATS`] to itself.
pub fn conversion(index: u16) -> Option<(u32, u32)> {
    let four = crate::plan::CONVERTIBLE;
    let n = four.len();
    let index = usize::from(index);
    if index < n * n {
        return Some((fourcc(four[index / n])?, fourcc(four[index % n])?));
    }
    let same = FORMATS
        .iter()
        .filter(|(_, f)| !four.contains(f))
        .nth(index - n * n)?;
    Some((same.0, same.0))
}

/// How many conversions [`conversion`] answers.
pub fn conversions() -> u16 {
    let four = crate::plan::CONVERTIBLE.len();
    (four * four + FORMATS.len() - four) as u16
}

/// What runs a job: [`Vglite`] on Linux, something that plays one back in tests.
pub trait Gpu {
    /// Where the GPU is.
    fn state(&self) -> State;
    /// A resize from `src` into `dst`.
    fn resize(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
        filter: Filter,
    ) -> Result<Transfer, RunError>;
    /// A conversion from `src` into `dst`.
    fn convert(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
    ) -> Result<Transfer, RunError>;
}

impl Gpu for Vglite {
    fn state(&self) -> State {
        Vglite::state(self)
    }

    fn resize(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
        filter: Filter,
    ) -> Result<Transfer, RunError> {
        self.resize_dmabuf(now_ns, src, dst, filter)
    }

    fn convert(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
    ) -> Result<Transfer, RunError> {
        self.convert_dmabuf(now_ns, src, dst)
    }
}

impl<G: Gpu + ?Sized> Gpu for Box<G> {
    fn state(&self) -> State {
        (**self).state()
    }

    fn resize(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
        filter: Filter,
    ) -> Result<Transfer, RunError> {
        (**self).resize(now_ns, src, dst, filter)
    }

    fn convert(
        &mut self,
        now_ns: u64,
        src: Frame<'_>,
        dst: Frame<'_>,
    ) -> Result<Transfer, RunError> {
        (**self).convert(now_ns, src, dst)
    }
}

/// Which contract a lane serves, and so which job its stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contract {
    /// `image.scaler/1`: [`ScaleJob`].
    Scale,
    /// `image.convert/1`: [`ConvertJob`].
    Convert,
}

/// Why the last failed job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job. The consumer's stream was faulted.
    Undecodable,
    /// A buffer the lane doesn't hold, or the same buffer twice.
    Buffers,
    /// A format outside [`FORMATS`], or a scale between two formats.
    Format,
    /// A side that is 0 or past the GPU's, a stride shorter than a row, or a conversion that
    /// changes the size.
    Size,
    /// The stream had no handle to lend: its buffers aren't dma-bufs, or the consumer went.
    NoHandle,
    /// The frame doesn't fit the dma-buf behind the buffer.
    Frame,
    /// The GPU refused the operation: one it cannot do as asked (an area scale other than 2x,
    /// a bilinear scale to the same size, a pair of formats it doesn't convert between).
    Refused,
    /// The GPU failed, or is backing off after a failure.
    Engine,
    /// The stream refused the completion.
    Completion,
}

/// One image of a job, as the job names it.
#[derive(Debug, Clone, Copy)]
struct Named {
    buffer: u32,
    width: u32,
    height: u32,
    fourcc: u32,
    stride: u32,
}

impl From<scaler::ImageRef> for Named {
    fn from(i: scaler::ImageRef) -> Self {
        Self {
            buffer: i.buffer,
            width: i.width,
            height: i.height,
            fourcc: i.fourcc,
            stride: i.stride,
        }
    }
}

impl From<convert::ImageRef> for Named {
    fn from(i: convert::ImageRef) -> Self {
        Self {
            buffer: i.buffer,
            width: i.width,
            height: i.height,
            fourcc: i.fourcc,
            stride: i.stride,
        }
    }
}

impl Named {
    /// The image, checked as far as the job alone allows.
    fn image(&self) -> Result<Image, Failure> {
        let image = Image {
            format: format(self.fourcc).ok_or(Failure::Format)?,
            width: self.width,
            height: self.height,
            stride: self.stride,
        };
        image.check(usize::MAX).map_err(|_| Failure::Size)?;
        Ok(image)
    }
}

/// A job of either contract.
#[derive(Debug, Clone, Copy)]
struct Job {
    identity: u64,
    input: Named,
    output: Named,
    /// The filter of a scale; `None` for a conversion.
    filter: Option<Filter>,
}

impl Job {
    fn decode(contract: Contract, bytes: &[u8]) -> Option<Self> {
        match contract {
            Contract::Scale => {
                let job = bytes
                    .get(..)
                    .filter(|b| b.len() <= scaler::JOB_BYTES)
                    .and_then(|b| ScaleJob::decode(&mut Reader::new(b)).ok())?;
                Some(Self {
                    identity: job.identity,
                    input: job.input.into(),
                    output: job.output.into(),
                    filter: Some(match job.filter {
                        scaler::Filter::Area => Filter::Area,
                        scaler::Filter::Bilinear => Filter::Bilinear,
                    }),
                })
            }
            Contract::Convert => {
                let job = bytes
                    .get(..)
                    .filter(|b| b.len() <= convert::JOB_BYTES)
                    .and_then(|b| ConvertJob::decode(&mut Reader::new(b)).ok())?;
                Some(Self {
                    identity: job.identity,
                    input: job.input.into(),
                    output: job.output.into(),
                    filter: None,
                })
            }
        }
    }
}

/// A frame over `fd`, checked to lie inside the dma-buf's real size.
fn frame(image: Image, fd: BorrowedFd<'_>) -> Result<Frame<'_>, Failure> {
    let size = dma_buf::size(fd).map_err(|_| Failure::Frame)?;
    let len = usize::try_from(size).unwrap_or(usize::MAX);
    image.check(len).map_err(|_| Failure::Frame)?;
    Ok(Frame { image, fd })
}

/// Jobs completed and failed on one lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Jobs completed with an output.
    pub jobs: u64,
    /// Jobs completed as failed.
    pub errors: u64,
}

/// One contract's job stream: the buffers held from it and its counters.
#[derive(Debug, Default)]
pub struct Lane {
    /// The stream's buffers this lane holds, one bit each.
    held: u64,
    counts: Counts,
    last_failure: Option<Failure>,
    sequence: u64,
}

impl Lane {
    /// Jobs completed and failed since this was made.
    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Why the last failed job failed.
    pub fn last_failure(&self) -> Option<Failure> {
        self.last_failure
    }

    /// Whether the lane holds `buffer`.
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

    /// Takes the stream's free buffers and runs the `contract` jobs waiting on it on `gpu`, at
    /// `now_ns` on the caller's clock. Returns how many jobs it read. An error is the stream's,
    /// and leaves the jobs not yet read where they are. Allocates nothing unless a job fails.
    pub fn serve<S: StreamEndpoint + ?Sized, G: Gpu + ?Sized>(
        &mut self,
        contract: Contract,
        stream: &mut S,
        gpu: &mut G,
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
                .and_then(|bytes| Job::decode(contract, bytes));
            let Some(job) = job else {
                // A broken consumer: it loses the stream, and no job of its is run after.
                self.fault(stream)?;
                break;
            };
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look.
            self.take_free(stream)?;
            let renewed =
                self.renew(stream, job.input.buffer)? | self.renew(stream, job.output.buffer)?;
            if renewed {
                self.take_free(stream)?;
            }
            let outcome = self.run(stream, gpu, &job, now_ns);
            self.complete(stream, &job, outcome, now_ns)?;
        }
        Ok(read)
    }

    /// Faults the consumer that posted bytes that are no job, and gives up every buffer held
    /// under it.
    fn fault<S: StreamEndpoint + ?Sized>(&mut self, stream: &mut S) -> Result<(), S::Error> {
        self.failed(Failure::Undecodable);
        stream.fault_consumer()?;
        for buffer in 0..MAX_BUFFERS {
            if !self.holds(buffer) {
                continue;
            }
            // Nothing is behind it, so this reaches no one.
            stream.publish(empty(buffer))?;
            self.set_held(buffer, false);
        }
        Ok(())
    }

    fn failed(&mut self, failure: Failure) {
        self.counts.errors = self.counts.errors.saturating_add(1);
        self.last_failure = Some(failure);
    }

    /// Publishes the job's output as its completion: `bytes` long when it ran, empty when it
    /// failed. A job whose output isn't a buffer this lane holds has nothing to complete on.
    fn complete<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &Job,
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
                outcome = outcome.and(Err(Failure::Completion));
                published = stream.publish(frame);
            }
            if published.is_ok() {
                self.set_held(job.output.buffer, false);
                self.sequence = self.sequence.wrapping_add(1);
            }
        }
        match outcome {
            Ok(_) => self.counts.jobs = self.counts.jobs.saturating_add(1),
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
        stream.publish(empty(buffer))?;
        self.set_held(buffer, false);
        Ok(true)
    }

    /// Runs one job. The bytes written to its output, or why it didn't run.
    fn run<S: StreamEndpoint + ?Sized, G: Gpu + ?Sized>(
        &mut self,
        stream: &S,
        gpu: &mut G,
        job: &Job,
        now_ns: u64,
    ) -> Result<u32, Failure> {
        let input = job.input.image()?;
        let output = job.output.image()?;
        match job.filter {
            Some(_) if input.format != output.format => return Err(Failure::Format),
            None if (input.width, input.height) != (output.width, output.height) => {
                return Err(Failure::Size);
            }
            _ => {}
        }
        let (from, to) = (job.input.buffer, job.output.buffer);
        if from == to || !self.holds(from) || !self.holds(to) {
            return Err(Failure::Buffers);
        }
        let (Ok(Some(from)), Ok(Some(to))) = (stream.buffer_handle(from), stream.buffer_handle(to))
        else {
            return Err(Failure::NoHandle);
        };
        let src = frame(input, from.fd())?;
        let dst = frame(output, to.fd())?;
        // The GPU has finished with both buffers when this returns.
        let ran = match job.filter {
            Some(filter) => gpu.resize(now_ns, src, dst, filter),
            None => gpu.convert(now_ns, src, dst),
        };
        match ran {
            Ok(_) => u32::try_from(output.frame_len()).map_err(|_| Failure::Size),
            Err(RunError::Refused(_)) => Err(Failure::Refused),
            Err(_) => Err(Failure::Engine),
        }
    }
}

/// A completion with nothing in it, for a buffer given back.
fn empty(buffer: u32) -> StreamFrame {
    StreamFrame {
        buffer,
        bytes: 0,
        sequence: 0,
        identity: 0,
        timestamp_ns: 0,
    }
}
