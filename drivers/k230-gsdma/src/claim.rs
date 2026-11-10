//! The engine, a claim on it, the claim's buffers, and a job submitted and completed.
//!
//! # One job at a time
//!
//! The engine walks one descriptor chain at a time (the kernel driver serialises callers with a
//! lock and waits for each chain), so a claim has at most one job in flight. A [`Claim`] borrows
//! its [`Gsdma`] mutably, so a `Gsdma` gives one claim at a time.
//!
//! # Failures
//!
//! A claim whose session fails to open starts a backoff: until it is due, [`Gsdma::claim`]
//! answers [`ClaimError::BackingOff`] without touching the device. The delay doubles from 250 ms,
//! is capped at 8 s, and clears when a claim is taken. The driver never gives up and never turns
//! the engine off. A job the session refuses comes back as [`SubmitError::Device`] and nothing is
//! in flight. A job that has not finished by the timeout stays in flight
//! ([`CompleteError::Timeout`]); dropping the claim drops its session first, which must stop the
//! engine (the kernel driver disables and re-enables the channel), and only then the buffers.
//!
//! # Allocation
//!
//! Taking a claim may allocate (the session). Adding and removing buffers, submitting and
//! completing allocate nothing.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use crate::job::{BufferId, Job, JobError, Plan, plan};

/// Buffers one claim may hold at once.
pub const MAX_BUFFERS: usize = 16;

/// How long [`Claim::complete`] waits by default: the kernel driver's
/// [`crate::uapi::DEFAULT_TIMEOUT_MS`].
pub const TIMEOUT: Duration = Duration::from_millis(crate::uapi::DEFAULT_TIMEOUT_MS as u64);

/// First delay before a claim is tried again.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay after `failures` failures in a row (from 1), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(failures: u32) -> u64 {
    let attempt = if failures == 0 { 0 } else { failures - 1 };
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// What a session is handed for one job: the checked [`Plan`] and the descriptors of the buffers
/// it names.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    plan: &'a Plan,
    buffers: &'a [Option<Buffer>; MAX_BUFFERS],
}

impl<'a> Request<'a> {
    /// The checked job: every plane's buffer, offset, extent and stride, the size of each plane,
    /// the format and the rotation.
    #[must_use]
    pub fn plan(&self) -> &'a Plan {
        self.plan
    }

    /// The dma-buf of a buffer the plan names.
    #[must_use]
    pub fn buffer(&self, id: BufferId) -> Option<BorrowedFd<'a>> {
        held(self.buffers, id).map(|b| b.fd.as_fd())
    }
}

/// One open of the engine. What a session must do is what the kernel interface it stands for
/// must do; see the crate's docs for why none exists yet.
pub trait Session {
    /// Starts `request`. Only buffers the request names, at the plan's extents, may be reached,
    /// so the session must place each buffer as one piece the engine can address (a contiguous
    /// buffer, or one the session copies through), and must build the descriptors with
    /// [`Plan::descriptors`], which checks every extent against the memory the engine may reach.
    fn submit(&mut self, request: &Request<'_>) -> io::Result<()>;

    /// Waits up to `timeout` for the job in flight: `true` when it finished.
    fn wait(&mut self, timeout: Duration) -> io::Result<bool>;
}

/// The engine and the facts the claim needs from the system.
pub trait Device {
    /// One open of the engine.
    type Session: Session;

    /// Opens a session.
    fn open(&mut self) -> io::Result<Self::Session>;

    /// The size of dma-buf `fd`, in bytes.
    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64>;
}

/// Why a claim was not taken.
#[derive(Debug)]
pub enum ClaimError {
    /// Opening the session failed; the next claim may be tried from `retry_at_ns`.
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
    /// The buffer is empty, or its size could not be read.
    Size(Option<io::Error>),
    /// No such buffer.
    Unknown,
    /// The job in flight uses the buffer.
    Busy,
}

/// Why a job was not submitted.
#[derive(Debug)]
pub enum SubmitError {
    /// Refused before anything was built.
    Job(JobError),
    /// A job is in flight; complete it first.
    Busy,
    /// The session refused it. Nothing is in flight.
    Device(io::Error),
}

/// Why no completion was taken.
#[derive(Debug)]
pub enum CompleteError {
    /// No job is in flight.
    Nothing,
    /// The job did not finish in time; it stays in flight.
    Timeout,
    /// The wait failed; the job stays in flight.
    Device(io::Error),
}

/// A job that finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The job's identity.
    pub identity: u64,
}

/// The engine, through `D`.
pub struct Gsdma<D: Device> {
    device: D,
    failures: u32,
    retry_at_ns: u64,
}

impl<D: Device> Gsdma<D> {
    /// The engine on `device`. Nothing is opened.
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

    /// Takes a claim: a new session. `now_ns` is the caller's clock, for the backoff.
    pub fn claim(&mut self, now_ns: u64) -> Result<Claim<'_, D>, ClaimError> {
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(ClaimError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        match self.device.open() {
            Ok(session) => {
                self.failures = 0;
                Ok(Claim {
                    session,
                    buffers: std::array::from_fn(|_| None),
                    gsdma: self,
                    generation: 0,
                    in_flight: None,
                })
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

#[derive(Debug)]
struct Buffer {
    fd: OwnedFd,
    size: u64,
    generation: u16,
}

fn held(buffers: &[Option<Buffer>; MAX_BUFFERS], id: BufferId) -> Option<&Buffer> {
    buffers
        .get(usize::from(id.slot))?
        .as_ref()
        .filter(|b| b.generation == id.generation)
}

/// A claim on the engine: a session, the buffers it holds, and the job in flight.
pub struct Claim<'g, D: Device> {
    // Declared first, so it is dropped (and the engine stopped) before the buffers close.
    session: D::Session,
    buffers: [Option<Buffer>; MAX_BUFFERS],
    gsdma: &'g mut Gsdma<D>,
    generation: u16,
    /// The job in flight: its identity and the buffers it uses.
    in_flight: Option<(u64, u64)>,
}

impl<D: Device> Claim<'_, D> {
    /// Takes a dma-buf for jobs to name. The claim keeps the descriptor until the buffer is
    /// removed; its size is measured once here.
    pub fn add_buffer(&mut self, fd: OwnedFd) -> Result<BufferId, BufferError> {
        let slot = self
            .buffers
            .iter()
            .position(Option::is_none)
            .ok_or(BufferError::Full)?;
        let size = self
            .gsdma
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
        });
        Ok(BufferId {
            // At most `MAX_BUFFERS`.
            slot: slot as u8,
            generation: self.generation,
        })
    }

    /// The size of a buffer the claim holds.
    pub fn buffer_size(&self, id: BufferId) -> Option<u64> {
        held(&self.buffers, id).map(|b| b.size)
    }

    /// Gives a buffer back, once the job in flight does not use it.
    pub fn remove_buffer(&mut self, id: BufferId) -> Result<OwnedFd, BufferError> {
        held(&self.buffers, id).ok_or(BufferError::Unknown)?;
        if self
            .in_flight
            .is_some_and(|(_, slots)| slots & (1 << id.slot) != 0)
        {
            return Err(BufferError::Busy);
        }
        self.buffers[usize::from(id.slot)]
            .take()
            .map(|b| b.fd)
            .ok_or(BufferError::Unknown)
    }

    /// Whether a job is in flight.
    pub fn busy(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Checks `job` against the claim's buffers and starts it.
    pub fn submit(&mut self, job: &Job<'_>) -> Result<(), SubmitError> {
        if self.in_flight.is_some() {
            return Err(SubmitError::Busy);
        }
        let buffers = &self.buffers;
        let plan = plan(job, |id| held(buffers, id).map(|b| b.size)).map_err(SubmitError::Job)?;
        self.session
            .submit(&Request {
                plan: &plan,
                buffers: &self.buffers,
            })
            .map_err(SubmitError::Device)?;
        self.in_flight = Some((plan.identity(), plan.slots()));
        Ok(())
    }

    /// Waits up to `timeout` (the kernel driver's is [`TIMEOUT`]) for the job in flight.
    pub fn complete(&mut self, timeout: Duration) -> Result<Completion, CompleteError> {
        let (identity, _) = self.in_flight.ok_or(CompleteError::Nothing)?;
        match self.session.wait(timeout) {
            Ok(true) => {
                self.in_flight = None;
                Ok(Completion { identity })
            }
            Ok(false) => Err(CompleteError::Timeout),
            Err(error) => Err(CompleteError::Device(error)),
        }
    }
}
