//! [`Vip9000Driver`]: the block as `npu/1`, a [`Claim`] on its node in `lemnos.ko` serving the
//! device's job stream.
//!
//! # The board file
//!
//! ```toml
//! [[devices]]
//! id = "npu"
//! driver = "vip9000"
//! config = { route = "lemnos", device = "3600000.npu", heap = "reserved" }
//! [devices.streams.jobs]
//! buffers = 8
//! buffer_bytes = 4194304
//! ```
//!
//! - `route`: `"lemnos"`, the block driven by `lemnos.ko` (named in the module's `vip9000`
//!   parameter, so the vendor's driver doesn't bind it), or `"vipcore"`, the vendor's node, on
//!   which no job is run: [`crate::confine::VIPCORE`] has gaps no user-space driver can close, and
//!   the driver binds only to say so in its health and to refuse every load (`NOT_SUPPORTED`).
//! - `device`: the block's device name as the module lists it.
//! - `heap`: a dma-heap under `/dev/dma_heap` the slots' memory comes from.
//! - The stream named [`JOB_STREAM`] holds the programs, the buffers attached to them and the
//!   result buffers, and carries the runs. The driver reads and writes them by copying. At most
//!   [`MAX_BUFFERS`] buffers; more is refused at bind.
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
//! the module stops it.
//!
//! # Failures
//!
//! Opening the node: [`Service`] backs off from 250 ms, doubling to 8 s, and never gives up (a
//! block the module has wedged refuses new streams with `EIO`, and is tried on the same
//! schedule). A method called while no claim is held tries to open it, and answers `NO_DEVICE`
//! if it can't. A claim that is lost ([`Claim::lost`]) is dropped and opened again: its programs
//! are gone with it, and their callers load them again. A stream error is retried on the same
//! backoff. The driver never turns itself off.
//!
//! # Allocation
//!
//! Opening the node, loading and attaching allocate. A tick with a claim held allocates nothing.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/H7a.md`.

use lemnos_contract_npu::{Counters, Npu, NpuInfo};
use lemnos_core::{
    CallerId, Config, ConfigError, Health, Key, Kind, Lifecycle, Result, Status, StreamHandle,
    driver,
};
use lemnos_hal::{ErrorKind, StreamEndpoint};

use crate::claim::{
    Claim, ClaimError, MAX_BUFFERS, MAX_PROGRAMS, Module, Served, Service, Session, backoff_ns,
};
use crate::linux::{LinuxModule, Settings, valid_name};

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";

/// How often the stream is looked at.
pub const POLL_NS: u64 = 1_000_000;

/// The job stream, as the driver holds it.
type JobStream = dyn StreamEndpoint<Error = ErrorKind> + Send;

/// A session, as the driver holds it.
pub type BoxedSession = Box<dyn Session + Send>;

/// What opens a session on the node, as the driver holds it.
pub type Opener = Box<dyn FnMut() -> std::io::Result<BoxedSession> + Send>;

/// The node as the driver holds it: whatever its [`Opener`] opens.
pub struct Node(Opener);

impl Session for BoxedSession {
    fn load(&mut self, program: &[u8]) -> std::io::Result<u32> {
        (**self).load(program)
    }

    fn unload(&mut self, program: u32) -> std::io::Result<()> {
        (**self).unload(program)
    }

    fn stage(&mut self, slot: u16, bytes: u32) -> std::io::Result<()> {
        (**self).stage(slot, bytes)
    }

    fn memory(&mut self, slot: u16) -> Option<&mut [u8]> {
        (**self).memory(slot)
    }

    fn run(&mut self, program: u32, lens: &[u32]) -> std::io::Result<bool> {
        (**self).run(program, lens)
    }
}

impl Module for Node {
    type Session = BoxedSession;

    fn open(&mut self) -> std::io::Result<BoxedSession> {
        (self.0)()
    }
}

/// Which way the block is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `lemnos.ko`.
    Lemnos,
    /// The vendor's `vipcore` node: refused.
    Vipcore,
}

/// Why there is no claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// Not opened yet.
    NotYet,
    /// Opening the node failed.
    Device,
    /// The claim was lost and dropped.
    Lost,
    /// The board names the vendor's node, which confines nothing.
    Vipcore,
}

/// The settings this driver reads from the board file's `config`. `device` and `heap` are read
/// only for the `lemnos` route, and check_config requires them there.
const KEYS: &[Key] = &[
    Key::required("route", Kind::Str),
    Key::optional("device", Kind::Str),
    Key::optional("heap", Kind::Str),
];

/// The VIP9000 as `npu/1`.
#[driver("vip9000", provides = [Npu])]
pub struct Vip9000Driver {
    #[needs(job)]
    jobs: StreamHandle,
    route: Route,
    service: Service<Node>,
    claim: Option<Claim<Node>>,
    closed: Closed,
    claim_at_ns: u64,
    now_ns: u64,
    caller: CallerId,
    owners: [Option<CallerId>; MAX_PROGRAMS],
    held: u64,
    stream_failures: u32,
    stream_retry_at_ns: u64,
}

impl Lifecycle for Vip9000Driver {
    type Needs = Vip9000DriverNeeds;
    const TICKS: bool = true;
    const TRACKS_CALLERS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        match cfg.str("route") {
            Some("lemnos") => {}
            // The vendor's node is named by the route alone; no device or heap is read for it.
            Some("vipcore") => return Ok(()),
            _ => return Err(ConfigError::new("route", "must be lemnos or vipcore")),
        }
        let device = cfg
            .str("device")
            .ok_or(ConfigError::new("device", "is required for route lemnos"))?;
        let heap = cfg
            .str("heap")
            .ok_or(ConfigError::new("heap", "is required for route lemnos"))?;
        if !valid_name(device) {
            return Err(ConfigError::new(
                "device",
                "must be a name that is not empty and has no slash or NUL",
            ));
        }
        if !valid_name(heap) {
            return Err(ConfigError::new(
                "heap",
                "must be a name that is not empty and has no slash or NUL",
            ));
        }
        Ok(())
    }

    fn start(needs: Vip9000DriverNeeds, cfg: &Config<'_>) -> Result<Self> {
        // check_config has accepted the route: it is `vipcore` or `lemnos`.
        if cfg.str("route") == Some("vipcore") {
            return Self::with_opener(
                needs,
                Route::Vipcore,
                Box::new(|| Err(std::io::ErrorKind::Unsupported.into())),
            );
        }
        // The names are checked by check_config, and `Settings::new` applies the same rule.
        let settings = Settings::new(
            cfg.str("device").ok_or(Status::INVALID_ARGUMENT)?,
            cfg.str("heap").ok_or(Status::INVALID_ARGUMENT)?,
        )
        .map_err(|_| Status::INVALID_ARGUMENT)?;
        let mut module = LinuxModule::new(settings);
        let open: Opener = Box::new(move || Ok(Box::new(module.open()?) as BoxedSession));
        Self::with_opener(needs, Route::Lemnos, open)
    }

    fn health(&self) -> Health {
        if self.claim.is_none() {
            return match self.closed {
                Closed::Vipcore => Health::Failed(
                    "the vendor's node confines nothing: name the block in lemnos.ko instead",
                ),
                Closed::NotYet => Health::Degraded("the node is not open yet"),
                Closed::Device => Health::Degraded("the node failed to open: trying it again"),
                Closed::Lost => Health::Degraded("the stream on the block ended: opening it again"),
            };
        }
        if self.stream_failures > 0 {
            return Health::Degraded("the job stream failed: trying it again");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.now_ns = now_ns;
        if self.route == Route::Vipcore {
            return None;
        }
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

impl Vip9000Driver {
    /// The driver over the node `open` opens, for a test or a host that builds it directly.
    /// Nothing is opened here. `INVALID_ARGUMENT` for a stream of more than [`MAX_BUFFERS`].
    pub fn with_opener(needs: Vip9000DriverNeeds, route: Route, open: Opener) -> Result<Self> {
        if needs.jobs.buffer_count() > MAX_BUFFERS {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            jobs: needs.jobs,
            route,
            service: Service::new(Node(open)),
            claim: None,
            closed: if route == Route::Vipcore {
                Closed::Vipcore
            } else {
                Closed::NotYet
            },
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
        if self.route == Route::Vipcore {
            return Err(Status::NOT_SUPPORTED);
        }
        if self.claim.is_none() {
            if self.now_ns < self.claim_at_ns {
                return Err(Status::NO_DEVICE);
            }
            match self.service.claim(self.now_ns) {
                Ok(mut claim) => {
                    claim.adopt(core::mem::take(&mut self.held));
                    self.owners = [None; MAX_PROGRAMS];
                    self.claim = Some(claim);
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

    /// Runs `f` on the contract served by the claim, and drops a claim that is lost.
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

impl Npu for Vip9000Driver {
    fn info(&mut self) -> Result<NpuInfo> {
        if self.route == Route::Vipcore {
            return Ok(NpuInfo {
                max_program_bytes: crate::claim::MAX_PROGRAM_BYTES,
                max_programs: 0,
                max_attached: 0,
            });
        }
        self.served(|npu| npu.info())
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        if self.route == Route::Vipcore {
            return Err(Status::NOT_SUPPORTED);
        }
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
