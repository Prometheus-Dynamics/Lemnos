//! `image.scaler/1` jobs, taken from a stream whose buffers are dma-bufs, on one output channel.
//!
//! The consumer names two buffers of the stream in a job. [`Jobs::serve`] borrows their handles
//! from the stream, builds the scaler's checked [`Job`] over them, runs it, and publishes the
//! output buffer as the job's completion. No pixel is copied or read here.
//!
//! # What is checked
//!
//! Everything in a job is the consumer's, and none of it is trusted:
//!
//! - A buffer number must be one this driver holds and is free to use, and the two must
//!   differ. The stream lends a handle for no other.
//! - Both images have the same format, one the block reads and the channel writes, and the
//!   filter is [`Filter::Bilinear`], the block's scaler (`bilinear_scale` in the vendor
//!   driver's `vpss_offline_v20.c`; its average scaler is left out, so [`Filter::Area`] is
//!   refused, never substituted).
//! - Each image is measured against the real size of the dma-buf behind its buffer, by
//!   [`Surface::new`], with its stride; the size, stride alignment, crop and scaling ratio are
//!   checked by [`Job::new`]. The request names the buffers by descriptor only.
//!
//! # When the block may still be writing
//!
//! The kernel's request waits for the frame, and gives up after 500 ms with `EAGAIN` without
//! stopping the block (`rkvpss_ofl_run` in `vpss_offline_v20.c`, rockchip-linux/kernel
//! `develop-6.1`), and the node's close, which follows a failure here, unmaps the buffers
//! without waiting either. So a job whose request failed after the node was open may have left
//! the block writing its output. That output is held back:
//!
//! - The driver takes its own hold on the output's dma-buf (a duplicate of the descriptor)
//!   before each request, so the memory stays alive whatever the stream or the kernel does with
//!   theirs. A job that succeeds lets it go at once.
//! - After a failure the output buffer is neither published nor used by another job, and the
//!   job's completion waits. Both wait for the block to be seen finishing a frame: the block
//!   runs one frame at a time, so a later frame's end means the earlier one is over. That frame
//!   is the next job that runs, or, when none comes, one the driver runs itself from the memory
//!   it holds (32 by 32 NV12 into a held-back dma-buf, which nobody reads), each time the
//!   scaler's backoff allows. Then every held-back output is published as its job's failed
//!   completion and every hold is let go.
//! - If the consumer goes meanwhile, a held-back buffer is given up to the stream (that reaches
//!   no one), and the hold on its memory is kept until the frame end all the same.
//!
//! # Buffers
//!
//! As the other image drivers do it: the driver holds every buffer the stream has free; the
//! output is published and is the driver's again once returned; buffers held from before a new
//! consumer are given up and taken again, and the dma-bufs the kernel kept attached for the old
//! consumer are released (`release_buffers`) once nothing is held back. A consumer that posts
//! bytes that are no job is faulted, as the contract says, and is such an old consumer from then.
//!
//! # Allocation
//!
//! A job allocates nothing unless it fails. Each job duplicates and closes one descriptor.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use lemnos_contract_image_scaler::{Counters, Filter, ImageRef, JOB_BYTES, ScaleJob};
use lemnos_core::{Decode, Reader};
use lemnos_driver_rkvpss_sys::{Channel, Crop, Job, MIN_SIZE, Output, PixelFormat, Surface};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;

use crate::device::Device;
use crate::scaler::{RunError, Scaler};

/// Buffers of a stream this driver can hold. A job naming one past these is refused.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken in one [`Jobs::serve`]; the rest wait for the next.
pub const MAX_JOBS_PER_ROUND: u32 = 64;

/// Holds on memory the block may still write, at most. A job finds one free before it runs.
pub const MAX_HOLDS: usize = 64;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a [`ScaleJob`] is taken off the queue and faults its consumer, not left at its head.
const LONGEST_JOB: usize = 4096;

/// The frame the driver runs to see the block finish one: the smallest square, in NV12, which
/// every channel writes.
const PROBE: (PixelFormat, u32) = (PixelFormat::Nv12, MIN_SIZE);

/// Why the last failed job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job. The consumer's stream was faulted.
    Undecodable,
    /// A buffer the driver doesn't hold or holds back, or the same buffer twice.
    Buffers,
    /// A format the channel doesn't take, two formats in one job, or the area filter.
    Format,
    /// The stream had no handle to lend: its buffers aren't dma-bufs, or the consumer went.
    NoHandle,
    /// An image doesn't fit the dma-buf behind its buffer, or the block's limits.
    Surface,
    /// Every hold is taken by outputs the block may still write.
    Held,
    /// The scaler refused the job, failed, or is backing off after a failure.
    Engine,
    /// The stream refused the completion.
    Completion,
}

/// Serves `image.scaler/1` jobs on one channel of the scaler.
pub struct Jobs<D: Device, F: FnMut() -> io::Result<D>> {
    scaler: Scaler<D, F>,
    channel: Channel,
    /// The stream's buffers this driver holds, one bit each.
    held: u64,
    /// Held buffers whose job failed while the block may still be writing them.
    held_back: u64,
    /// The identity of the job each held-back buffer completes.
    waiting: [u64; MAX_BUFFERS as usize],
    /// The driver's own holds on memory the block may still write.
    holds: [Option<OwnedFd>; MAX_HOLDS],
    /// The kernel keeps dma-bufs attached from a consumer that has gone.
    stale: bool,
    counters: Counters,
    last_failure: Option<Failure>,
    sequence: u64,
}

impl<D: Device, F: FnMut() -> io::Result<D>> std::fmt::Debug for Jobs<D, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs")
            .field("state", &self.scaler.state())
            .field("channel", &self.channel)
            .field("held", &self.held)
            .field("held_back", &self.held_back)
            .field("holds", &self.holds_taken())
            .field("counters", &self.counters)
            .field("last_failure", &self.last_failure)
            .finish()
    }
}

/// Whether bit `index` of `mask` is set; `false` past the mask.
const fn has(mask: u64, index: u32) -> bool {
    index < MAX_BUFFERS && mask & (1 << index) != 0
}

/// `mask` with bit `index` set or cleared; unchanged past the mask.
const fn with(mask: u64, index: u32, set: bool) -> u64 {
    if index >= MAX_BUFFERS {
        mask
    } else if set {
        mask | (1 << index)
    } else {
        mask & !(1 << index)
    }
}

/// An empty completion or give-up of `buffer`.
const fn empty(buffer: u32, identity: u64, sequence: u64, now_ns: u64) -> StreamFrame {
    StreamFrame {
        buffer,
        bytes: 0,
        sequence,
        identity,
        timestamp_ns: now_ns,
    }
}

/// One image of a job, checked as far as the job alone allows.
fn format_of(image: &ImageRef, channel: Channel) -> Result<PixelFormat, Failure> {
    PixelFormat::from_fourcc(image.fourcc)
        .filter(|f| f.is_input() && f.is_output_of(channel))
        .ok_or(Failure::Format)
}

impl<D: Device, F: FnMut() -> io::Result<D>> Jobs<D, F> {
    /// Jobs on `channel` of `scaler`, holding nothing yet. The scaler must hold the channel.
    pub fn new(scaler: Scaler<D, F>, channel: Channel) -> Self {
        Self {
            scaler,
            channel,
            held: 0,
            held_back: 0,
            waiting: [0; MAX_BUFFERS as usize],
            holds: std::array::from_fn(|_| None),
            stale: false,
            counters: Counters::default(),
            last_failure: None,
            sequence: 0,
        }
    }

    /// The scaler.
    pub fn scaler(&self) -> &Scaler<D, F> {
        &self.scaler
    }

    /// The channel jobs are written by.
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// The formats a job may use, in [`PixelFormat::ALL`]'s order.
    pub fn formats(&self) -> impl Iterator<Item = PixelFormat> + '_ {
        PixelFormat::ALL
            .into_iter()
            .filter(|f| f.is_input() && f.is_output_of(self.channel))
    }

    /// Jobs completed and failed since this was made. A held-back job is counted as failed
    /// when it fails, before its completion goes out.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Why the last failed job failed.
    pub fn last_failure(&self) -> Option<Failure> {
        self.last_failure
    }

    /// Whether the driver holds `buffer`.
    pub fn holds(&self, buffer: u32) -> bool {
        has(self.held, buffer)
    }

    /// Whether `buffer` is held back: its job failed while the block may still be writing it.
    pub fn held_back(&self, buffer: u32) -> bool {
        has(self.held_back, buffer)
    }

    /// The holds taken on memory the block may still write.
    pub fn holds_taken(&self) -> usize {
        self.holds.iter().flatten().count()
    }

    /// Takes every buffer the stream has free or returned, at most one round of the stream.
    fn take_free<S: StreamEndpoint + ?Sized>(&mut self, stream: &mut S) -> Result<(), S::Error> {
        for _ in 0..stream.buffer_count() {
            let Some(buffer) = stream.next_buffer()? else {
                break;
            };
            self.held = with(self.held, buffer, true);
        }
        Ok(())
    }

    /// Takes the stream's free buffers, tries to see the block finish a frame if something is
    /// held back, and runs the jobs waiting, at `now_ns` on the caller's clock. Returns how many
    /// jobs it read. An error is the stream's, and leaves the jobs not yet read where they are.
    /// Allocates nothing unless a job fails.
    pub fn serve<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        now_ns: u64,
    ) -> Result<u32, S::Error> {
        self.take_free(stream)?;
        self.probe(stream, now_ns)?;
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
            match self.resize(stream, &job, now_ns) {
                Ran::Done(bytes) => {
                    self.settle(stream, now_ns)?;
                    self.complete(stream, &job, Ok(bytes), now_ns)?;
                }
                Ran::Failed(failure) => self.complete(stream, &job, Err(failure), now_ns)?,
                Ran::HeldBack => {
                    let output = job.output.buffer;
                    self.held_back = with(self.held_back, output, true);
                    if let Some(slot) = self.waiting.get_mut(output as usize) {
                        *slot = job.identity;
                    }
                    self.failed(Failure::Engine);
                }
            }
        }
        if self.stale && self.holds_taken() == 0 && self.scaler.release_buffers(now_ns).is_ok() {
            self.stale = false;
        }
        Ok(read)
    }

    /// Faults the consumer that posted bytes that are no job. Every buffer held was taken under
    /// it and has nothing behind it now, so each is given up as [`renew`](Self::renew) does: a
    /// held-back one too, and the hold on its memory stays until the block ends a frame.
    fn fault<S: StreamEndpoint + ?Sized>(&mut self, stream: &mut S) -> Result<(), S::Error> {
        self.failed(Failure::Undecodable);
        stream.fault_consumer()?;
        for buffer in 0..MAX_BUFFERS {
            if !self.holds(buffer) {
                continue;
            }
            // Nothing is behind it, so this reaches no one.
            stream.publish(empty(buffer, 0, 0, 0))?;
            self.held = with(self.held, buffer, false);
            self.held_back = with(self.held_back, buffer, false);
            self.stale = true;
        }
        Ok(())
    }

    fn failed(&mut self, failure: Failure) {
        self.counters.errors = self.counters.errors.saturating_add(1);
        self.last_failure = Some(failure);
    }

    /// Publishes the job's output as its completion: `bytes` long when it ran, empty when it
    /// failed. A job whose output isn't a buffer this driver holds, or is held back, has
    /// nothing to complete on.
    fn complete<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &ScaleJob,
        outcome: Result<u32, Failure>,
        now_ns: u64,
    ) -> Result<(), S::Error> {
        let output = job.output.buffer;
        let frame = empty(output, job.identity, self.sequence, now_ns);
        let mut outcome = outcome;
        let mut published = Ok(());
        if self.holds(output) && !self.held_back(output) && output != job.input.buffer {
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
                self.held = with(self.held, output, false);
                self.sequence = self.sequence.wrapping_add(1);
            }
        }
        match outcome {
            Ok(_) => self.counters.jobs = self.counters.jobs.saturating_add(1),
            Err(failure) => self.failed(failure),
        }
        published
    }

    /// The block finished a frame, so nothing earlier is still being written: every held-back
    /// output goes out as its job's failed completion, and every hold is let go.
    fn settle<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        now_ns: u64,
    ) -> Result<(), S::Error> {
        for buffer in 0..MAX_BUFFERS {
            if !self.held_back(buffer) {
                continue;
            }
            let identity = self.waiting.get(buffer as usize).copied().unwrap_or(0);
            stream.publish(empty(buffer, identity, self.sequence, now_ns))?;
            self.sequence = self.sequence.wrapping_add(1);
            self.held_back = with(self.held_back, buffer, false);
            self.held = with(self.held, buffer, false);
        }
        for hold in &mut self.holds {
            *hold = None;
        }
        Ok(())
    }

    /// Gives up a buffer that has nothing behind it, so it can be taken again with something.
    /// A held-back one is given up too: what is behind it now is no one's, and the hold on its
    /// memory stays.
    fn renew<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        buffer: u32,
    ) -> Result<bool, S::Error> {
        if !self.holds(buffer) || !matches!(stream.buffer_handle(buffer), Ok(None)) {
            return Ok(false);
        }
        // Nothing is behind it, so this reaches no one.
        stream.publish(empty(buffer, 0, 0, 0))?;
        self.held = with(self.held, buffer, false);
        self.held_back = with(self.held_back, buffer, false);
        self.stale = true;
        Ok(true)
    }

    /// Runs one job.
    fn resize<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &S,
        job: &ScaleJob,
        now_ns: u64,
    ) -> Ran {
        let format = match (
            format_of(&job.input, self.channel),
            format_of(&job.output, self.channel),
        ) {
            (Ok(input), Ok(output)) if input == output => input,
            _ => return Ran::Failed(Failure::Format),
        };
        if job.filter != Filter::Bilinear {
            return Ran::Failed(Failure::Format);
        }
        let (from, to) = (job.input.buffer, job.output.buffer);
        let usable = |b| self.holds(b) && !self.held_back(b);
        if from == to || !usable(from) || !usable(to) {
            return Ran::Failed(Failure::Buffers);
        }
        let (Ok(Some(from)), Ok(Some(to))) = (stream.buffer_handle(from), stream.buffer_handle(to))
        else {
            return Ran::Failed(Failure::NoHandle);
        };
        // The images are measured against the dma-bufs' real sizes here.
        let surface = |fd, image: &ImageRef| {
            Surface::new(fd, format, image.width, image.height, image.stride)
        };
        let (Ok(source), Ok(target)) = (
            surface(from.fd(), &job.input),
            surface(to.fd(), &job.output),
        ) else {
            return Ran::Failed(Failure::Surface);
        };
        let output = Output {
            channel: self.channel,
            crop: Crop::full(job.input.width, job.input.height),
            target,
            flip: false,
        };
        let Ok(mut request) = Job::new(source, &[output]) else {
            return Ran::Failed(Failure::Surface);
        };
        let bytes = format.frame_len(job.output.stride, job.output.height);
        // A frame past 4 GiB fits no stream's buffer: the completion is refused.
        let bytes = u32::try_from(bytes).unwrap_or(u32::MAX);
        if self.scaler.open(now_ns).is_err() {
            return Ran::Failed(Failure::Engine);
        }
        let Some(hold) = self.hold(to.fd()) else {
            return Ran::Failed(Failure::Held);
        };
        match self.scaler.run(now_ns, &mut request) {
            Ok(()) => Ran::Done(bytes),
            Err(RunError::Device { .. }) => Ran::HeldBack,
            Err(_) => {
                // The block was not started: the hold isn't needed.
                if let Some(slot) = self.holds.get_mut(hold) {
                    *slot = None;
                }
                Ran::Failed(Failure::Engine)
            }
        }
    }

    /// Takes a hold on `fd`'s memory in a free slot, and returns the slot.
    fn hold(&mut self, fd: BorrowedFd<'_>) -> Option<usize> {
        let slot = self.holds.iter().position(Option::is_none)?;
        let duplicate = fd.try_clone_to_owned().ok()?;
        *self.holds.get_mut(slot)? = Some(duplicate);
        Some(slot)
    }

    /// When an output is held back and the scaler is due, runs a frame of the driver's own,
    /// from memory it holds into memory it holds, to see the block finish one.
    fn probe<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        now_ns: u64,
    ) -> Result<(), S::Error> {
        let mut taken = self.holds.iter().flatten();
        let Some(target) = taken.next() else {
            return Ok(());
        };
        let (format, side) = PROBE;
        let stride = side * format.bytes_per_pixel();
        let fallback = (0..MAX_BUFFERS)
            .filter(|b| self.holds(*b) && !self.held_back(*b))
            .find_map(|b| match stream.buffer_handle(b) {
                Ok(Some(handle)) => Some(handle.fd()),
                _ => None,
            });
        let Some(source) = taken.next().map(AsFd::as_fd).or(fallback) else {
            return Ok(());
        };
        let surfaces = (
            Surface::new(source, format, side, side, stride),
            Surface::new(target.as_fd(), format, side, side, stride),
        );
        let (Ok(source), Ok(target)) = surfaces else {
            return Ok(());
        };
        let output = Output {
            channel: self.channel,
            crop: Crop::full(side, side),
            target,
            flip: false,
        };
        let Ok(mut request) = Job::new(source, &[output]) else {
            return Ok(());
        };
        if self.scaler.run(now_ns, &mut request).is_ok() {
            self.settle(stream, now_ns)?;
        }
        Ok(())
    }
}

/// How a job went.
enum Ran {
    /// The block finished it, writing this many bytes.
    Done(u32),
    /// It did not run, or did and failed with the block stopped.
    Failed(Failure),
    /// It failed while the block may still be writing its output.
    HeldBack,
}
