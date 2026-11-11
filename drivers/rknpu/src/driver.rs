//! [`Rknpu`]: the NPU as `npu/1`, a [`Claim`] on its node serving the device's job stream.
//!
//! # The board file
//!
//! ```toml
//! [[devices]]
//! id = "npu"
//! driver = "rknpu"
//! config = { node = "/dev/dri/renderD129" }   # optional
//! [devices.streams.jobs]
//! buffers = 8
//! buffer_bytes = 4194304
//! ```
//!
//! - `node`: the NPU's render node. Left out, it is found each time the node is opened: the one
//!   render node under `/sys/class/drm` whose device is bound to the NPU's driver (two such
//!   nodes, or none, is a failed open). Nothing is opened at bind: the first tick opens it.
//! - The stream named [`JOB_STREAM`] holds the programs, the buffers attached to them and the
//!   result buffers, and carries the runs. Shared memory or dma-bufs: the driver reads and writes
//!   the buffers by copying. At most [`MAX_BUFFERS`] buffers; more is refused at bind.
//!
//! # Confinement
//!
//! Everything [`crate::claim`] says holds here: each program in an IOMMU domain of its own
//! holding only its objects, checked before every run. One thing is the board's, not this
//! driver's: the daemon must be the node's only opener (the device node's permissions), because
//! the kernel's domains are shared by every opener.
//!
//! # Callers
//!
//! A program belongs to the caller that loaded it. Another caller's `unload`, `attach` and
//! `detach` of it answer `NOT_FOUND`, and when its caller goes, it is unloaded.
//!
//! # Ticks
//!
//! Each tick opens the node if no claim is held, then takes the stream's free buffers and runs
//! the jobs waiting, and asks again after [`POLL_NS`]. A run blocks until the block finishes or
//! the kernel resets it.
//!
//! # Failures
//!
//! Opening the node: [`Service`] backs off from 250 ms, doubling to 8 s, and never gives up; a
//! block not behind an IOMMU is refused on the same schedule. A method called while no claim is
//! held tries to open it, and answers `NO_DEVICE` if it can't. A claim the kernel says is gone
//! ([`Claim::lost`]) is dropped and opened again: its programs are gone with it, and their
//! callers load them again. A stream error is retried on the same backoff. The driver never turns
//! itself off.
//!
//! # Allocation
//!
//! Opening the node, loading and attaching allocate. A tick with a claim held allocates nothing.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E32.md` and
//! `testing/board-runs/G2.md`.

use std::io;
use std::path::{Path, PathBuf};

use lemnos_contract_npu::{Counters, Npu, NpuInfo};
use lemnos_core::{CallerId, Config, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_driver_rknpu_sys::NpuNode;
use lemnos_hal::{ErrorKind, StreamEndpoint};

use crate::claim::{
    Claim, ClaimError, Device, MAX_BUFFERS, MAX_PROGRAMS, Served, Service, Session, backoff_ns,
};
use crate::node::find_in;

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";

/// How often the stream is looked at.
pub const POLL_NS: u64 = 1_000_000;

/// The job stream, as the driver holds it.
type JobStream = dyn StreamEndpoint<Error = ErrorKind> + Send;

/// A session, as the driver holds it.
pub type BoxedSession = Box<dyn Session + Send>;

/// What opens a session on the node, as the driver holds it.
pub type Opener = Box<dyn FnMut() -> io::Result<BoxedSession> + Send>;

/// The node as the driver holds it: whatever its [`Opener`] opens.
pub struct Node(Opener);

impl Device for Node {
    type Session = BoxedSession;

    fn open(&mut self) -> io::Result<BoxedSession> {
        (self.0)()
    }
}

/// Why the last open of the node failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// Not opened yet.
    NotYet,
    /// The block is not behind an IOMMU.
    NotConfined,
    /// Opening or asking the node failed.
    Device,
    /// The kernel said the node's device was gone; the claim was dropped.
    Lost,
}

/// The Rockchip NPU as `npu/1`.
#[driver("rknpu", provides = [Npu])]
pub struct Rknpu {
    #[needs]
    jobs: StreamHandle,
    service: Service<Node>,
    claim: Option<Claim<Node>>,
    /// Why there is no claim.
    closed: Closed,
    /// Opens failed in a row, for a block not behind an IOMMU (the service counts the rest).
    refusals: u32,
    /// When a claim is tried again.
    claim_at_ns: u64,
    /// The host's clock at the last tick.
    now_ns: u64,
    /// The caller of the method running now.
    caller: CallerId,
    /// Who loaded each program, by program index.
    owners: [Option<CallerId>; MAX_PROGRAMS],
    /// The stream's buffers a dropped claim held, for the next one.
    held: u64,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    stream_retry_at_ns: u64,
}

impl Lifecycle for Rknpu {
    type Needs = RknpuNeeds;
    const TICKS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn start(needs: RknpuNeeds, cfg: &Config<'_>) -> Result<Self> {
        let node = match cfg.str("node") {
            Some("") => return Err(Status::INVALID_ARGUMENT),
            Some(path) => Some(PathBuf::from(path)),
            None => None,
        };
        let open: Opener = Box::new(move || {
            let path = node
                .clone()
                .or_else(|| find_in(Path::new("/sys")))
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            Ok(Box::new(NpuNode::new(path).open()?) as BoxedSession)
        });
        Self::with_opener(needs, open)
    }

    fn health(&self) -> Health {
        if self.claim.is_none() {
            return match self.closed {
                Closed::NotConfined => {
                    Health::Failed("the block is not behind an IOMMU: nothing would confine it")
                }
                Closed::NotYet => Health::Degraded("the node is not open yet"),
                Closed::Device => Health::Degraded("the node failed to open: trying it again"),
                Closed::Lost => Health::Degraded("the node went away: opening it again"),
            };
        }
        if self.stream_failures > 0 {
            return Health::Degraded("the job stream failed: trying it again");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.now_ns = now_ns;
        if self.claimed().is_err() {
            return Some(self.claim_at_ns.max(now_ns.saturating_add(POLL_NS)));
        }
        if self.stream_failures > 0 && now_ns < self.stream_retry_at_ns {
            return Some(self.stream_retry_at_ns);
        }
        let claim = self.claim.as_mut()?;
        let served = claim.serve(&mut *self.jobs, now_ns);
        if claim.lost() {
            self.drop_claim();
        }
        match served {
            Ok(_) => {
                self.stream_failures = 0;
                Some(now_ns.saturating_add(POLL_NS))
            }
            Err(_) => {
                self.stream_failures = self.stream_failures.saturating_add(1);
                self.stream_retry_at_ns = now_ns.saturating_add(backoff_ns(self.stream_failures));
                Some(self.stream_retry_at_ns)
            }
        }
    }

    fn caller(&mut self, caller: CallerId) {
        self.caller = caller;
    }

    fn caller_gone(&mut self, caller: CallerId) {
        for index in 0..MAX_PROGRAMS {
            if self.owners[index] == Some(caller) {
                self.owners[index] = None;
                if let Some(claim) = self.claim.as_mut() {
                    let _ = claim.unload(index as u32 + 1);
                }
            }
        }
        self.check_lost();
    }
}

impl Rknpu {
    /// The driver over the node `open` opens, for a test or a host that builds it directly.
    /// Nothing is opened here. `INVALID_ARGUMENT` for a stream of more than [`MAX_BUFFERS`].
    pub fn with_opener(needs: RknpuNeeds, open: Opener) -> Result<Self> {
        if needs.jobs.buffer_count() > MAX_BUFFERS {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            jobs: needs.jobs,
            service: Service::new(Node(open)),
            claim: None,
            closed: Closed::NotYet,
            refusals: 0,
            claim_at_ns: 0,
            now_ns: 0,
            caller: CallerId::LOCAL,
            owners: [None; MAX_PROGRAMS],
            held: 0,
            stream_failures: 0,
            stream_retry_at_ns: 0,
        })
    }

    /// The claim held, if any.
    pub fn claim(&self) -> Option<&Claim<Node>> {
        self.claim.as_ref()
    }

    /// Why no claim is held, when none is.
    pub fn closed(&self) -> Option<Closed> {
        self.claim.is_none().then_some(self.closed)
    }

    /// The claim, opened now if none is held and its time has come.
    fn claimed(&mut self) -> Result<&mut Claim<Node>> {
        if self.claim.is_none() {
            if self.now_ns < self.claim_at_ns {
                return Err(Status::NO_DEVICE);
            }
            match self.service.claim(self.now_ns) {
                Ok(mut claim) => {
                    claim.adopt(core::mem::take(&mut self.held));
                    self.refusals = 0;
                    self.owners = [None; MAX_PROGRAMS];
                    self.claim = Some(claim);
                }
                Err(ClaimError::NotConfined) => {
                    self.refusals = self.refusals.saturating_add(1);
                    self.claim_at_ns = self.now_ns.saturating_add(backoff_ns(self.refusals));
                    self.closed = Closed::NotConfined;
                }
                Err(
                    ClaimError::Device { retry_at_ns, .. } | ClaimError::BackingOff { retry_at_ns },
                ) => {
                    self.claim_at_ns = retry_at_ns;
                    if self.closed != Closed::Lost {
                        self.closed = Closed::Device;
                    }
                }
                // Never: the driver holds the only claim, and it is `None` here.
                Err(ClaimError::Claimed) => {}
            }
        }
        self.claim.as_mut().ok_or(Status::NO_DEVICE)
    }

    fn drop_claim(&mut self) {
        if let Some(claim) = self.claim.take() {
            self.held = claim.held();
        }
        self.owners = [None; MAX_PROGRAMS];
        self.closed = Closed::Lost;
    }

    fn check_lost(&mut self) {
        if self.claim.as_ref().is_some_and(Claim::lost) {
            self.drop_claim();
        }
    }

    /// Whether the caller now may change program `program`.
    fn owns(&self, program: u32) -> bool {
        let index = usize::try_from(program)
            .ok()
            .and_then(|p| p.checked_sub(1))
            .filter(|&index| index < MAX_PROGRAMS);
        index.is_some_and(|index| self.owners[index] == Some(self.caller))
    }

    /// Runs `f` on the contract served by the claim, and drops a claim the kernel says is gone.
    fn served<T>(
        &mut self,
        f: impl FnOnce(&mut Served<'_, Node, JobStream>) -> Result<T>,
    ) -> Result<T> {
        self.claimed()?;
        let Some(claim) = self.claim.as_mut() else {
            return Err(Status::NO_DEVICE);
        };
        let mut served = Served {
            claim,
            stream: &mut *self.jobs,
            name: JOB_STREAM,
        };
        let result = f(&mut served);
        self.check_lost();
        result
    }
}

impl Npu for Rknpu {
    fn info(&mut self) -> Result<NpuInfo> {
        self.served(|npu| npu.info())
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = JOB_STREAM.as_bytes();
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn load(&mut self, buffer: u32, bytes: u32) -> Result<u32> {
        let program = self.served(|npu| npu.load(buffer, bytes))?;
        if let Some(index) = (program as usize)
            .checked_sub(1)
            .filter(|&i| i < MAX_PROGRAMS)
        {
            self.owners[index] = Some(self.caller);
        }
        Ok(program)
    }

    fn unload(&mut self, program: u32) -> Result<()> {
        if !self.owns(program) {
            return Err(Status::NOT_FOUND);
        }
        self.served(|npu| npu.unload(program))?;
        self.owners[program as usize - 1] = None;
        Ok(())
    }

    fn attach(&mut self, program: u32, slot: u16, buffer: u32, bytes: u32) -> Result<()> {
        if !self.owns(program) {
            return Err(Status::NOT_FOUND);
        }
        self.served(|npu| npu.attach(program, slot, buffer, bytes))
    }

    fn detach(&mut self, program: u32, slot: u16) -> Result<()> {
        if !self.owns(program) {
            return Err(Status::NOT_FOUND);
        }
        self.served(|npu| npu.detach(program, slot))
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.claim.as_ref().map(Claim::counters).unwrap_or_default())
    }
}
