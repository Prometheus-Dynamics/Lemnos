//! The job stream of `codec.block/1`, the same for every codec block: the buffers held for the
//! consumer, jobs taken off the ring and checked, the work read once into the driver's own
//! memory, the block asked to run it, and completions written and published. A block driver
//! implements [`Block`], which is only what its block does with a job that passed the checks
//! here, and keeps a [`Server`] beside it.
//!
//! This crate is no driver and knows no block.
//!
//! # Buffers
//!
//! The server holds every buffer of the stream the consumer has not been handed, so a job can
//! name any of them. A job's result buffer is published as its completion and is the server's
//! again once the consumer returns it. A job may name a buffer the consumer returned since the
//! server last looked: when a job names a buffer the server does not hold, the server takes the
//! returns back and looks again before it refuses the job. Buffers numbered [`MAX_BUFFERS`] and
//! up are never used.
//!
//! A stream's memory is made anew for each consumer. Every tick asks the stream whether one
//! held buffer is still the present consumer's ([`StreamEndpoint::buffer_current`]) before it
//! takes more. When it is not, everything held is given up (published while it reaches no
//! one), every job in flight is forgotten, and the block is told to let go of everything it
//! took from that consumer's buffers ([`Block::release`]).
//!
//! # What is checked here, before a block sees a job
//!
//! Everything in a job is the consumer's, and none of it is trusted.
//!
//! 1. The job must decode, to no more than the contract's size. Its work and result buffers
//!    must be ones the server holds, and its result buffer not one a job in flight uses.
//!    Otherwise the consumer is broken and its stream is faulted.
//! 2. `work_bytes` is at most the server's work memory and the block's own limit, and that
//!    many bytes are read from the work buffer through the stream, which refuses a range
//!    outside the buffer. They are read once: the block sees the copy.
//! 3. The copy must be work ([`Work::parse`]: every count and length inside the bytes), with
//!    no more references and items than the block takes.
//! 4. Every reference names a buffer the server holds, below [`MAX_BUFFERS`], that is not the
//!    job's result buffer.
//!
//! A job that fails 2 to 4 completes as refused, and nothing reaches the block. What a
//! reference's `slot`, `offset` and `length` and an item's tag and bytes may be is the
//! block's to check, against the memory really behind the buffer: a block gets at a buffer
//! only through [`Buffers`], which lends the handle of a buffer the job referenced and
//! nothing else.

#![forbid(unsafe_code)]

use lemnos_contract_codec_block::work::Work;
use lemnos_contract_codec_block::{
    BlockInfo, COMPLETION_BYTES, Completion, Counters, DATA_OFFSET, JOB_BYTES, Job, JobStatus,
};
use lemnos_core::{Decode, Encode, Health, Reader, Writer};
use lemnos_hal::blocks::{BufferHandle, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};

/// Buffers of the stream a server can hold. A job naming one past these is refused, or is a
/// broken consumer's when it is the work or the result.
pub const MAX_BUFFERS: u32 = 64;
/// The most bytes of work one job carries here. A block may take less.
pub const MAX_WORK_BYTES: usize = 16 * 1024;
/// The most bytes one job reads back here. A block may give less.
pub const MAX_READ_BACK: usize = 1024;
/// Jobs a block may have in flight at once.
pub const MAX_IN_FLIGHT: usize = 16;
/// Jobs taken from the stream in one tick; the rest wait for the next, which comes at once.
pub const MAX_JOBS_PER_TICK: u32 = 8;
/// How often the stream is looked at for jobs.
pub const POLL_NS: u64 = 1_000_000;

/// The longest job a stream's queue can carry. Jobs are read into this much, so that one too
/// long to be a job is taken off the queue and faults its consumer, not left at its head.
const LONGEST_JOB: usize = 4096;

/// What became of a job a block was asked to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// It ran to the end: this many bytes of read-back are in the memory the block was given.
    Done(usize),
    /// It is with the block, which will report it from [`Block::poll`] under this token.
    Pending(u32),
    /// The block refused it and nothing reached the hardware.
    Refused,
    /// The hardware was given it and did not finish it, or is not there.
    Failed,
}

/// A job that was pending and is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// The token [`Start::Pending`] gave.
    pub token: u32,
    pub status: JobStatus,
    /// Bytes of read-back the block put in the memory it was given.
    pub read_back: usize,
}

/// The stream's buffers as a block may reach them while it starts one job: by handle, and
/// only those the job referenced.
pub struct Buffers<'a> {
    stream: &'a dyn StreamEndpoint<Error = ErrorKind>,
    /// The buffers the job referenced, one bit each.
    named: u64,
}

impl<'a> Buffers<'a> {
    /// The buffers of `stream` whose bits are set in `named`, as a job that referenced them
    /// would reach them. The server makes one for each job; a block's own tests make one to
    /// drive the block without a server. The stream still lends a handle only for a buffer
    /// its producer holds.
    #[must_use]
    pub fn naming(stream: &'a dyn StreamEndpoint<Error = ErrorKind>, named: u64) -> Self {
        Self { stream, named }
    }

    /// The handle of `buffer`, when the job referenced it, the stream's buffers have handles
    /// and the consumer it was taken under is still there. The handle is good until the
    /// block's call returns: hardware that works past that needs its own hold (a duplicate,
    /// or the kernel's when it is given the descriptor in a request).
    #[must_use]
    pub fn handle(&self, buffer: u32) -> Option<BufferHandle<'a>> {
        if buffer >= MAX_BUFFERS || self.named & (1 << buffer) == 0 {
            return None;
        }
        self.stream.buffer_handle(buffer).ok().flatten()
    }
}

/// What one codec block does with a job that passed the server's checks.
pub trait Block {
    /// The block's name, at most the contract's `MAX_BLOCK_NAME` bytes.
    fn name(&self) -> &str;

    /// The block's limits. The server answers the smaller of each and its own.
    fn limits(&self) -> BlockInfo;

    /// Checks `work` against the block and the memory behind its buffers, and runs or queues
    /// it. Read-back goes into `read_back`. Nothing here may wait longer than the block's own
    /// bounded time for one job.
    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start;

    /// The next pending job that is over, its read-back in `read_back`. Never waits. A block
    /// whose jobs all end in [`start`](Self::start) has none.
    fn poll(&mut self, read_back: &mut [u8], now_ns: u64) -> Option<Finished> {
        let _ = (read_back, now_ns);
        None
    }

    /// The consumer whose jobs these were has gone: lets go of everything taken from its
    /// buffers, and forgets its pending jobs.
    fn release(&mut self);

    /// How the block stands.
    fn health(&self) -> Health {
        Health::Ok
    }
}

/// A job with the block.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    token: u32,
    identity: u64,
    result: u32,
    /// Its result and referenced buffers, one bit each.
    buffers: u64,
}

/// One job stream's state.
pub struct Server {
    /// The buffers held, one bit each.
    held: u64,
    /// Whether a consumer was attached when last looked.
    attached: bool,
    sequence: u64,
    work: Box<[u8]>,
    read_back: Box<[u8]>,
    in_flight: [Option<InFlight>; MAX_IN_FLIGHT],
    counters: Counters,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    /// A server holding nothing. Its memory for work and read-back is taken here, once.
    #[must_use]
    pub fn new() -> Self {
        Self {
            held: 0,
            attached: false,
            sequence: 0,
            work: vec![0; MAX_WORK_BYTES].into_boxed_slice(),
            read_back: vec![0; MAX_READ_BACK].into_boxed_slice(),
            in_flight: [None; MAX_IN_FLIGHT],
            counters: Counters::default(),
        }
    }

    /// Counted since the server was made.
    #[must_use]
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// What the contract's `info` answers for `block`: the smaller of its limits and the
    /// server's.
    #[must_use]
    pub fn info<B: Block + ?Sized>(block: &B) -> BlockInfo {
        let limits = block.limits();
        BlockInfo {
            max_work_bytes: limits.max_work_bytes.min(MAX_WORK_BYTES as u32),
            max_refs: limits.max_refs,
            max_items: limits.max_items,
            max_read_back: limits.max_read_back.min(MAX_READ_BACK as u32),
            max_buffers: limits.max_buffers.min(MAX_BUFFERS),
        }
    }

    fn holds(&self, buffer: u32) -> bool {
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
    /// Allocates nothing.
    fn take_returned<S>(&mut self, stream: &mut S) -> Result<(), ErrorKind>
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
    {
        for _ in 0..stream.buffer_count() {
            let Some(buffer) = stream.next_buffer()? else {
                break;
            };
            self.set_held(buffer, true);
        }
        Ok(())
    }

    /// The buffers jobs in flight use.
    fn busy(&self) -> u64 {
        self.in_flight
            .iter()
            .flatten()
            .fold(0, |all, job| all | job.buffers)
    }

    /// Gives up everything held, published while it reaches no one, forgets the jobs in
    /// flight and has the block let go: they were a consumer's that has gone.
    fn forget<S, B>(&mut self, stream: &mut S, block: &mut B) -> Result<(), ErrorKind>
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
        B: Block + ?Sized,
    {
        block.release();
        self.in_flight = [None; MAX_IN_FLIGHT];
        self.attached = false;
        while self.held != 0 {
            let buffer = self.held.trailing_zeros();
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

    /// Holds the stream's free buffers when a consumer is attached, and nothing when none is.
    /// Whether one is.
    fn refresh<S, B>(&mut self, stream: &mut S, block: &mut B) -> Result<bool, ErrorKind>
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
        B: Block + ?Sized,
    {
        // Asked before any buffer is taken, so everything held is of one consumer's time.
        if self.held != 0 && !stream.buffer_current(self.held.trailing_zeros())? {
            self.forget(stream, block)?;
        }
        if !self.attached {
            let Some(buffer) = stream.next_buffer()? else {
                return Ok(false);
            };
            self.set_held(buffer, true);
            if !stream.buffer_current(buffer)? {
                self.forget(stream, block)?;
                return Ok(false);
            }
            self.attached = true;
        }
        self.take_returned(stream)?;
        Ok(true)
    }

    /// Publishes `result` as the completion of the job `identity`, with its record and
    /// `read_back` bytes of the server's read-back memory after it.
    fn complete<S>(
        &mut self,
        stream: &mut S,
        identity: u64,
        result: u32,
        status: JobStatus,
        read_back: usize,
        now_ns: u64,
    ) -> Result<(), ErrorKind>
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
    {
        let mut status = status;
        let mut count = if status == JobStatus::Done {
            read_back.min(self.read_back.len())
        } else {
            0
        };
        // Read-back that doesn't fit the result buffer fails the job: the consumer gave it
        // too little room.
        if count > 0
            && !matches!(
                stream.write(result, DATA_OFFSET, &self.read_back[..count]),
                Ok(true)
            )
        {
            status = JobStatus::Failed;
            count = 0;
        }
        let record = Completion {
            status,
            count: count as u32,
        };
        let mut bytes = [0u8; COMPLETION_BYTES];
        let mut writer = Writer::new(&mut bytes);
        let encoded = record.encode(&mut writer).is_ok();
        // A buffer too small for the record completes empty.
        let recorded = encoded && matches!(stream.write(result, 0, &bytes), Ok(true));
        let length = if recorded { DATA_OFFSET + count } else { 0 };
        stream.publish(StreamFrame {
            buffer: result,
            bytes: length as u32,
            sequence: self.sequence,
            identity,
            timestamp_ns: now_ns,
        })?;
        self.set_held(result, false);
        self.sequence = self.sequence.wrapping_add(1);
        match status {
            JobStatus::Done => self.counters.jobs += 1,
            JobStatus::Refused => self.counters.refused += 1,
            JobStatus::Failed => self.counters.failed += 1,
        }
        Ok(())
    }

    /// Reads `job`'s work and checks it and its references. The references' buffers, one bit
    /// each, and the work's length; `Ok(None)` when the job is refused.
    fn check<S, B>(
        &mut self,
        stream: &mut S,
        block: &B,
        job: &Job,
    ) -> Result<Option<(u64, usize)>, ErrorKind>
    where
        S: StreamEndpoint<Error = ErrorKind> + ?Sized,
        B: Block + ?Sized,
    {
        let limits = block.limits();
        let Some(len) = usize::try_from(job.work_bytes).ok() else {
            return Ok(None);
        };
        if len > limits.max_work_bytes as usize {
            return Ok(None);
        }
        let Some(copy) = self.work.get_mut(..len) else {
            return Ok(None);
        };
        if !matches!(stream.read(job.work, 0, copy), Ok(true)) {
            return Ok(None);
        }
        let Some(work) = Work::parse(copy) else {
            return Ok(None);
        };
        if work.ref_count() > usize::from(limits.max_refs)
            || work.item_count() > usize::from(limits.max_items)
        {
            return Ok(None);
        }
        let usable = limits.max_buffers.min(MAX_BUFFERS);
        let mut named = 0u64;
        for reference in work.refs() {
            let buffer = reference.buffer;
            if buffer >= usable || buffer == job.result {
                return Ok(None);
            }
            named |= 1 << buffer;
        }
        // A buffer a reference names that is not held may have been returned since the last
        // look, as for the job's own buffers: take the returns back, then look again.
        if named & !self.held != 0 {
            self.take_returned(stream)?;
        }
        if named & !self.held != 0 {
            return Ok(None);
        }
        Ok(Some((named, len)))
    }

    /// Serves the stream once: completes what the block has finished, then takes new jobs
    /// while the block has room. Whether to look again at once (a round's worth of jobs was
    /// taken). An error is the stream's, and leaves the jobs not yet read where they are.
    /// Allocates nothing.
    pub fn serve<B>(
        &mut self,
        stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
        block: &mut B,
        now_ns: u64,
    ) -> Result<bool, ErrorKind>
    where
        B: Block + ?Sized,
    {
        if !self.refresh(stream, block)? {
            return Ok(false);
        }
        while let Some(finished) = block.poll(&mut self.read_back, now_ns) {
            let at = self
                .in_flight
                .iter()
                .position(|job| job.is_some_and(|job| job.token == finished.token));
            // A token the server never had is the block's mistake, and completes nothing.
            if let Some(job) = at.and_then(|at| self.in_flight[at].take()) {
                self.complete(
                    stream,
                    job.identity,
                    job.result,
                    finished.status,
                    finished.read_back,
                    now_ns,
                )?;
            }
        }
        let mut encoded = [0u8; LONGEST_JOB];
        let mut taken = 0;
        while taken < MAX_JOBS_PER_TICK {
            let Some(free) = self.in_flight.iter().position(Option::is_none) else {
                break;
            };
            let Some(len) = stream.next_job(&mut encoded)? else {
                break;
            };
            taken += 1;
            let busy = self.busy();
            let decoded = encoded
                .get(..len)
                .filter(|bytes| bytes.len() <= JOB_BYTES)
                .and_then(|bytes| Job::decode(&mut Reader::new(bytes)).ok());
            // Again for each job: its consumer may have returned a buffer and named it in this job
            // since the last look. A consumer releases a buffer before it posts a job that names
            // it, so once the job is read the release is in place, and the returns are taken back
            // before the job is checked. Only a job that names a buffer not held asks.
            let unheld =
                decoded.is_some_and(|job| !(self.holds(job.work) && self.holds(job.result)));
            if unheld {
                self.take_returned(stream)?;
            }
            let job = decoded.filter(|job| {
                self.holds(job.work) && self.holds(job.result) && busy & (1 << job.result) == 0
            });
            let Some(job) = job else {
                // Bytes that are no job, or a job with nothing to complete on: a broken
                // consumer. It loses the stream, and what was held under it goes with it.
                self.counters.refused += 1;
                stream.fault_consumer()?;
                self.forget(stream, block)?;
                return Ok(false);
            };
            let Some((named, len)) = self.check(stream, block, &job)? else {
                self.complete(
                    stream,
                    job.identity,
                    job.result,
                    JobStatus::Refused,
                    0,
                    now_ns,
                )?;
                continue;
            };
            // Checked to parse in `check`, from this same copy.
            let Some(work) = Work::parse(&self.work[..len]) else {
                continue;
            };
            let buffers = Buffers::naming(&*stream, named);
            let started = block.start(&work, &buffers, &mut self.read_back, now_ns);
            let (status, read_back) = match started {
                Start::Pending(token) => {
                    self.in_flight[free] = Some(InFlight {
                        token,
                        identity: job.identity,
                        result: job.result,
                        buffers: named | (1 << job.result),
                    });
                    continue;
                }
                Start::Done(read_back) => (JobStatus::Done, read_back),
                Start::Refused => (JobStatus::Refused, 0),
                Start::Failed => (JobStatus::Failed, 0),
            };
            self.complete(stream, job.identity, job.result, status, read_back, now_ns)?;
        }
        Ok(taken >= MAX_JOBS_PER_TICK)
    }

    /// When a driver whose stream this is should tick next, given what
    /// [`serve`](Self::serve) answered.
    #[must_use]
    pub fn next_tick_ns(more: bool, now_ns: u64) -> u64 {
        if more {
            now_ns
        } else {
            now_ns.saturating_add(POLL_NS)
        }
    }
}

/// Copies `name` into `into`, as the contract's name methods answer.
pub fn name_into(name: &str, into: &mut [u8]) -> lemnos_core::Result<usize> {
    into.get_mut(..name.len())
        .ok_or(lemnos_core::Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(name.as_bytes());
    Ok(name.len())
}

#[cfg(all(feature = "testing", target_os = "linux"))]
pub mod testing;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
