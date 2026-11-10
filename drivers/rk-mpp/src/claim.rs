//! The service, a claim on one of its blocks, the claim's buffers, and jobs submitted and
//! completed.
//!
//! # Exclusive
//!
//! A [`Claim`] borrows its [`Service`] mutably, so a service has one claim at a time. That is
//! what confines a job: the block reaches only what its IOMMU maps, and with one session open the
//! only buffers mapped are the claim's. Dropping a claim waits for its jobs, asks the kernel to
//! unmap every buffer the session mapped (`MPP_CMD_RESET_SESSION`), and closes the session, before
//! the next claim can be taken. One process should hold one `Service` for the node, and no other
//! process should open the node.
//!
//! # Failures
//!
//! A claim that fails to open or bind its session starts a backoff: until it is due,
//! [`Service::claim`] answers [`ClaimError::BackingOff`] without touching the device. The delay
//! doubles from 250 ms and is capped at 8 s, and clears when a claim is taken. The service never
//! gives up. A job the kernel refuses comes back as [`SubmitError::Device`]; the claim stays.
//!
//! # Allocation
//!
//! Taking a claim allocates. Adding a buffer, submitting and completing allocate nothing.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

use crate::block::{Block, Facts};
use crate::job::{BufferId, Held, Job, JobError, Plan, Staging, plan};
use crate::message::{Message, READ_BACK_SLOTS};
use crate::uapi::{Command, FD_MASK};

/// Buffers one claim may hold at once.
pub const MAX_BUFFERS: usize = 32;

/// First delay before a claim is tried again.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay after `failures` failures in a row (from 1), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(failures: u32) -> u64 {
    let attempt = if failures == 0 { 0 } else { failures - 1 };
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// One open of `/dev/mpp_service`: a kernel session.
///
/// The implementation holds the `unsafe`: it points each [`Message`] at its payload and issues
/// `MPP_IOC_CFG_V1`. The messages are made only by this crate (see [`crate::message`]).
pub trait Session {
    /// Sends `messages` as one `MPP_IOC_CFG_V1` call, retrying on `EINTR`. An
    /// [`crate::Payload::In`] or [`crate::Payload::InOut`] is used only during the call. A
    /// [`crate::Payload::ReadBack`] points at the session's own area `slot` of
    /// [`crate::READ_BACK_BYTES`], which must stay in place while the session is open: the kernel
    /// writes it during a later call that polls the task. The session zeroes the area when it
    /// sends a message naming it, so a task the kernel drops after collecting it (its `alloc_task`
    /// failing, which the call does not report) completes with zeros rather than an older task's
    /// registers.
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()>;

    /// Copies the start of read-back area `slot` into `into`.
    fn read_back(&mut self, slot: usize, into: &mut [u8]);
}

/// The node and the facts the claim needs from the system.
pub trait Device {
    /// A session on the node.
    type Session: Session;

    /// Opens a new session.
    fn open(&mut self) -> io::Result<Self::Session>;

    /// Whether `block` sits behind an IOMMU. Without one, a device address is a physical address
    /// and nothing confines what the caller's registers make the block read or write.
    fn confined(&mut self, block: Block) -> io::Result<bool>;

    /// The size of dma-buf `fd`, in bytes.
    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64>;
}

/// Why a claim was not taken.
#[derive(Debug)]
pub enum ClaimError {
    /// The service has no such block.
    Absent,
    /// The block is not behind an IOMMU, so a job could not be confined to the claim's buffers.
    NotConfined,
    /// Opening or binding the session failed; the next claim may be tried from `retry_at_ns`.
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
}

/// Why a buffer was not added or removed.
#[derive(Debug)]
pub enum BufferError {
    /// The claim holds [`MAX_BUFFERS`] buffers already.
    Full,
    /// The descriptor's number is 0 or above 1023, which a register's 10 descriptor bits cannot
    /// carry.
    Descriptor,
    /// The buffer is empty, or its size could not be read.
    Size(Option<io::Error>),
    /// No such buffer.
    Unknown,
    /// A job in flight uses the buffer.
    Busy,
}

/// Why a job was not submitted.
#[derive(Debug)]
pub enum SubmitError {
    /// The job was refused before reaching the device.
    Job(JobError),
    /// [`crate::READ_BACK_SLOTS`] jobs are in flight; complete one first.
    Full,
    /// The kernel refused the job. Nothing is in flight for it.
    Device(io::Error),
}

/// Why no completion was taken.
#[derive(Debug)]
pub enum CompleteError {
    /// No job is in flight.
    Nothing,
    /// `into` holds fewer registers than the oldest job reads back.
    TooSmall {
        /// Registers it reads back.
        need: usize,
    },
    /// The poll failed; the job stays in flight.
    Device(io::Error),
}

/// A job that finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The job's identity.
    pub identity: u64,
    /// Registers read back into the caller's slice.
    pub registers: usize,
}

/// `/dev/mpp_service`, through `D`.
pub struct Service<D: Device> {
    device: D,
    failures: u32,
    retry_at_ns: u64,
}

impl<D: Device> Service<D> {
    /// The service on `device`. Nothing is opened.
    pub fn new(device: D) -> Self {
        Self {
            device,
            failures: 0,
            retry_at_ns: 0,
        }
    }

    /// The device.
    pub fn device(&self) -> &D {
        &self.device
    }

    /// Takes a claim on `block`: a new session, checked to have the block behind an IOMMU, bound
    /// to it. `now_ns` is the caller's clock, for the backoff.
    pub fn claim(&mut self, block: Block, now_ns: u64) -> Result<Claim<'_, D>, ClaimError> {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(ClaimError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        match self.bind(block) {
            Ok(Ok(session)) => {
                self.failures = 0;
                Ok(Claim::new(self, session, block))
            }
            Ok(Err(refused)) => Err(refused),
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

    fn bind(&mut self, block: Block) -> io::Result<Result<D::Session, ClaimError>> {
        let facts = block.facts();
        let mut session = self.device.open()?;
        let mut present = [0u8; 4];
        session.send(&mut [Message::in_out(Command::QueryHwSupport, &mut present)])?;
        if u32::from_ne_bytes(present) & (1 << facts.client) == 0 {
            return Ok(Err(ClaimError::Absent));
        }
        if !self.device.confined(block)? {
            return Ok(Err(ClaimError::NotConfined));
        }
        let client = facts.client.to_ne_bytes();
        session.send(&mut [Message::input(Command::InitClientType, 0, &client)])?;
        Ok(Ok(session))
    }
}

struct Buffer {
    fd: OwnedFd,
    size: u64,
    generation: u16,
    /// Jobs in flight that use it.
    in_flight: u16,
    /// Whether the kernel may have mapped it (a job used it).
    mapped: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Pending {
    identity: u64,
    read_bytes: usize,
    buffers: u64,
}

/// A claim on one block: its session, the buffers it holds, and its jobs in flight.
pub struct Claim<'s, D: Device> {
    service: &'s mut Service<D>,
    session: D::Session,
    block: Block,
    facts: Facts,
    buffers: [Option<Buffer>; MAX_BUFFERS],
    generation: u16,
    staging: Box<Staging>,
    pending: [Pending; READ_BACK_SLOTS],
    /// Slot of the oldest job in flight.
    head: usize,
    in_flight: usize,
}

impl<'s, D: Device> Claim<'s, D> {
    fn new(service: &'s mut Service<D>, session: D::Session, block: Block) -> Self {
        Self {
            service,
            session,
            block,
            facts: block.facts(),
            buffers: std::array::from_fn(|_| None),
            generation: 0,
            staging: Staging::new(),
            pending: [Pending::default(); READ_BACK_SLOTS],
            head: 0,
            in_flight: 0,
        }
    }

    /// The block.
    pub fn block(&self) -> Block {
        self.block
    }

    /// The block's facts.
    pub fn facts(&self) -> Facts {
        self.facts
    }

    /// Jobs submitted and not yet completed.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Takes a dma-buf for jobs to name. The claim keeps the descriptor until the buffer is
    /// removed, so its number cannot be reused for another file meanwhile; its size is measured
    /// once here.
    pub fn add_buffer(&mut self, fd: OwnedFd) -> Result<BufferId, BufferError> {
        let raw = fd.as_raw_fd();
        if raw <= 0 || raw > FD_MASK as i32 {
            return Err(BufferError::Descriptor);
        }
        let slot = self
            .buffers
            .iter()
            .position(Option::is_none)
            .ok_or(BufferError::Full)?;
        let size = self
            .service
            .device
            .buffer_size(fd.as_fd())
            .map_err(|e| BufferError::Size(Some(e)))?;
        if size == 0 {
            return Err(BufferError::Size(None));
        }
        self.generation = self.generation.wrapping_add(1);
        self.buffers[slot] = Some(Buffer {
            fd,
            size,
            generation: self.generation,
            in_flight: 0,
            mapped: false,
        });
        Ok(BufferId {
            slot: slot as u8,
            generation: self.generation,
        })
    }

    /// The size of a buffer the claim holds.
    pub fn buffer_size(&self, id: BufferId) -> Option<u64> {
        self.buffer(id).map(|b| b.size)
    }

    fn buffer(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers
            .get(usize::from(id.slot))?
            .as_ref()
            .filter(|b| b.generation == id.generation)
    }

    /// Gives a buffer back, once no job in flight uses it. When a job used it, the kernel is
    /// asked to drop its mapping first (`MPP_CMD_RELEASE_FD`); the kernel answers `EINVAL` when it
    /// had let the mapping go already, and the answer is not otherwise acted on.
    pub fn remove_buffer(&mut self, id: BufferId) -> Result<OwnedFd, BufferError> {
        let buffer = self.buffer(id).ok_or(BufferError::Unknown)?;
        if buffer.in_flight > 0 {
            return Err(BufferError::Busy);
        }
        if buffer.mapped {
            let fd = (buffer.fd.as_raw_fd() as u32).to_ne_bytes();
            let _ = self
                .session
                .send(&mut [Message::input(Command::ReleaseFd, 0, &fd)]);
        }
        let buffer = self.buffers[usize::from(id.slot)]
            .take()
            .ok_or(BufferError::Unknown)?;
        Ok(buffer.fd)
    }

    /// Checks `job` and queues it on the block. It runs after the jobs before it; take its
    /// completion with [`Self::complete`].
    pub fn submit(&mut self, job: &Job<'_>) -> Result<(), SubmitError> {
        if self.in_flight == READ_BACK_SLOTS {
            return Err(SubmitError::Full);
        }
        let buffers = &self.buffers;
        let held = |id: BufferId| {
            let b = buffers
                .get(usize::from(id.slot))?
                .as_ref()
                .filter(|b| b.generation == id.generation)?;
            Some(Held {
                slot: id.slot,
                fd: b.fd.as_raw_fd() as u32,
                size: b.size,
            })
        };
        let plan = plan(&self.facts, job, held, &mut self.staging).map_err(SubmitError::Job)?;
        let slot = (self.head + self.in_flight) % READ_BACK_SLOTS;
        send_job(&mut self.session, &self.staging, &plan, slot).map_err(SubmitError::Device)?;

        self.pending[slot] = Pending {
            identity: job.identity,
            read_bytes: plan.read.map_or(0, |(_, bytes)| bytes),
            buffers: plan.buffers,
        };
        self.in_flight += 1;
        for (i, buffer) in self.buffers.iter_mut().enumerate() {
            if let Some(buffer) = buffer.as_mut().filter(|_| plan.buffers & (1 << i) != 0) {
                buffer.in_flight += 1;
                buffer.mapped = true;
            }
        }
        Ok(())
    }

    /// Waits for the oldest job in flight and copies the registers it reads back into `into`.
    pub fn complete(&mut self, into: &mut [u32]) -> Result<Completion, CompleteError> {
        if self.in_flight == 0 {
            return Err(CompleteError::Nothing);
        }
        let slot = self.head;
        let pending = self.pending[slot];
        let registers = pending.read_bytes / 4;
        if into.len() < registers {
            return Err(CompleteError::TooSmall { need: registers });
        }
        self.session
            .send(&mut [Message::bare(Command::PollHwFinish)])
            .map_err(CompleteError::Device)?;

        let bytes = &mut self.staging.read_back[..pending.read_bytes];
        self.session.read_back(slot, bytes);
        let (words, _) = bytes.as_chunks::<4>();
        for (out, word) in into.iter_mut().zip(words) {
            *out = u32::from_ne_bytes(*word);
        }
        self.head = (self.head + 1) % READ_BACK_SLOTS;
        self.in_flight -= 1;
        for (i, buffer) in self.buffers.iter_mut().enumerate() {
            if let Some(buffer) = buffer.as_mut().filter(|_| pending.buffers & (1 << i) != 0) {
                buffer.in_flight -= 1;
            }
        }
        Ok(Completion {
            identity: pending.identity,
            registers,
        })
    }
}

/// One job as one call: the session's translation table, the write, the offsets, the read-back.
fn send_job<S: Session>(
    session: &mut S,
    staging: &Staging,
    plan: &Plan,
    slot: usize,
) -> io::Result<()> {
    let (write_at, from, len) = plan.write;
    let mut messages = [
        Message::input(Command::InitTransTable, 0, &staging.table[..plan.table]),
        Message::input(
            Command::SetRegWrite,
            write_at,
            &staging.registers[from..from + len],
        ),
        Message::bare(Command::PollHwFinish),
        Message::bare(Command::PollHwFinish),
    ];
    let mut n = 2;
    if plan.offsets > 0 {
        messages[n] = Message::input(
            Command::SetRegAddrOffset,
            0,
            &staging.offsets[..plan.offsets],
        );
        n += 1;
    }
    if let Some((at, bytes)) = plan.read {
        messages[n] = Message::read_back(at, bytes, slot);
        n += 1;
    }
    let messages = &mut messages[..n];
    Message::chain(messages);
    session.send(messages)
}

impl<D: Device> Drop for Claim<'_, D> {
    fn drop(&mut self) {
        // Wait for every job, so no task still holds a mapping, then unmap the session's buffers.
        while self.in_flight > 0 {
            if self
                .session
                .send(&mut [Message::bare(Command::PollHwFinish)])
                .is_err()
            {
                break;
            }
            self.head = (self.head + 1) % READ_BACK_SLOTS;
            self.in_flight -= 1;
        }
        let _ = self
            .session
            .send(&mut [Message::bare(Command::ResetSession)]);
    }
}
