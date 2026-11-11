//! The block, a claim on a codec session, the claim's buffers, and sources and destinations
//! submitted and completed.
//!
//! # Sessions
//!
//! Each open of the node is a session of its own in the kernel: its own queues, its own firmware
//! instance, and its own page table in the block's MMU, holding only the pages of buffers queued
//! on it. A [`Claim`] is one session. It borrows its [`Vpu`] mutably, so one `Vpu` gives one claim
//! at a time; the kernel schedules sessions of different processes on the block itself.
//!
//! # Order
//!
//! Take a claim with a [`Setup`]; add the dma-bufs it will use ([`ClaimOf::add_buffer`]); give
//! it destinations ([`ClaimOf::give`]); submit sources ([`ClaimOf::submit`], which starts the
//! session on its first call); take completions from both sides ([`ClaimOf::complete_source`],
//! [`ClaimOf::complete_destination`]), waiting with [`ClaimOf::wait`]. A decoder that asked for
//! no destinations learns the stream's format from [`ClaimOf::source_change`] and then
//! [`ClaimOf::adopt_destinations`]. [`ClaimOf::drain`] finishes the stream: the last
//! destination comes back flagged `V4L2_BUF_FLAG_LAST`.
//!
//! # Failures
//!
//! A claim that fails to open or set up its session for a reason other than the setup itself
//! starts a backoff: until it is due, [`Vpu::claim`] answers [`ClaimError::BackingOff`] without
//! touching the device. The delay doubles from 250 ms, is capped at 8 s, and clears when a claim
//! is taken. The driver never gives up and never turns the block off. A setup the kernel refuses
//! as invalid is the caller's ([`ClaimError::Refused`]) and starts no backoff. A job the kernel
//! refuses comes back as [`SubmitError::Device`]; the claim stays, and dropping it closes the
//! session, which the kernel tears down with its buffers.
//!
//! # Allocation
//!
//! Taking a claim allocates (the session). Adding and removing buffers, submitting, giving,
//! completing and waiting allocate nothing.

use std::io;
use std::ops::DerefMut;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use lemnos_driver_m2m_decode::{
    Dequeued, Drain, Format, M2mSession, MAX_BUFFERS as QUEUE_BUFFERS, Memory, Node,
    PixFormatMplane, PlaneMemory, QueuePlane, QueueSetup, Ready, SourceChange,
};
use lemnos_linux_sys::v4l2::BUF_FLAG_ERROR;

use crate::job::{BufferId, Destination, Held, JobError, Plan, Source, plan};
use crate::setup::{Direction, Setup, SetupError};
use crate::uapi::{DRIVER, MAX_PLANES};

/// Buffers one claim may hold at once.
pub const MAX_BUFFERS: usize = 32;

/// The session's queues hold at most this many buffers each.
const SLOTS: usize = QUEUE_BUFFERS as usize;

/// First delay before a claim is tried again.
const BACKOFF_FIRST_NS: u64 = 250_000_000;

/// The delay after `failures` failures in a row (from 1), doubling to the 8 s cap.
#[must_use]
pub const fn backoff_ns(failures: u32) -> u64 {
    let attempt = if failures == 0 { 0 } else { failures - 1 };
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

/// The node and the facts the claim needs from the system.
pub trait Device {
    /// One open of the node.
    type Node: Node;

    /// Opens the node: a new session.
    fn open(&mut self) -> io::Result<Self::Node>;

    /// The size of dma-buf `fd`, in bytes.
    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64>;
}

/// Why a claim was not taken.
#[derive(Debug)]
pub enum ClaimError {
    /// The setup was refused before anything was opened.
    Setup(SetupError),
    /// The node is not this block's (`QUERYCAP` names another driver).
    NotTheBlock,
    /// The kernel refused the setup: a format or a control it does not take, or a format it
    /// changed. Nothing is held; no backoff.
    Refused(io::Error),
    /// Opening or setting up the session failed; the next claim may be tried from `retry_at_ns`.
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

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(e) => write!(f, "the setup was refused: {e}"),
            Self::NotTheBlock => write!(f, "the node is not the codec block's"),
            Self::Refused(e) => write!(f, "the kernel refused the setup: {e}"),
            Self::Device { error, retry_at_ns } => {
                write!(
                    f,
                    "the session failed ({error}); retrying at {retry_at_ns} ns"
                )
            }
            Self::BackingOff { retry_at_ns } => write!(f, "backing off until {retry_at_ns} ns"),
        }
    }
}

impl std::error::Error for ClaimError {}

/// Why a buffer was not added or removed.
#[derive(Debug)]
pub enum BufferError {
    /// The claim holds [`MAX_BUFFERS`] buffers already.
    Full,
    /// The buffer is empty, or its size could not be read.
    Size(Option<io::Error>),
    /// The buffer is larger than a plane's 32-bit length can say.
    TooLarge,
    /// No such buffer.
    Unknown,
    /// The block holds the buffer (it is queued).
    Busy,
}

/// Why a source or destination was not queued.
#[derive(Debug)]
pub enum SubmitError {
    /// Refused before reaching the device.
    Job(JobError),
    /// The block holds every buffer of that queue; take a completion first.
    Full,
    /// The destinations wait for the stream's format ([`ClaimOf::adopt_destinations`]).
    Pending,
    /// The kernel refused it. Nothing is queued for it.
    Device(io::Error),
}

/// Why a control was not set.
#[derive(Debug)]
pub enum ControlError {
    /// One of [`crate::REFUSED_CONTROLS`]. The device was not touched.
    Refused,
    /// The kernel refused it (`EBUSY` once the session's firmware is loaded, for the controls it
    /// reads only then).
    Device(io::Error),
}

/// A source or destination the block has finished with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The identity it was queued with.
    pub identity: u64,
    /// The kernel's flags (`V4L2_BUF_FLAG_*` and the driver's, [`crate::uapi::flags`]).
    /// `V4L2_BUF_FLAG_ERROR` is added when the kernel reported bytes outside a buffer.
    pub flags: u32,
    /// For a destination: the bytes the block wrote in each plane, from the buffer's start, each
    /// checked to be inside its buffer (all 0 otherwise, with the error flag). 0 for a source.
    pub bytes: [u32; MAX_PLANES],
    /// The planes.
    pub planes: usize,
    /// The timestamp of the source it came from.
    pub timestamp: Duration,
    /// The kernel's sequence number.
    pub sequence: u32,
}

/// The codec block's node, through `D`.
pub struct Vpu<D: Device> {
    pub(crate) device: D,
    pub(crate) failures: u32,
    retry_at_ns: u64,
}

impl<D: Device> Vpu<D> {
    /// The block on `device`. Nothing is opened.
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

    /// Takes a claim: a new session set up as `setup` says. `now_ns` is the caller's clock, for
    /// the backoff.
    pub fn claim(&mut self, setup: &Setup<'_>, now_ns: u64) -> Result<Claim<'_, D>, ClaimError> {
        let session = self.session_for(setup, now_ns)?;
        Ok(ClaimOf::new(self, session, setup.direction))
    }

    /// [`Self::claim`], for a holder that keeps the claim past this borrow (a driver, which
    /// holds its claim across jobs for as long as it serves one consumer): the claim owns the
    /// block, and gives it back with [`ClaimOf::release`]. When no claim is taken the block
    /// comes back with the reason.
    pub fn claim_owned(
        mut self: Box<Self>,
        setup: &Setup<'_>,
        now_ns: u64,
    ) -> Result<OwnedClaim<D>, (Box<Self>, ClaimError)> {
        match self.session_for(setup, now_ns) {
            Ok(session) => Ok(ClaimOf::new(self, session, setup.direction)),
            Err(error) => Err((self, error)),
        }
    }

    /// A session set up as `setup` says, or why not, with the backoff kept.
    fn session_for(
        &mut self,
        setup: &Setup<'_>,
        now_ns: u64,
    ) -> Result<M2mSession<D::Node>, ClaimError> {
        setup.check().map_err(ClaimError::Setup)?;
        if self.failures > 0 && now_ns < self.retry_at_ns {
            return Err(ClaimError::BackingOff {
                retry_at_ns: self.retry_at_ns,
            });
        }
        match self.open(setup) {
            Ok(session) => {
                self.failures = 0;
                Ok(session)
            }
            Err(ClaimError::Device { error, .. }) => {
                self.failures = self.failures.saturating_add(1);
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.failures));
                Err(ClaimError::Device {
                    error,
                    retry_at_ns: self.retry_at_ns,
                })
            }
            Err(other) => Err(other),
        }
    }

    fn open(&mut self, setup: &Setup<'_>) -> Result<M2mSession<D::Node>, ClaimError> {
        let device = |error| ClaimError::Device {
            error,
            retry_at_ns: 0,
        };
        let settle = |error: io::Error| match error.kind() {
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => ClaimError::Refused(error),
            _ => device(error),
        };
        let node = self.device.open().map_err(device)?;
        if node.capability().map_err(device)?.driver != DRIVER {
            return Err(ClaimError::NotTheBlock);
        }
        let queue = |format, buffers| QueueSetup {
            format: Format::Multi(format),
            memory: Memory::DmaBuf,
            buffers,
            crop: None,
        };
        let mut session = M2mSession::new(
            node,
            queue(setup.source, setup.sources),
            queue(setup.destination, setup.destinations),
        )
        .map_err(settle)?;
        let kept = |format: &Format, wanted: &PixFormatMplane| {
            matches!(format, Format::Multi(m) if m.pixelformat == wanted.pixelformat
                && m.num_planes == wanted.num_planes)
        };
        if !kept(session.output_format(), &setup.source)
            || !kept(session.capture_format(), &setup.destination)
        {
            return Err(ClaimError::Refused(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the kernel did not keep a format",
            )));
        }
        for control in setup.controls {
            session
                .set_control(control.id, control.value)
                .map_err(settle)?;
        }
        Ok(session)
    }
}

struct Buffer {
    fd: OwnedFd,
    /// At most `u32::MAX`.
    size: u64,
    generation: u16,
    /// Queued now.
    busy: bool,
}

/// What a V4L2 buffer index of a queue carries while the block holds it.
#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    identity: u64,
    plan: Plan,
}

/// A claim that borrows its block, from [`Vpu::claim`].
pub type Claim<'v, D> = ClaimOf<&'v mut Vpu<D>, D>;

/// A claim that owns its block, from [`Vpu::claim_owned`].
pub type OwnedClaim<D> = ClaimOf<Box<Vpu<D>>, D>;

/// A claim on the block: one session, the buffers it holds, and what the block holds of them.
/// `H` is how it holds its [`Vpu`], borrowed ([`Claim`]) or owned ([`OwnedClaim`]); either way
/// the `Vpu` has this one claim and no other.
pub struct ClaimOf<H: DerefMut<Target = Vpu<D>>, D: Device> {
    vpu: H,
    session: M2mSession<D::Node>,
    direction: Direction,
    buffers: [Option<Buffer>; MAX_BUFFERS],
    generation: u16,
    sources: [Slot; SLOTS],
    destinations: [Slot; SLOTS],
    /// The output queue is streaming (and the capture queue with it, unless it was pending).
    streaming: bool,
    /// The capture queue is streaming.
    capturing: bool,
}

impl<H: DerefMut<Target = Vpu<D>>, D: Device> ClaimOf<H, D> {
    fn new(vpu: H, session: M2mSession<D::Node>, direction: Direction) -> Self {
        Self {
            vpu,
            session,
            direction,
            buffers: std::array::from_fn(|_| None),
            generation: 0,
            sources: [Slot::default(); SLOTS],
            destinations: [Slot::default(); SLOTS],
            streaming: false,
            capturing: false,
        }
    }

    /// Which way the block runs.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// The source format the kernel kept.
    pub fn source_format(&self) -> PixFormatMplane {
        multi(self.session.output_format())
    }

    /// The destination format the kernel has now (after a source change, the stream's).
    pub fn destination_format(&self) -> PixFormatMplane {
        multi(self.session.capture_format())
    }

    /// Source buffers the kernel granted.
    pub fn source_slots(&self) -> u32 {
        self.session.output_buffers()
    }

    /// Destination buffers the kernel granted (0 while they wait for the stream's format).
    pub fn destination_slots(&self) -> u32 {
        if self.session.capture_pending() {
            0
        } else {
            self.session.capture_buffers()
        }
    }

    /// Takes a dma-buf for sources and destinations to name. The claim keeps the descriptor
    /// until the buffer is removed; its size is measured once here.
    pub fn add_buffer(&mut self, fd: OwnedFd) -> Result<BufferId, BufferError> {
        let slot = self
            .buffers
            .iter()
            .position(Option::is_none)
            .ok_or(BufferError::Full)?;
        let size = self
            .vpu
            .device
            .buffer_size(fd.as_fd())
            .map_err(|e| BufferError::Size(Some(e)))?;
        if size == 0 {
            return Err(BufferError::Size(None));
        }
        if size > u64::from(u32::MAX) {
            return Err(BufferError::TooLarge);
        }
        self.generation = self.generation.wrapping_add(1);
        self.buffers[slot] = Some(Buffer {
            fd,
            size,
            generation: self.generation,
            busy: false,
        });
        Ok(BufferId {
            // At most `MAX_BUFFERS`.
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

    /// Gives a buffer back, once the block does not hold it.
    pub fn remove_buffer(&mut self, id: BufferId) -> Result<OwnedFd, BufferError> {
        let buffer = self.buffer(id).ok_or(BufferError::Unknown)?;
        if buffer.busy {
            return Err(BufferError::Busy);
        }
        self.buffers[usize::from(id.slot)]
            .take()
            .map(|b| b.fd)
            .ok_or(BufferError::Unknown)
    }

    fn held(&self, id: BufferId) -> Option<Held> {
        self.buffer(id).map(|b| Held {
            slot: id.slot,
            size: b.size,
            busy: b.busy,
        })
    }

    /// Checks `source` and queues it. The first source starts the session.
    pub fn submit(&mut self, source: &Source<'_>) -> Result<(), SubmitError> {
        let index = self.session.free_output().ok_or(SubmitError::Full)?;
        let need = plane_sizes(self.session.output_format());
        let plan = plan(
            &need.0[..need.1],
            source.planes.iter().map(|p| (p.buffer, Some(p.bytes))),
            |id| self.held(id),
        )
        .map_err(SubmitError::Job)?;
        if !self.streaming {
            self.session.stream_on().map_err(SubmitError::Device)?;
            self.streaming = true;
            self.capturing = !self.session.capture_pending();
        }
        let planes = queue_planes(&self.buffers, &plan);
        self.session
            .queue_output(index, &planes[..plan.planes], source.timestamp)
            .map_err(SubmitError::Device)?;
        self.hold(true, index, source.identity, plan);
        Ok(())
    }

    /// Checks `destination` and queues it for the block to fill.
    pub fn give(&mut self, destination: &Destination<'_>) -> Result<(), SubmitError> {
        if self.session.capture_pending() {
            return Err(SubmitError::Pending);
        }
        let index = self.session.free_capture().ok_or(SubmitError::Full)?;
        let need = plane_sizes(self.session.capture_format());
        let plan = plan(
            &need.0[..need.1],
            destination.planes.iter().map(|&b| (b, None)),
            |id| self.held(id),
        )
        .map_err(SubmitError::Job)?;
        if self.streaming && !self.capturing {
            self.session.start_capture().map_err(SubmitError::Device)?;
            self.capturing = true;
        }
        let planes = queue_planes(&self.buffers, &plan);
        self.session
            .queue_capture(index, &planes[..plan.planes])
            .map_err(SubmitError::Device)?;
        self.hold(false, index, destination.identity, plan);
        Ok(())
    }

    fn hold(&mut self, source: bool, index: u32, identity: u64, plan: Plan) {
        let slots = if source {
            &mut self.sources
        } else {
            &mut self.destinations
        };
        slots[index as usize] = Slot { identity, plan };
        for &slot in &plan.slots[..plan.planes] {
            if let Some(b) = self.buffers[usize::from(slot)].as_mut() {
                b.busy = true;
            }
        }
    }

    fn give_back(&mut self, source: bool, index: usize) -> Slot {
        let slots = if source {
            &mut self.sources
        } else {
            &mut self.destinations
        };
        let Some(slot) = slots.get_mut(index).map(std::mem::take) else {
            return Slot::default();
        };
        for &s in &slot.plan.slots[..slot.plan.planes] {
            if let Some(b) = self.buffers[usize::from(s)].as_mut() {
                b.busy = false;
            }
        }
        slot
    }

    fn release_all(&mut self, source: bool) {
        for index in 0..SLOTS {
            self.give_back(source, index);
        }
    }

    /// Takes back a source the block has finished reading, or `None` when none is done.
    pub fn complete_source(&mut self) -> io::Result<Option<Completion>> {
        let Some(done) = self.session.dequeue_output()? else {
            return Ok(None);
        };
        let slot = self.give_back(true, done.index as usize);
        if slot.plan.planes == 0 {
            return Err(not_held());
        }
        Ok(Some(Completion {
            identity: slot.identity,
            flags: done.flags,
            bytes: [0; MAX_PLANES],
            planes: slot.plan.planes,
            timestamp: done.timestamp,
            sequence: done.sequence,
        }))
    }

    /// Takes back a destination the block has filled, or `None` when none is ready.
    pub fn complete_destination(&mut self) -> io::Result<Option<Completion>> {
        let Some(done) = self.session.dequeue_capture()? else {
            return Ok(None);
        };
        let slot = self.give_back(false, done.index as usize);
        if slot.plan.planes == 0 {
            return Err(not_held());
        }
        let (bytes, flags) = match written(&done, &slot.plan) {
            Some(bytes) => (bytes, done.flags),
            None => ([0; MAX_PLANES], done.flags | BUF_FLAG_ERROR),
        };
        Ok(Some(Completion {
            identity: slot.identity,
            flags,
            bytes,
            planes: slot.plan.planes,
            timestamp: done.timestamp,
            sequence: done.sequence,
        }))
    }

    /// Waits until one of `want` holds or `timeout` passes: a source done (`output`), a
    /// destination filled (`capture`), an event (`event`).
    pub fn wait(&self, want: Ready, timeout: Option<Duration>) -> io::Result<Ready> {
        self.session.wait(want, timeout)
    }

    /// Takes pending events; `Some` when one says the stream's format changed.
    pub fn source_change(&self) -> io::Result<Option<SourceChange>> {
        self.session.take_source_change()
    }

    /// Asks the block to finish what is queued. The last destination comes back flagged
    /// `V4L2_BUF_FLAG_LAST`.
    pub fn drain(&self) -> io::Result<()> {
        self.session.drain(match self.direction {
            Direction::Decode => Drain::Decoder,
            Direction::Encode => Drain::Encoder,
        })
    }

    /// Allocates `count` destination slots for the format the stream now has
    /// ([`Self::destination_format`]): the first time, for a decoder that asked for none; after
    /// a resolution change, once the stream was drained (its last destination taken). Every
    /// destination the block held comes back, without a completion. Then give destinations.
    pub fn adopt_destinations(&mut self, count: u32) -> io::Result<()> {
        if self.session.capture_pending() {
            self.session.adopt_capture(count)?;
        } else {
            self.session.reconfigure_capture(count)?;
            self.release_all(false);
        }
        self.capturing = false;
        Ok(())
    }

    /// Ends the claim: the session is closed, which makes the kernel tear it down with every
    /// buffer it was given, and the claim's buffers are let go. Gives back how the block was
    /// held: for an [`OwnedClaim`], the [`Vpu`] itself, with its backoff, to claim again.
    pub fn release(self) -> H {
        let Self { vpu, session, .. } = self;
        drop(session);
        vpu
    }

    /// Stops the session: every source and destination comes back without a completion, and
    /// the next [`Self::submit`] starts it again.
    pub fn stop(&mut self) -> io::Result<()> {
        if self.streaming {
            self.session.stream_off()?;
        }
        self.streaming = false;
        self.capturing = false;
        self.release_all(true);
        self.release_all(false);
        Ok(())
    }

    /// Sets a control on the session (`VIDIOC_S_CTRL`) and answers the value the kernel kept.
    /// The controls in [`crate::REFUSED_CONTROLS`] are refused.
    pub fn set_control(&mut self, id: u32, value: i32) -> Result<i32, ControlError> {
        if crate::REFUSED_CONTROLS.contains(&id) {
            return Err(ControlError::Refused);
        }
        self.session
            .set_control(id, value)
            .map_err(ControlError::Device)
    }
}

fn not_held() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the kernel returned a buffer the claim had not queued",
    )
}

/// The multi-planar format of a queue the claim set up as one.
fn multi(format: &Format) -> PixFormatMplane {
    match format {
        Format::Multi(m) => *m,
        Format::Single(_) => PixFormatMplane::default(),
    }
}

/// The plane sizes of a queue's format, and how many.
fn plane_sizes(format: &Format) -> ([u32; MAX_PLANES], usize) {
    let mut sizes = [0; MAX_PLANES];
    let Format::Multi(m) = format else {
        return (sizes, 0);
    };
    let n = (m.num_planes as usize).min(MAX_PLANES);
    for (to, from) in sizes.iter_mut().zip(&m.planes[..n]) {
        *to = from.size_image;
    }
    (sizes, n)
}

/// The planes to queue for a checked plan.
fn queue_planes<'a>(
    buffers: &'a [Option<Buffer>; MAX_BUFFERS],
    plan: &Plan,
) -> [QueuePlane<'a>; MAX_PLANES] {
    std::array::from_fn(|i| {
        let fd = buffers
            .get(usize::from(plan.slots[i]))
            .and_then(Option::as_ref)
            .filter(|_| i < plan.planes);
        QueuePlane {
            memory: match fd {
                Some(b) => PlaneMemory::Dmabuf(b.fd.as_fd()),
                None => PlaneMemory::Mmap,
            },
            length: plan.lengths[i],
            bytes_used: plan.bytes[i],
            data_offset: 0,
        }
    })
}

/// The bytes the block wrote in each plane of a destination, when the kernel's counts are
/// inside the buffers it was queued with.
fn written(done: &Dequeued, plan: &Plan) -> Option<[u32; MAX_PLANES]> {
    if done.num_planes as usize != plan.planes {
        return None;
    }
    let mut bytes = [0; MAX_PLANES];
    let kernel = done.bytes_used.iter().zip(&done.data_offset);
    let planes = bytes.iter_mut().zip(kernel).zip(&plan.lengths);
    for ((to, (&used, &offset)), &length) in planes.take(plan.planes) {
        if used > length || offset > used {
            return None;
        }
        *to = used;
    }
    Some(bytes)
}

impl<H: DerefMut<Target = Vpu<D>>, D: Device> ClaimOf<H, D>
where
    D::Node: AsFd,
{
    /// Issues one of the block's private requests (`VIDIOC_G_MVX_*`, `VIDIOC_S_MVX_*`) on the
    /// claim's session, with `payload` as its structure, which the kernel reads and writes
    /// back. Only the requests `lemnos-driver-k230-vpu-sys` lists as sound are issued, each
    /// with a payload of exactly its structure's size; any other is `InvalidInput` and
    /// reaches nothing. The bytes are the caller's settings and are not read here.
    pub fn private(&mut self, request: u32, payload: &mut [u8]) -> io::Result<()> {
        lemnos_driver_k230_vpu_sys::private(self.session.node().as_fd(), request, payload)
    }
}
