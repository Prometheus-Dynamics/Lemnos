//! The block through `lemnos.ko`: a claim on its node, programs, slots and runs.
//!
//! # What confines a job
//!
//! The module does (`kernel/lemnos_vip.c`, `kernel/lemnos_vip.h`): the block's registers,
//! interrupt and MMU never reach user space; a program is copied into the module and checked once
//! (state loads outside the block's control registers, NOP and STALL, nothing else); while a job
//! runs the block's MMU maps the module's copy of the program, read-only, and the job's slots,
//! each from a dma-buf registered on the stream, and nothing more; every slot entry is cleared
//! when the job ends. A program that reaches anywhere else faults, the module resets the block,
//! and the job's frame comes back with 0 bytes. So nothing here is believed: this side only
//! chooses what to send, and the module refuses what it must.
//!
//! # Slots and copies
//!
//! A slot is a dma-buf of the session's own, registered on the module's stream ([`Session::stage`]):
//! the stream's buffers are the consumer's, and a job's slot must be whole pages of a dma-buf the
//! module has pinned. Before a run the driver copies each attached buffer into its slot and
//! zeroes the rest of the pages the run maps; after the run it copies the slot back. A run maps
//! slots 0 to the highest attached, and every one in between must be attached.
//!
//! Slot `n` is at [`crate::confine::slot_address`]`(n)` on the block while the job runs, so a
//! caller's tooling can emit a program that names its slots without Lemnos reading the program.
//!
//! # Failures
//!
//! A claim that fails to open starts a backoff: until it is due, [`Service::claim`] answers
//! [`ClaimError::BackingOff`] without touching the device. The delay doubles from 250 ms, is
//! capped at 8 s, and clears when a claim is taken. The service never gives up. A run the block
//! faults on or doesn't finish completes as [`RunStatus::Failed`] and the claim stays: the module
//! has reset the block before the frame comes back. A session call that fails with `ENODEV`,
//! `EIO` (a wedged block, which the module refuses new streams on) or the stream's end marks the
//! claim [`Claim::lost`]; its holder drops it and claims again.
//!
//! # Allocation
//!
//! Taking a claim, loading and attaching allocate. A run allocates nothing.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lemnos_contract_npu::{
    Counters, JOB_BYTES, MAX_STREAM_NAME, Npu, NpuInfo, RESULT_BYTES, RunJob, RunResult, RunStatus,
};
use lemnos_core::{Decode, Encode, Reader, Status, Writer};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;

use crate::confine::SLOTS;

/// Programs loaded at once (`LEMNOS_VIP_PROGRAMS`).
pub const MAX_PROGRAMS: usize = 8;

/// Slots a program's runs may use (`LEMNOS_VIP_SLOTS`).
pub const MAX_SLOTS: u16 = SLOTS;

/// The largest program the module takes (`LEMNOS_VIP_MAX_PROGRAM_BYTES`): whole 8-byte command
/// words.
pub const MAX_PROGRAM_BYTES: u32 = 65535 * 8 - 64;

/// The page a slot is a whole number of.
pub const PAGE: u32 = 4096;

/// Buffers of a stream this driver can hold.
pub const MAX_BUFFERS: u32 = 64;

/// Jobs taken in one [`Claim::serve`]; the rest wait for the next.
pub const MAX_JOBS_PER_ROUND: u32 = 64;

/// The longest job read off the queue: a longer one is taken off and faults its consumer.
const LONGEST_JOB: usize = 256;

/// `EPERM`, `ENOENT`, `EIO`, `ENODEV`, `EINVAL`, `EBUSY` (`include/uapi/asm-generic/errno-base.h`).
const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const EIO: i32 = 5;
const EBUSY: i32 = 16;
const ENODEV: i32 = 19;
const EINVAL: i32 = 22;

/// First delay before a claim is tried again.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay after `failures` failures in a row (from 1), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(failures: u32) -> u64 {
    let attempt = if failures == 0 { 0 } else { failures - 1 };
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// `bytes` rounded up to whole pages; `None` past `u32`.
#[must_use]
pub const fn pages(bytes: u32) -> Option<u32> {
    match bytes.checked_add(PAGE - 1) {
        Some(b) => Some(b / PAGE * PAGE),
        None => None,
    }
}

/// One stream on the block's node in the module.
pub trait Session {
    /// Copies `program` into the module, which checks it there; the module's number for it.
    /// `EPERM` or `EINVAL` for a program it refuses, `EBUSY` while it holds as many as it takes.
    fn load(&mut self, program: &[u8]) -> io::Result<u32>;

    /// Lets go of a program the module loaded.
    fn unload(&mut self, program: u32) -> io::Result<()>;

    /// Makes slot `slot`'s memory at least `bytes` long (whole pages), registered on the stream.
    /// What it held may be lost when it grows.
    fn stage(&mut self, slot: u16, bytes: u32) -> io::Result<()>;

    /// Slot `slot`'s memory, as the CPU sees it; `None` before [`Self::stage`].
    fn memory(&mut self, slot: u16) -> Option<&mut [u8]>;

    /// Runs the module's `program` on slots `0..lens.len()`, slot `i`'s first `lens[i]` bytes
    /// (whole pages, inside what was staged), and waits for its end. `true`: the block finished;
    /// `false`: the module stopped it (a fault or the time limit) and reset the block. An error
    /// is a refusal, or the stream's end.
    fn run(&mut self, program: u32, lens: &[u32]) -> io::Result<bool>;
}

/// The node.
pub trait Module {
    /// A session on it.
    type Session: Session;

    /// Opens a new session: a stream on the block. `EBUSY` while another holds the block, `EIO`
    /// once the module has wedged it.
    fn open(&mut self) -> io::Result<Self::Session>;
}

/// Why a claim was not taken.
#[derive(Debug)]
pub enum ClaimError {
    /// Opening the node failed; the next claim may be tried from `retry_at_ns`.
    Device {
        /// What failed.
        error: io::Error,
        /// When, on the caller's clock.
        retry_at_ns: u64,
    },
    /// An earlier failure's backoff has not run out. The device was not touched.
    BackingOff {
        /// When the next claim may be tried.
        retry_at_ns: u64,
    },
    /// A claim taken on this service is still alive.
    Claimed,
}

/// The node, through `M`.
#[derive(Debug)]
pub struct Service<M: Module> {
    module: M,
    failures: u32,
    retry_at_ns: u64,
    /// Set while a claim taken here is alive; the claim clears it when dropped.
    claimed: Arc<AtomicBool>,
}

impl<M: Module> Service<M> {
    /// The service on `module`. Nothing is opened.
    pub fn new(module: M) -> Self {
        Self {
            module,
            failures: 0,
            retry_at_ns: 0,
            claimed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The node.
    pub fn module(&self) -> &M {
        &self.module
    }

    /// Takes the claim: a session on the block. `now_ns` is the caller's clock, for the backoff.
    pub fn claim(&mut self, now_ns: u64) -> Result<Claim<M>, ClaimError> {
        if self.claimed.load(Ordering::Acquire) {
            return Err(ClaimError::Claimed);
        }
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(ClaimError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        match self.module.open() {
            Ok(session) => {
                self.failures = 0;
                self.claimed.store(true, Ordering::Release);
                Ok(Claim::new(Arc::clone(&self.claimed), session))
            }
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
                Err(ClaimError::Device {
                    error,
                    retry_at_ns: self.retry_at_ns,
                })
            }
        }
    }
}

/// Why the last failed job failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The bytes on the job queue were not a job. The consumer's stream was faulted.
    Undecodable,
    /// The result buffer is not one the driver holds, so the job cannot complete. One that is
    /// not on the stream at all faulted the consumer's stream.
    Result,
    /// Refused before it ran: an unknown program, a gap in its slots, or the result buffer
    /// attached.
    Refused,
    /// A buffer could not be copied in or out.
    Copy,
    /// The module refused the job, or the block faulted or ran out of time.
    Device,
    /// The stream refused the completion.
    Completion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attached {
    buffer: u32,
    bytes: u32,
}

#[derive(Debug)]
struct Program {
    /// The module's number for it.
    module: u32,
    attached: [Option<Attached>; MAX_SLOTS as usize],
}

/// The claim on the node: a session, its programs, and the stream's buffers held.
pub struct Claim<M: Module> {
    session: M::Session,
    programs: [Option<Program>; MAX_PROGRAMS],
    /// Bytes staged per slot.
    staged: [u32; MAX_SLOTS as usize],
    /// The stream's buffers this driver holds, one bit each.
    held: u64,
    counters: Counters,
    last_failure: Option<Failure>,
    sequence: u64,
    /// Whether the session is beyond use.
    lost: bool,
    /// The service's flag, cleared when the claim is dropped.
    claimed: Arc<AtomicBool>,
}

impl<M: Module> std::fmt::Debug for Claim<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Claim")
            .field("programs", &self.programs)
            .field("staged", &self.staged)
            .field("held", &self.held)
            .field("counters", &self.counters)
            .field("last_failure", &self.last_failure)
            .field("lost", &self.lost)
            .finish_non_exhaustive()
    }
}

fn program_index(program: u32) -> Option<usize> {
    let index = usize::try_from(program).ok()?.checked_sub(1)?;
    (index < MAX_PROGRAMS).then_some(index)
}

impl<M: Module> Claim<M> {
    fn new(claimed: Arc<AtomicBool>, session: M::Session) -> Self {
        Self {
            session,
            programs: std::array::from_fn(|_| None),
            staged: [0; MAX_SLOTS as usize],
            held: 0,
            counters: Counters::default(),
            last_failure: None,
            sequence: 0,
            lost: false,
            claimed,
        }
    }

    /// Whether the session is beyond use: the node's device went, the block was wedged, or the
    /// stream ended. Drop the claim and take a new one.
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// Notes an error from the session.
    fn seen(&mut self, error: &io::Error) {
        self.lost |= matches!(error.raw_os_error(), Some(ENODEV | EIO))
            || error.kind() == io::ErrorKind::BrokenPipe;
    }

    /// The session.
    pub fn session(&self) -> &M::Session {
        &self.session
    }

    /// What the block takes.
    pub fn info(&self) -> NpuInfo {
        NpuInfo {
            max_program_bytes: MAX_PROGRAM_BYTES,
            max_programs: MAX_PROGRAMS as u16,
            max_attached: MAX_SLOTS,
        }
    }

    /// Runs completed and failed since the claim was taken.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Why the last failed job failed.
    pub fn last_failure(&self) -> Option<Failure> {
        self.last_failure
    }

    /// The stream's buffers held, one bit each, to hand to the next claim on the same stream
    /// with [`Self::adopt`] when this one is dropped.
    pub fn held(&self) -> u64 {
        self.held
    }

    /// Takes on the buffers an earlier claim on the same stream held.
    pub fn adopt(&mut self, held: u64) {
        self.held |= held;
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

    /// Gives up `buffer` if it was taken under a consumer that has gone, so the next
    /// [`Self::take_free`] takes it again under the consumer attached now.
    fn renew<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        buffer: u32,
    ) -> Result<(), S::Error> {
        if !self.holds(buffer) || !matches!(stream.buffer_current(buffer), Ok(false)) {
            return Ok(());
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
        Ok(())
    }

    /// Whether `bytes` from the start of `buffer` are inside it: its last byte reads.
    fn fits<S: StreamEndpoint + ?Sized>(&self, stream: &mut S, buffer: u32, bytes: u32) -> bool {
        let Some(last) = bytes.checked_sub(1) else {
            return false;
        };
        self.holds(buffer) && matches!(stream.read(buffer, last as usize, &mut [0]), Ok(true))
    }

    /// Loads the program in the first `bytes` of `buffer` on the stream and returns its number.
    /// The bytes are read once; the module checks that copy. `INVALID_ARGUMENT` for a program
    /// the module refuses, `BUSY` when no program slot is free.
    pub fn load<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        buffer: u32,
        bytes: u32,
    ) -> Result<u32, Status> {
        if bytes == 0 || !bytes.is_multiple_of(8) || bytes > MAX_PROGRAM_BYTES {
            return Err(Status::INVALID_ARGUMENT);
        }
        let index = self
            .programs
            .iter()
            .position(Option::is_none)
            .ok_or(Status::BUSY)?;
        self.renew(stream, buffer).map_err(|_| Status::IO)?;
        self.take_free(stream).map_err(|_| Status::IO)?;
        if !self.fits(stream, buffer, bytes) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut program = vec![0u8; bytes as usize];
        if !matches!(stream.read(buffer, 0, &mut program), Ok(true)) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let module = match self.session.load(&program) {
            Ok(module) => module,
            Err(error) => {
                self.seen(&error);
                return Err(match error.raw_os_error() {
                    Some(EPERM | EINVAL) => Status::INVALID_ARGUMENT,
                    Some(EBUSY) => Status::BUSY,
                    _ => Status::IO,
                });
            }
        };
        self.programs[index] = Some(Program {
            module,
            attached: [None; MAX_SLOTS as usize],
        });
        Ok(index as u32 + 1)
    }

    /// Unloads a program and detaches its buffers.
    pub fn unload(&mut self, program: u32) -> Result<(), Status> {
        let index = program_index(program).ok_or(Status::NOT_FOUND)?;
        let program = self.programs[index].take().ok_or(Status::NOT_FOUND)?;
        if let Err(error) = self.session.unload(program.module) {
            self.seen(&error);
            // ENOENT: the module has none by that number, which is what was wanted.
            if error.raw_os_error() != Some(ENOENT) && !self.lost {
                return Err(Status::IO);
            }
        }
        Ok(())
    }

    /// Attaches the first `bytes` of `buffer` on the stream as slot `slot` of `program`, and
    /// makes the slot's memory hold that many. A slot attached already is replaced.
    pub fn attach<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        program: u32,
        slot: u16,
        buffer: u32,
        bytes: u32,
    ) -> Result<(), Status> {
        let index = program_index(program).ok_or(Status::NOT_FOUND)?;
        if self.programs[index].is_none() {
            return Err(Status::NOT_FOUND);
        }
        let need = pages(bytes).ok_or(Status::INVALID_ARGUMENT)?;
        if slot >= MAX_SLOTS || bytes == 0 || u64::from(need) > crate::confine::SLOT_SPAN {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.renew(stream, buffer).map_err(|_| Status::IO)?;
        self.take_free(stream).map_err(|_| Status::IO)?;
        if !self.fits(stream, buffer, bytes) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let s = usize::from(slot);
        if self.staged[s] < need {
            if let Err(error) = self.session.stage(slot, need) {
                self.seen(&error);
                return Err(Status::IO);
            }
            self.staged[s] = need;
        }
        if let Some(p) = self.programs[index].as_mut() {
            p.attached[s] = Some(Attached { buffer, bytes });
        }
        Ok(())
    }

    /// Detaches one slot.
    pub fn detach(&mut self, program: u32, slot: u16) -> Result<(), Status> {
        let index = program_index(program).ok_or(Status::NOT_FOUND)?;
        let program = self.programs[index].as_mut().ok_or(Status::NOT_FOUND)?;
        program
            .attached
            .get_mut(usize::from(slot))
            .and_then(Option::take)
            .map(|_| ())
            .ok_or(Status::NOT_FOUND)
    }

    /// Takes the stream's free buffers and runs the jobs waiting on it, at `now_ns` on the
    /// caller's clock. Returns how many jobs it read. An error is the stream's, and leaves the
    /// jobs not yet read where they are. Allocates nothing.
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
                .and_then(|bytes| RunJob::decode(&mut Reader::new(bytes)).ok());
            // A broken consumer, by the contract: bytes that are no job, or a result buffer that
            // isn't on the stream. It loses the stream, and no job of its is run after.
            let Some(job) = job else {
                self.fault(stream, Failure::Undecodable)?;
                break;
            };
            if job.result >= stream.buffer_count() {
                self.fault(stream, Failure::Result)?;
                break;
            }
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look, or come since the buffer was taken.
            self.renew(stream, job.result)?;
            self.take_free(stream)?;
            if !self.holds(job.result) {
                self.failed(Failure::Result);
                continue;
            }
            let outcome = self.run(stream, &job);
            self.complete(stream, &job, outcome, now_ns)?;
        }
        Ok(read)
    }

    /// Faults the consumer whose job was no job, and gives up every buffer held under it.
    fn fault<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        failure: Failure,
    ) -> Result<(), S::Error> {
        self.failed(failure);
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

    /// Writes the run's record into its result buffer and publishes it.
    fn complete<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &RunJob,
        outcome: Result<(), (RunStatus, Failure)>,
        now_ns: u64,
    ) -> Result<(), S::Error> {
        let status = match outcome {
            Ok(()) => RunStatus::Done,
            Err((status, _)) => status,
        };
        let mut record = [0u8; RESULT_BYTES];
        let mut writer = Writer::new(&mut record);
        let encoded = RunResult { status }.encode(&mut writer).is_ok();
        let used = writer.len();
        let written = encoded && matches!(stream.write(job.result, 0, &record[..used]), Ok(true));
        let frame = StreamFrame {
            buffer: job.result,
            bytes: if written { used as u32 } else { 0 },
            sequence: self.sequence,
            identity: job.identity,
            timestamp_ns: now_ns,
        };
        let published = stream.publish(frame);
        if published.is_ok() {
            self.set_held(job.result, false);
            self.sequence = self.sequence.wrapping_add(1);
        }
        match outcome {
            Ok(()) if written => self.counters.jobs = self.counters.jobs.saturating_add(1),
            Ok(()) => self.failed(Failure::Completion),
            Err((_, failure)) => self.failed(failure),
        }
        published
    }

    /// Runs one job: checks, copies in, runs, copies out.
    fn run<S: StreamEndpoint + ?Sized>(
        &mut self,
        stream: &mut S,
        job: &RunJob,
    ) -> Result<(), (RunStatus, Failure)> {
        const REFUSED: (RunStatus, Failure) = (RunStatus::Refused, Failure::Refused);
        let index = program_index(job.program).ok_or(REFUSED)?;
        let program = self.programs[index].as_ref().ok_or(REFUSED)?;
        // Slots 0 to the highest attached, none missing in between.
        let used = program
            .attached
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |last| last + 1);
        let attached = program.attached;
        let module = program.module;
        let attached = &attached[..used];
        if attached.iter().any(Option::is_none) {
            return Err(REFUSED);
        }
        if attached.iter().flatten().any(|a| a.buffer == job.result) {
            return Err(REFUSED);
        }
        let mut lens = [0u32; MAX_SLOTS as usize];
        for (slot, a) in attached.iter().flatten().enumerate() {
            let len = pages(a.bytes).ok_or(REFUSED)?;
            if len > self.staged[slot] {
                return Err(REFUSED);
            }
            lens[slot] = len;
            let memory = self
                .session
                .memory(slot as u16)
                .and_then(|m| m.get_mut(..len as usize))
                .ok_or((RunStatus::Failed, Failure::Copy))?;
            let (data, rest) = memory.split_at_mut(a.bytes as usize);
            if !matches!(stream.read(a.buffer, 0, data), Ok(true)) {
                return Err((RunStatus::Refused, Failure::Copy));
            }
            // Whatever the last run left in the pages this run maps goes.
            rest.fill(0);
        }
        match self.session.run(module, &lens[..used]) {
            Ok(true) => {}
            Ok(false) => return Err((RunStatus::Failed, Failure::Device)),
            Err(error) => {
                self.seen(&error);
                return Err((RunStatus::Failed, Failure::Device));
            }
        }
        for (slot, a) in attached.iter().flatten().enumerate() {
            let data = self
                .session
                .memory(slot as u16)
                .and_then(|m| m.get(..a.bytes as usize))
                .ok_or((RunStatus::Failed, Failure::Copy))?;
            if !matches!(stream.write(a.buffer, 0, data), Ok(true)) {
                return Err((RunStatus::Failed, Failure::Copy));
            }
        }
        Ok(())
    }
}

impl<M: Module> Drop for Claim<M> {
    /// Lets go of every program before the session closes; closing the stream frees them anyway.
    fn drop(&mut self) {
        for index in 0..MAX_PROGRAMS {
            let _ = self.unload(index as u32 + 1);
        }
        self.claimed.store(false, Ordering::Release);
    }
}

/// `npu/1` served on a claim and a stream: the contract's calls, with the stream they need.
pub struct Served<'c, M: Module, S: StreamEndpoint + ?Sized> {
    /// The claim.
    pub claim: &'c mut Claim<M>,
    /// The job stream.
    pub stream: &'c mut S,
    /// The stream's name, as [`Npu::job_stream`] answers it.
    pub name: &'c str,
}

impl<M: Module, S: StreamEndpoint + ?Sized> Npu for Served<'_, M, S> {
    fn info(&mut self) -> lemnos_core::Result<NpuInfo> {
        Ok(self.claim.info())
    }

    fn job_stream(&mut self, into: &mut [u8]) -> lemnos_core::Result<usize> {
        let name = self.name.as_bytes();
        if name.len() > MAX_STREAM_NAME {
            return Err(Status::INVALID_ARGUMENT);
        }
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn load(&mut self, buffer: u32, bytes: u32) -> lemnos_core::Result<u32> {
        self.claim.load(self.stream, buffer, bytes)
    }

    fn unload(&mut self, program: u32) -> lemnos_core::Result<()> {
        self.claim.unload(program)
    }

    fn attach(
        &mut self,
        program: u32,
        slot: u16,
        buffer: u32,
        bytes: u32,
    ) -> lemnos_core::Result<()> {
        self.claim.attach(self.stream, program, slot, buffer, bytes)
    }

    fn detach(&mut self, program: u32, slot: u16) -> lemnos_core::Result<()> {
        self.claim.detach(program, slot)
    }

    fn counters(&mut self) -> lemnos_core::Result<Counters> {
        Ok(self.claim.counters())
    }
}
