use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_core::{Contract, Status, Tail};
use lemnos_wire::ring::Layout;
use lemnos_wire::{
    CORE_HASH, DeviceDescription, Frame, LEMNOS, LemnosName, MAX_QUEUED_BYTES, PART_BITS,
    PART_BYTES, PART_HASHES, PART_NAMES, Parts, Planes, VERSION, WireError, append, check_parts,
    decode, decode_description, feature, handle_descriptors, health, package_mode, state,
};

use crate::connection::{Connection, Taker};
use crate::error::Error;
use crate::holds::{Hold, Holds};
use crate::stream::{FrameStream, StreamPlatform};
use crate::values::ValueQueue;

/// Picks a request's answer out of the frames that carry its id.
pub(crate) trait Pick<R>: FnMut(Frame<'_>) -> Option<Result<R, Error>> {}
impl<R, F: FnMut(Frame<'_>) -> Option<Result<R, Error>>> Pick<R> for F {}

const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_CAP: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EVENTS: usize = 64;
/// Job events kept for a reader that hasn't taken them. Past it the oldest go, progress first.
const MAX_JOB_EVENTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Blocking,
    Polled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Running,
    BackingOff,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceHealth {
    Ok,
    Degraded,
    Failed,
}

const fn device_state(code: u8) -> Option<DeviceState> {
    match code {
        state::RUNNING => Some(DeviceState::Running),
        state::BACKOFF => Some(DeviceState::BackingOff),
        state::OFF => Some(DeviceState::Off),
        _ => None,
    }
}

const fn device_health(code: u8) -> Option<DeviceHealth> {
    match code {
        health::OK => Some(DeviceHealth::Ok),
        health::DEGRADED => Some(DeviceHealth::Degraded),
        health::FAILED => Some(DeviceHealth::Failed),
        _ => None,
    }
}

/// One device, as the service lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub index: u32,
    pub name: String,
    pub driver: String,
    pub tags: Vec<String>,
    pub state: DeviceState,
    pub health: DeviceHealth,
    pub reason: String,
    /// The name of the device this one is wired to, from its board entry's `link`: a camera
    /// sensor's names its receiver, and the receiver's names its ISP. `None` when the board
    /// names none, and from a service older than this field. See [`follow`].
    pub link: Option<String>,
}

/// The devices `from` leads to through `link`, in order, starting with `from` itself: a sensor,
/// its receiver, the receiver's ISP. Ends at a device with no `link`, at a `link` naming a
/// device that isn't in `devices`, and before any device already passed, so a board whose
/// links go round in a circle ends too. Empty when `from` isn't in `devices`.
#[must_use]
pub fn follow<'a>(devices: &'a [DeviceEntry], from: &str) -> Vec<&'a DeviceEntry> {
    let mut chain: Vec<&DeviceEntry> = Vec::new();
    let mut next = Some(from);
    while let Some(name) = next {
        let Some(device) = devices.iter().find(|d| d.name == name) else {
            break;
        };
        if chain.iter().any(|seen| seen.index == device.index) {
            break;
        }
        chain.push(device);
        next = device.link.as_deref();
    }
    chain
}

/// Which driver packages the service runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageMode {
    /// None: no package directory is configured.
    Off,
    /// Only packages signed by a key the service trusts.
    Signed,
    /// Any package in its directory.
    Open,
}

/// One package file the service looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub path: String,
    /// The name of the driver it was loaded as. `None` for a file that was refused.
    pub driver: Option<String>,
    /// Why it was refused. Empty for one that was loaded.
    pub reason: String,
}

/// What became of the service's driver packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageList {
    pub mode: PackageMode,
    /// Why the package directory could not be used at all, or empty.
    pub note: String,
    /// Every file the service looked at, loaded or refused, in the order it looked.
    pub packages: Vec<PackageEntry>,
}

/// One contract of one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    name: String,
    tag: String,
    device: u32,
    contract: u8,
    generation: u64,
}

impl Handle {
    pub const fn device(&self) -> u32 {
        self.device
    }

    pub const fn contract(&self) -> u8 {
        self.contract
    }
}

/// A subscription. Its identity survives a reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscription {
    pub id: u64,
    /// The period the service granted.
    pub period_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueInfo {
    pub subscription: u64,
    pub stamp_ns: u64,
    pub len: usize,
}

/// A device changing state, delivered when the session asked for events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub device: u32,
    pub state: DeviceState,
    pub health: DeviceHealth,
    pub reason: String,
}

/// What happened to a long job this session started ([`Session::start_job`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// `done` of `total` (0 when not known), in the unit the contract says.
    Progress { job: u64, done: u64, total: u64 },
    /// The job is over. `result` is what the contract says it finishes with, when `status` is
    /// success, and empty otherwise. `CANCELLED` for a job that was cancelled; `INTERRUPTED`
    /// for one whose driver was lost, or whose connection ended: the service keeps nothing of a
    /// job across a restart, so a job the connection was lost under is over.
    Done {
        job: u64,
        status: Status,
        result: Vec<u8>,
    },
}

impl JobEvent {
    /// The job it is about.
    pub const fn job(&self) -> u64 {
        match self {
            Self::Progress { job, .. } | Self::Done { job, .. } => *job,
        }
    }
}

/// What [`Session::poll`] delivers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Incoming<'a> {
    Reply {
        id: u32,
        bytes: &'a [u8],
    },
    Refused {
        id: u32,
        status: Status,
        text: &'a str,
    },
    Value {
        subscription: u64,
        stamp_ns: u64,
        bytes: &'a [u8],
    },
    /// The answer to [`Session::send_job`]: request `id` started job `job`.
    JobStarted {
        id: u32,
        job: u64,
    },
    /// Something happened to a long job.
    Job(&'a JobEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubState {
    Opening(u32),
    Subscribing(u32),
    Live(u64),
    Waiting,
}

#[derive(Debug)]
struct Sub {
    local: u64,
    name: String,
    tag: String,
    method: u16,
    period_us: u64,
    only_changes: bool,
    state: SubState,
    /// Refused only stops a subscription that never worked.
    established: bool,
}

/// Everything a session knows apart from its socket.
#[derive(Debug)]
struct Shared {
    ready: bool,
    next_id: u32,
    retry_delay: Duration,
    subs: Vec<Sub>,
    sub_retry: Option<(Instant, Duration)>,
    values: ValueQueue,
    events: VecDeque<Event>,
    /// Long jobs started on this connection and not over yet.
    jobs: Vec<u64>,
    /// Jobs that ended before their JobStarted was read, so it doesn't open them again.
    ended_early: VecDeque<u64>,
    job_events: VecDeque<JobEvent>,
    host: String,
    /// What the service said it can do, in its Welcome.
    features: u64,
    /// The parts the service's Welcome was compared with: what is refused, and its hashes.
    refusal: Parts,
    /// Requests whose answers nobody waits for: giving back a stream that couldn't be set up, and
    /// closing a stream that was dropped.
    unheard: Vec<u32>,
    /// The streams this session holds, and the closes owed for dropped ones.
    holds: Arc<Holds>,
    rejected: Option<Error>,
    close: bool,
}

/// What the service answered a stream being opened with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamAnswer {
    layout: Layout,
    descriptors: u8,
    /// StreamRingHandles: a descriptor per buffer, or per plane of each. Otherwise StreamRings.
    handles: bool,
    /// Each buffer's planes, as StreamRingHandles gave them; `None` when what it gave is no
    /// planes of the buffer. One plane for StreamRings.
    planes: Option<Planes>,
    /// Whether the request said this client takes StreamRingHandles.
    asked_handles: bool,
    /// The producer's working set (`StreamRings::working`).
    working: u32,
    /// Whether the producer's frames are job results (`StreamRings::kind` is 1).
    job_driven: bool,
}

impl Shared {
    /// The error for a feature bit this connection does not use: the part's name and both hashes
    /// when the part was refused at the handshake, otherwise the service did not report it.
    fn unusable(&self, bit: u64, text: &str) -> Error {
        let index = PART_BITS.iter().position(|&part| part == bit);
        if let Some(index) = index.filter(|_| self.refusal.refused & bit != 0) {
            return Error::PartMismatch {
                part: PART_NAMES[index],
                ours: PART_HASHES[index],
                theirs: self.refusal.theirs[index],
            };
        }
        Error::Refused {
            status: Status::NOT_SUPPORTED,
            text: text.to_string(),
        }
    }

    fn id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    fn arm_subscriptions(&mut self, tx: &mut Vec<u8>) {
        for index in 0..self.subs.len() {
            if self.subs[index].state != SubState::Waiting {
                continue;
            }
            let id = self.id();
            let sub = &mut self.subs[index];
            sub.state = SubState::Opening(id);
            let open = Frame::Open {
                id,
                device: &sub.name,
                tag: &sub.tag,
            };
            if append(&open, tx).is_err() {
                sub.state = SubState::Waiting;
            }
        }
    }

    fn job_event(&mut self, event: JobEvent) {
        if let JobEvent::Progress { job, .. } = event
            && let Some(queued) = self
                .job_events
                .iter_mut()
                .rev()
                .find(|queued| queued.job() == job)
            && matches!(queued, JobEvent::Progress { .. })
        {
            *queued = event;
            return;
        }
        if self.job_events.len() >= MAX_JOB_EVENTS {
            let progress = self
                .job_events
                .iter()
                .position(|queued| matches!(queued, JobEvent::Progress { .. }));
            self.job_events.remove(progress.unwrap_or(0));
        }
        self.job_events.push_back(event);
    }

    fn sub_refused(&mut self) {
        let delay = self
            .sub_retry
            .map_or(RETRY_FIRST, |(_, d)| (d * 2).min(RETRY_CAP));
        self.sub_retry = Some((Instant::now() + delay, delay));
    }

    /// Takes the frames the session itself answers. Whatever it returns goes
    /// on to whoever is waiting.
    fn route(&mut self, frame: Frame<'_>, tx: &mut Vec<u8>) -> bool {
        match frame {
            Frame::Reply { id, .. } | Frame::Refusal { id, .. } if self.unheard.contains(&id) => {
                self.unheard.retain(|&unheard| unheard != id);
                false
            }
            Frame::Welcome {
                version,
                features,
                host,
                parts,
                ..
            } => {
                if version != VERSION {
                    self.rejected = Some(Error::Version {
                        ours: VERSION,
                        theirs: version,
                    });
                    self.close = true;
                    return false;
                }
                let Ok(parts) = check_parts(parts.0) else {
                    self.rejected = Some(Error::Wire(WireError::Malformed));
                    self.close = true;
                    return false;
                };
                host.clone_into(&mut self.host);
                // A part the service and this client disagree on is refused by name; the rest works.
                self.features = features & !parts.refused;
                self.refusal = parts;
                self.ready = true;
                self.retry_delay = RETRY_FIRST;
                self.sub_retry = None;
                self.arm_subscriptions(tx);
                false
            }
            Frame::Opened {
                id,
                device,
                contract,
            } => {
                if let Some(sub) = self
                    .subs
                    .iter()
                    .position(|s| s.state == SubState::Opening(id))
                {
                    let next = self.id();
                    let sub = &mut self.subs[sub];
                    sub.state = SubState::Subscribing(next);
                    let subscribe = Frame::Subscribe {
                        id: next,
                        device,
                        contract,
                        method: sub.method,
                        period_us: sub.period_us,
                        only_changes: sub.only_changes,
                    };
                    if append(&subscribe, tx).is_err() {
                        sub.state = SubState::Waiting;
                    }
                    return false;
                }
                true
            }
            Frame::Subscribed {
                id, subscription, ..
            } => {
                if let Some(sub) = self
                    .subs
                    .iter_mut()
                    .find(|s| s.state == SubState::Subscribing(id))
                {
                    sub.state = SubState::Live(subscription);
                    sub.established = true;
                }
                true
            }
            Frame::Refusal { id, status, text } => {
                if !self.ready {
                    self.rejected = Some(Error::Refused {
                        status: Status(status),
                        text: text.to_string(),
                    });
                    self.close = true;
                    return false;
                }
                let matches = |s: &Sub| {
                    s.state == SubState::Opening(id) || s.state == SubState::Subscribing(id)
                };
                if let Some(sub) = self.subs.iter_mut().find(|s| matches(s))
                    && sub.established
                {
                    sub.state = SubState::Waiting;
                    self.sub_refused();
                    return false;
                }
                true
            }
            Frame::JobStarted { job, .. } => {
                if let Some(at) = self.ended_early.iter().position(|ended| *ended == job) {
                    self.ended_early.remove(at);
                } else {
                    self.jobs.push(job);
                }
                true
            }
            Frame::JobProgress { job, done, total } => {
                self.job_event(JobEvent::Progress { job, done, total });
                false
            }
            Frame::JobDone { job, status, bytes } => {
                if let Some(at) = self.jobs.iter().position(|open| *open == job) {
                    self.jobs.swap_remove(at);
                } else {
                    if self.ended_early.len() >= MAX_JOB_EVENTS {
                        self.ended_early.pop_front();
                    }
                    self.ended_early.push_back(job);
                }
                self.job_event(JobEvent::Done {
                    job,
                    status: Status(status),
                    result: bytes.to_vec(),
                });
                false
            }
            Frame::Event {
                device,
                state,
                health,
                reason,
            } => {
                let (Some(state), Some(health)) = (device_state(state), device_health(health))
                else {
                    self.close = true;
                    return false;
                };
                if self.events.len() == MAX_EVENTS {
                    self.events.pop_front();
                }
                self.events.push_back(Event {
                    device,
                    state,
                    health,
                    reason: reason.to_string(),
                });
                false
            }
            _ => true,
        }
    }
}

/// Which request a frame answers.
const fn answers(frame: &Frame<'_>) -> Option<u32> {
    match *frame {
        Frame::Device { id, .. }
        | Frame::ListDone { id }
        | Frame::Opened { id, .. }
        | Frame::Description { id, .. }
        | Frame::Reply { id, .. }
        | Frame::Subscribed { id, .. }
        | Frame::StreamRings { id, .. }
        | Frame::StreamRingHandles { id, .. }
        | Frame::Endpoints { id }
        | Frame::Package { id, .. }
        | Frame::PackagesDone { id, .. }
        | Frame::JobStarted { id, .. }
        | Frame::Refusal { id, .. } => Some(id),
        _ => None,
    }
}

/// A function's bulk endpoints, borrowed from the service: what [`Session::usb_endpoints`] gives.
/// Each is a descriptor that blocks, as the function's own files do.
#[derive(Debug)]
pub struct UsbEndpoints {
    /// The OUT endpoint: what the host sent to the device is read from it.
    pub out: OwnedFd,
    /// The IN endpoint: what the device sends is written to it.
    pub input: OwnedFd,
}

/// A connection to the service that one thread owns.
#[derive(Debug)]
pub struct Session {
    path: PathBuf,
    name: String,
    events_wanted: bool,
    mode: Mode,
    timeout: Duration,
    connection: Option<Connection>,
    /// Goes up every time a connection ends.
    generation: u64,
    next_try: Instant,
    next_local: u64,
    shared: Shared,
}

impl Session {
    /// Connects, blocking until the service answers. An error here means no
    /// service is there; [`Session::connecting`] keeps trying instead.
    pub fn connect(path: impl AsRef<Path>, name: &str) -> Result<Self, Error> {
        Self::started(path.as_ref(), name, Mode::Blocking)
    }

    /// Connects for use with [`Session::poll`], which never waits.
    pub fn connect_polled(path: impl AsRef<Path>, name: &str) -> Result<Self, Error> {
        Self::started(path.as_ref(), name, Mode::Polled)
    }

    /// A session that connects whenever the service appears, for a program
    /// that may start first.
    pub fn connecting(path: impl AsRef<Path>, name: &str) -> Self {
        Self::new(path.as_ref(), name, Mode::Blocking)
    }

    pub(crate) fn started(path: &Path, name: &str, mode: Mode) -> Result<Self, Error> {
        let mut session = Self::new(path, name, mode);
        session.settle()?;
        Ok(session)
    }

    pub(crate) fn new(path: &Path, name: &str, mode: Mode) -> Self {
        Self {
            path: path.to_path_buf(),
            name: name.to_string(),
            events_wanted: true,
            mode,
            timeout: REQUEST_TIMEOUT,
            connection: None,
            generation: 0,
            next_try: Instant::now(),
            next_local: 0,
            shared: Shared {
                ready: false,
                next_id: 0,
                retry_delay: RETRY_FIRST,
                subs: Vec::new(),
                sub_retry: None,
                values: ValueQueue::new(),
                events: VecDeque::new(),
                jobs: Vec::new(),
                ended_early: VecDeque::new(),
                job_events: VecDeque::new(),
                host: String::new(),
                features: 0,
                refusal: Parts::default(),
                unheard: Vec::new(),
                holds: Arc::new(Holds::default()),
                rejected: None,
                close: false,
            },
        }
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    #[cfg(feature = "tokio")]
    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The service's name, once connected.
    pub fn host(&self) -> &str {
        &self.shared.host
    }

    pub const fn is_connected(&self) -> bool {
        self.shared.ready
    }

    /// Goes up every time a connection ends.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Values dropped because nothing was reading them.
    pub const fn dropped_values(&self) -> u64 {
        self.shared.values.dropped()
    }

    pub fn next_event(&mut self) -> Option<Event> {
        self.shared.events.pop_front()
    }

    /// The oldest thing that happened to a long job this session started, if any has.
    pub fn next_job_event(&mut self) -> Option<JobEvent> {
        self.shared.job_events.pop_front()
    }

    /// The oldest thing that happened to a long job this session started, waiting up to
    /// `timeout` for one.
    pub fn wait_job_event(&mut self, timeout: Duration) -> Option<JobEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = self.shared.job_events.pop_front() {
                return Some(event);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let wait = match self.mode {
                Mode::Blocking => Some(left),
                Mode::Polled => self.wait_slice(),
            };
            self.turn(wait, &mut |_| false);
        }
    }

    /// Jobs this session started that haven't ended.
    pub fn open_jobs(&self) -> &[u64] {
        &self.shared.jobs
    }

    #[cfg(feature = "tokio")]
    pub(crate) fn stream(&self) -> Option<&UnixStream> {
        self.connection.as_ref().map(|c| &c.stream)
    }

    #[cfg(feature = "tokio")]
    pub(crate) fn has_unsent(&self) -> bool {
        self.connection.as_ref().is_some_and(|c| c.unsent() > 0)
    }

    pub(crate) fn until_next_try(&self) -> Duration {
        self.next_try.saturating_duration_since(Instant::now())
    }

    fn lost(&mut self) {
        // The service cancels a job when its starter's connection ends, and keeps nothing of
        // one across a restart: every open job is over.
        for job in core::mem::take(&mut self.shared.jobs) {
            self.shared.job_event(JobEvent::Done {
                job,
                status: Status::INTERRUPTED,
                result: Vec::new(),
            });
        }
        self.shared.ended_early.clear();
        self.connection = None;
        self.shared.ready = false;
        self.shared.features = 0;
        self.shared.refusal = Parts::default();
        self.shared.unheard.clear();
        self.shared.close = false;
        self.generation += 1;
        for sub in &mut self.shared.subs {
            sub.state = SubState::Waiting;
        }
        self.arm_retry();
    }

    fn arm_retry(&mut self) {
        self.next_try = Instant::now() + self.shared.retry_delay;
        self.shared.retry_delay = (self.shared.retry_delay * 2).min(RETRY_CAP);
    }

    fn try_connect(&mut self) {
        if self.connection.is_some() || Instant::now() < self.next_try {
            return;
        }
        let Ok(stream) = UnixStream::connect(&self.path) else {
            self.arm_retry();
            return;
        };
        if stream.set_nonblocking(self.mode == Mode::Polled).is_err() {
            self.arm_retry();
            return;
        }
        let mut connection = Connection::new(stream);
        let hello = Frame::Hello {
            core: CORE_HASH,
            lemnos: LemnosName(LEMNOS),
            version: VERSION,
            name: &self.name,
            events: self.events_wanted,
            parts: Tail(&PART_BYTES),
        };
        if append(&hello, &mut connection.tx).is_err() {
            self.arm_retry();
            return;
        }
        self.connection = Some(connection);
    }

    fn retry_subscriptions(&mut self) {
        let Some((at, _)) = self.shared.sub_retry else {
            return;
        };
        if !self.shared.ready || Instant::now() < at {
            return;
        }
        if let Some(connection) = self.connection.as_mut() {
            self.shared.sub_retry = None;
            self.shared.arm_subscriptions(&mut connection.tx);
        }
    }

    fn drain(&mut self, handler: &mut dyn FnMut(Frame<'_>) -> bool) -> Option<usize> {
        let connection = self.connection.as_mut()?;
        let Connection {
            rx, start, end, tx, ..
        } = connection;
        let mut count = 0;
        loop {
            match decode(&rx[*start..*end]) {
                Ok(None) => return Some(count),
                Err(WireError::LayoutMismatch(peer)) => {
                    self.shared.rejected = Some(Error::LayoutMismatch {
                        ours: CORE_HASH,
                        theirs: peer.core,
                        theirs_lemnos: peer.lemnos().to_string(),
                    });
                    self.shared.close = true;
                    return None;
                }
                Err(_) => return None,
                Ok(Some((frame, used))) => {
                    *start += used;
                    count += 1;
                    let passed = self.shared.route(frame, tx);
                    if self.shared.close {
                        return None;
                    }
                    if !passed || handler(frame) {
                        continue;
                    }
                    if let Frame::Value {
                        subscription,
                        stamp_ns,
                        bytes,
                    } = frame
                    {
                        let shared = &mut self.shared;
                        if let Some(local) = local_id(&shared.subs, subscription) {
                            shared.values.push(local, stamp_ns, bytes);
                        }
                    }
                }
            }
        }
    }

    /// Moves everything one step. `wait` is how long a blocking read may wait.
    /// Returns how much happened, 0 when there was nothing to do.
    pub(crate) fn turn(
        &mut self,
        wait: Option<Duration>,
        handler: &mut dyn FnMut(Frame<'_>) -> bool,
    ) -> usize {
        self.try_connect();
        self.retry_subscriptions();
        self.send_closing();
        let Some(connection) = self.connection.as_mut() else {
            if let Some(wait) = wait {
                let pause = wait
                    .min(self.until_next_try())
                    .max(Duration::from_millis(1));
                std::thread::sleep(pause);
            }
            return 0;
        };
        if connection.flush().is_err() {
            self.lost();
            return 0;
        }
        let Some(mut moved) = self.drain(handler) else {
            self.lost();
            return 0;
        };
        if moved == 0 {
            let Some(connection) = self.connection.as_mut() else {
                return 0;
            };
            let filled = wait
                .map_or(Ok(()), |wait| {
                    if self.mode == Mode::Blocking {
                        connection.set_wait(wait)
                    } else {
                        Ok(())
                    }
                })
                .and_then(|()| connection.fill());
            let Ok(filled) = filled else {
                self.lost();
                return 0;
            };
            let Some(decoded) = self.drain(handler) else {
                self.lost();
                return 0;
            };
            moved = decoded.max(usize::from(filled > 0));
        }
        if let Some(connection) = self.connection.as_mut()
            && connection.flush().is_err()
        {
            self.lost();
            return 0;
        }
        moved
    }

    /// In polled mode a helper that must wait does so here.
    fn wait_slice(&self) -> Option<Duration> {
        match self.mode {
            Mode::Blocking => Some(self.timeout),
            Mode::Polled => {
                std::thread::sleep(Duration::from_micros(200));
                None
            }
        }
    }

    /// One step of connecting. `Some` means finished.
    pub(crate) fn try_settle(&mut self, wait: Option<Duration>) -> Option<Result<(), Error>> {
        if self.shared.ready {
            return Some(Ok(()));
        }
        self.turn(wait, &mut |_| false);
        if let Some(error) = self.shared.rejected.take() {
            return Some(Err(error));
        }
        if self.shared.ready {
            return Some(Ok(()));
        }
        (self.connection.is_none()).then_some(Err(Error::Disconnected))
    }

    fn settle(&mut self) -> Result<(), Error> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let wait = self.wait_slice();
            if let Some(result) = self.try_settle(wait) {
                return result;
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
        }
    }

    /// Sends the closes owed for streams dropped since the last write, on this connection. Nothing
    /// waits for them: their answers are absorbed when they come. A close owed for a connection
    /// that has since ended is dropped, since the service released that stream when it ended.
    fn send_closing(&mut self) {
        if !self.shared.ready {
            return;
        }
        for close in self.shared.holds.take_closing() {
            if close.generation != self.generation {
                continue;
            }
            let id = self.shared.id();
            let frame = Frame::StreamClose {
                id,
                device: close.device,
                name: &close.name,
            };
            let Some(connection) = self.connection.as_mut() else {
                return;
            };
            if append(&frame, &mut connection.tx).is_ok() {
                self.shared.unheard.push(id);
            }
        }
    }

    fn queue(&mut self, frame: &Frame<'_>) -> Result<(), Error> {
        if !self.shared.ready {
            return Err(Error::Disconnected);
        }
        self.send_closing();
        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
        if connection.unsent() > MAX_QUEUED_BYTES {
            return Err(Error::Full);
        }
        append(frame, &mut connection.tx)?;
        Ok(())
    }

    /// One step of waiting for the answer to `id`. `Some` means finished.
    pub(crate) fn try_answer<R>(
        &mut self,
        wait: Option<Duration>,
        generation: u64,
        id: u32,
        pick: &mut impl FnMut(Frame<'_>) -> Option<Result<R, Error>>,
    ) -> Option<Result<R, Error>> {
        let mut out = None;
        self.turn(wait, &mut |frame| {
            if answers(&frame) != Some(id) {
                return false;
            }
            if let Frame::Refusal { status, text, .. } = frame {
                out = Some(Err(Error::Refused {
                    status: Status(status),
                    text: text.to_string(),
                }));
            } else if let Some(answer) = pick(frame) {
                out = Some(answer);
            }
            true
        });
        if out.is_none() && self.generation != generation {
            return Some(Err(Error::Disconnected));
        }
        out
    }

    fn wait<R>(
        &mut self,
        id: u32,
        mut pick: impl FnMut(Frame<'_>) -> Option<Result<R, Error>>,
    ) -> Result<R, Error> {
        let generation = self.generation;
        let deadline = Instant::now() + self.timeout;
        loop {
            let wait = self.wait_slice();
            if let Some(result) = self.try_answer(wait, generation, id, &mut pick) {
                return result;
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
        }
    }

    pub(crate) fn op_list(&mut self) -> Result<(u32, impl Pick<Vec<DeviceEntry>> + use<>), Error> {
        let id = self.shared.id();
        self.queue(&Frame::List { id })?;
        let mut entries = Vec::new();
        Ok((id, move |frame: Frame<'_>| match frame {
            Frame::Device {
                device,
                name,
                driver,
                tags,
                state,
                health,
                reason,
                link,
                ..
            } => {
                let (Some(state), Some(health)) = (device_state(state), device_health(health))
                else {
                    return Some(Err(Error::Wire(lemnos_wire::WireError::Malformed)));
                };
                entries.push(DeviceEntry {
                    index: device,
                    name: name.to_string(),
                    driver: driver.to_string(),
                    tags: tags
                        .split(',')
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect(),
                    state,
                    health,
                    reason: reason.to_string(),
                    link: (!link.0.is_empty()).then(|| link.0.to_string()),
                });
                None
            }
            Frame::ListDone { .. } => Some(Ok(core::mem::take(&mut entries))),
            _ => None,
        }))
    }

    pub(crate) fn op_packages(&mut self) -> Result<(u32, impl Pick<PackageList> + use<>), Error> {
        // Only a service that said so knows the question. To any other it is an unknown kind,
        // which ends the connection.
        if self.shared.features & feature::PACKAGES == 0 {
            return Err(self.shared.unusable(
                feature::PACKAGES,
                "the service does not report its driver packages",
            ));
        }
        let id = self.shared.id();
        self.queue(&Frame::Packages { id })?;
        let mut packages = Vec::new();
        Ok((id, move |frame: Frame<'_>| match frame {
            Frame::Package {
                path,
                driver,
                reason,
                ..
            } => {
                packages.push(PackageEntry {
                    path: path.to_string(),
                    driver: (!driver.is_empty()).then(|| driver.to_string()),
                    reason: reason.to_string(),
                });
                None
            }
            Frame::PackagesDone { mode, note, .. } => {
                let mode = match mode {
                    package_mode::OFF => PackageMode::Off,
                    package_mode::SIGNED => PackageMode::Signed,
                    package_mode::OPEN => PackageMode::Open,
                    _ => return Some(Err(Error::Wire(lemnos_wire::WireError::Malformed))),
                };
                Some(Ok(PackageList {
                    mode,
                    note: note.to_string(),
                    packages: core::mem::take(&mut packages),
                }))
            }
            _ => None,
        }))
    }

    pub(crate) fn op_open(
        &mut self,
        name: &str,
        tag: &str,
    ) -> Result<(u32, impl Pick<Handle> + use<>), Error> {
        let id = self.shared.id();
        self.queue(&Frame::Open {
            id,
            device: name,
            tag,
        })?;
        let (name, tag, generation) = (name.to_string(), tag.to_string(), self.generation);
        Ok((id, move |frame: Frame<'_>| match frame {
            Frame::Opened {
                device, contract, ..
            } => Some(Ok(Handle {
                name: name.clone(),
                tag: tag.clone(),
                device,
                contract,
                generation,
            })),
            _ => None,
        }))
    }

    pub(crate) fn op_describe(
        &mut self,
        device: u32,
    ) -> Result<(u32, impl Pick<DeviceDescription> + use<>), Error> {
        let id = self.shared.id();
        self.queue(&Frame::Describe { id, device })?;
        Ok((id, |frame: Frame<'_>| match frame {
            Frame::Description { bytes, .. } => Some(
                decode_description(bytes)
                    .map_err(|_| Error::Wire(lemnos_wire::WireError::Malformed)),
            ),
            _ => None,
        }))
    }

    pub(crate) fn op_call<'r>(
        &mut self,
        handle: &Handle,
        method: u16,
        args: &[u8],
        reply: &'r mut [u8],
    ) -> Result<(u32, impl Pick<usize> + use<'r>), Error> {
        if handle.generation != self.generation {
            return Err(Error::Stale);
        }
        let id = self.shared.id();
        self.queue(&Frame::Call {
            id,
            device: handle.device,
            contract: handle.contract,
            method,
            reply_capacity: reply.len() as u32,
            args,
        })?;
        Ok((id, move |frame: Frame<'_>| match frame {
            Frame::Reply { bytes, .. } => Some(copy(bytes, reply)),
            _ => None,
        }))
    }

    pub(crate) fn op_start_job(
        &mut self,
        handle: &Handle,
        method: u16,
        args: &[u8],
    ) -> Result<(u32, impl Pick<u64> + use<>), Error> {
        if handle.generation != self.generation {
            return Err(Error::Stale);
        }
        // Only a service that said so knows the request. To any other it is an unknown kind,
        // which ends the connection.
        if self.shared.features & feature::JOBS == 0 {
            return Err(self
                .shared
                .unusable(feature::JOBS, "the service does not run long jobs"));
        }
        let id = self.shared.id();
        self.queue(&Frame::CallJob {
            id,
            device: handle.device,
            contract: handle.contract,
            method,
            args,
        })?;
        Ok((id, |frame: Frame<'_>| match frame {
            Frame::JobStarted { job, .. } => Some(Ok(job)),
            _ => None,
        }))
    }

    pub(crate) fn op_cancel_job(
        &mut self,
        job: u64,
    ) -> Result<(u32, impl Pick<()> + use<>), Error> {
        let id = self.shared.id();
        self.queue(&Frame::JobCancel { id, job })?;
        Ok((id, ack))
    }

    pub(crate) fn op_subscribe(
        &mut self,
        handle: &Handle,
        method: u16,
        period_us: u64,
        only_changes: bool,
    ) -> Result<(u32, u64, impl Pick<Subscription> + use<>), Error> {
        if handle.generation != self.generation {
            return Err(Error::Stale);
        }
        let id = self.shared.id();
        self.queue(&Frame::Subscribe {
            id,
            device: handle.device,
            contract: handle.contract,
            method,
            period_us,
            only_changes,
        })?;
        self.next_local += 1;
        let local = self.next_local;
        self.shared.subs.push(Sub {
            local,
            name: handle.name.clone(),
            tag: handle.tag.clone(),
            method,
            period_us,
            only_changes,
            state: SubState::Subscribing(id),
            established: false,
        });
        Ok((id, local, move |frame: Frame<'_>| match frame {
            Frame::Subscribed { period_us, .. } => Some(Ok(Subscription {
                id: local,
                period_us,
            })),
            _ => None,
        }))
    }

    pub(crate) fn forget(&mut self, local: u64) {
        self.shared.subs.retain(|s| s.local != local);
    }

    /// Takes a subscription off the books. Returns the request to send, if it
    /// is live on this connection.
    pub(crate) fn op_unsubscribe(
        &mut self,
        local: u64,
    ) -> Result<Option<(u32, impl Pick<()> + use<>)>, Error> {
        let live = self.shared.subs.iter().find_map(|s| match s.state {
            SubState::Live(server) if s.local == local => Some(server),
            _ => None,
        });
        self.forget(local);
        let Some(subscription) = live else {
            return Ok(None);
        };
        let id = self.shared.id();
        self.queue(&Frame::Unsubscribe { id, subscription })?;
        Ok(Some((id, ack)))
    }

    pub(crate) fn op_set_power(
        &mut self,
        device: u32,
        on: bool,
    ) -> Result<(u32, impl Pick<()> + use<>), Error> {
        let id = self.shared.id();
        self.queue(&Frame::SetPower { id, device, on })?;
        Ok((id, ack))
    }

    /// Every device the service has.
    pub fn list(&mut self) -> Result<Vec<DeviceEntry>, Error> {
        self.settle()?;
        let (id, pick) = self.op_list()?;
        self.wait(id, pick)
    }

    /// What became of the service's driver packages: which were loaded, which refused and why.
    /// A service from before it could say is `Refused` with `NOT_SUPPORTED`.
    pub fn packages(&mut self) -> Result<PackageList, Error> {
        self.settle()?;
        let (id, pick) = self.op_packages()?;
        self.wait(id, pick)
    }

    /// The first device providing `tag`.
    pub fn find(&mut self, tag: &str) -> Result<Option<DeviceEntry>, Error> {
        Ok(self
            .list()?
            .into_iter()
            .find(|entry| entry.tags.iter().any(|t| t == tag)))
    }

    /// Opens one contract of the device called `name`, or of any device
    /// providing `tag` when `name` is empty.
    pub fn open(&mut self, name: &str, tag: &str) -> Result<Handle, Error> {
        self.settle()?;
        let (id, pick) = self.op_open(name, tag)?;
        self.wait(id, pick)
    }

    /// Opens contract `C` on the device called `name`, checked against `C` before the handle is
    /// returned: the same bind check as [`Client::open`](crate::Client::open). Nothing is called on
    /// a device the check refuses.
    pub fn open_typed<C: Contract + ?Sized>(&mut self, name: &str) -> Result<Handle, Error> {
        let handle = self.open(name, C::TAG.as_str())?;
        let description = self.describe(handle.device())?;
        crate::bind::verify::<C>(&description)?;
        Ok(handle)
    }

    /// What a device provides and how it presents itself.
    pub fn describe(&mut self, device: u32) -> Result<DeviceDescription, Error> {
        self.settle()?;
        let (id, pick) = self.op_describe(device)?;
        self.wait(id, pick)
    }

    /// Calls a method and waits for its answer. A handle from before a
    /// reconnect is found again by name and tag first.
    pub fn call(
        &mut self,
        handle: &mut Handle,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize, Error> {
        if !self.shared.ready {
            self.settle()?;
        }
        if handle.generation != self.generation {
            *handle = self.open(&handle.name.clone(), &handle.tag.clone())?;
        }
        let (id, pick) = self.op_call(handle, method, args, reply)?;
        self.wait(id, pick)
    }

    /// Asks for a method's result every `period_us`. The subscription is
    /// restored after any reconnect.
    pub fn subscribe(
        &mut self,
        handle: &mut Handle,
        method: u16,
        period_us: u64,
        only_changes: bool,
    ) -> Result<Subscription, Error> {
        if !self.shared.ready {
            self.settle()?;
        }
        if handle.generation != self.generation {
            *handle = self.open(&handle.name.clone(), &handle.tag.clone())?;
        }
        let (id, local, pick) = self.op_subscribe(handle, method, period_us, only_changes)?;
        let result = self.wait(id, pick);
        if result.is_err() {
            self.forget(local);
        }
        result
    }

    pub fn unsubscribe(&mut self, subscription: u64) -> Result<(), Error> {
        match self.op_unsubscribe(subscription)? {
            Some((id, pick)) => self.wait(id, pick),
            None => Ok(()),
        }
    }

    /// Turns a device on or off.
    pub fn set_power(&mut self, device: u32, on: bool) -> Result<(), Error> {
        self.settle()?;
        let (id, pick) = self.op_set_power(device, on)?;
        self.wait(id, pick)
    }

    /// Starts a long job: a method the contract marks as one. Returns the job's number at once;
    /// what the job does after comes from [`Session::next_job_event`] or
    /// [`Session::wait_job`]. The job is the service's to run and this connection's to own: it
    /// is cancelled if the connection ends, and then reported [`Status::INTERRUPTED`] here.
    pub fn start_job(
        &mut self,
        handle: &mut Handle,
        method: u16,
        args: &[u8],
    ) -> Result<u64, Error> {
        if !self.shared.ready {
            self.settle()?;
        }
        if handle.generation != self.generation {
            *handle = self.open(&handle.name.clone(), &handle.tag.clone())?;
        }
        let (id, pick) = self.op_start_job(handle, method, args)?;
        self.wait(id, pick)
    }

    /// Borrows the bulk endpoints of device `device`'s function: `[OUT, IN]`, as two descriptors
    /// the caller reads and writes itself. `platform` takes the descriptors off the socket, as it
    /// does for a stream. Until [`usb_endpoints_release`](Self::usb_endpoints_release) or the
    /// connection ends, no other connection can be given them. See `docs/wire.md` (Endpoints).
    ///
    /// # Errors
    ///
    /// `Refused` with `BUSY` while another connection holds them, `NO_DEVICE` while the function
    /// is not enabled, `NOT_SUPPORTED` for a service that can't lend them.
    pub fn usb_endpoints(
        &mut self,
        device: u32,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<UsbEndpoints, Error> {
        self.settle()?;
        if self.shared.features & feature::ENDPOINTS == 0 {
            return Err(self
                .shared
                .unusable(feature::ENDPOINTS, "the service does not lend endpoints"));
        }
        let id = self.shared.id();
        self.queue(&Frame::EndpointsOpen { id, device })?;
        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
        connection.taker = Some(Taker {
            platform: Arc::clone(&platform),
            fds: Vec::new(),
        });
        let ready = self.wait(id, |frame| match frame {
            Frame::Endpoints { .. } => Some(Ok(())),
            _ => None,
        });
        let taker = self.connection.as_mut().and_then(|c| c.taker.take());
        match ready {
            Ok(()) => {
                let mut fds = taker.ok_or(Error::Disconnected)?.fds;
                // Every descriptor came with the answer, or the connection is out of step.
                if fds.len() != 2 {
                    self.lost();
                    return Err(Error::Wire(lemnos_wire::WireError::Malformed));
                }
                let input = fds.pop().ok_or(Error::Disconnected)?;
                let out = fds.pop().ok_or(Error::Disconnected)?;
                Ok(UsbEndpoints { out, input })
            }
            Err(Error::TimedOut) => {
                // A late Endpoints would carry descriptors that a plain read loses.
                self.lost();
                Err(Error::TimedOut)
            }
            Err(error) => Err(error),
        }
    }

    /// Gives up the endpoints this session holds on device `device`. The descriptors the caller
    /// holds keep working on the function's files only until the function is taken down; close
    /// them too.
    ///
    /// # Errors
    ///
    /// `Refused` with `NOT_FOUND` when this session holds none.
    pub fn usb_endpoints_release(&mut self, device: u32) -> Result<(), Error> {
        self.settle()?;
        if self.shared.features & feature::ENDPOINTS == 0 {
            return Err(self
                .shared
                .unusable(feature::ENDPOINTS, "the service does not lend endpoints"));
        }
        let id = self.shared.id();
        self.queue(&Frame::EndpointsClose { id, device })?;
        self.wait(id, |frame| match frame {
            Frame::Reply { .. } => Some(Ok(())),
            _ => None,
        })
    }

    /// Asks the service to stop a job this session started. Its [`JobEvent::Done`] follows.
    pub fn cancel_job(&mut self, job: u64) -> Result<(), Error> {
        self.settle()?;
        let (id, pick) = self.op_cancel_job(job)?;
        self.wait(id, pick)
    }

    /// Takes job `job`'s queued events: its progress goes to `progress`, and its end is
    /// returned. Other jobs' events stay queued.
    fn take_job(
        &mut self,
        job: u64,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Option<(Status, Vec<u8>)> {
        while let Some(at) = self
            .shared
            .job_events
            .iter()
            .position(|event| event.job() == job)
        {
            match self.shared.job_events.remove(at) {
                Some(JobEvent::Progress { done, total, .. }) => progress(done, total),
                Some(JobEvent::Done { status, result, .. }) => return Some((status, result)),
                None => break,
            }
        }
        None
    }

    /// One step of waiting for job `job`'s end. `Some` means it ended.
    pub(crate) fn try_job(
        &mut self,
        wait: Option<Duration>,
        job: u64,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Option<(Status, Vec<u8>)> {
        if let Some(end) = self.take_job(job, progress) {
            return Some(end);
        }
        if !self.shared.jobs.contains(&job) {
            // Neither open nor ended here: a number this connection never had, or one whose
            // end was already taken.
            return Some((Status::NOT_FOUND, Vec::new()));
        }
        self.turn(wait, &mut |_| false);
        self.take_job(job, progress)
    }

    /// Waits until job `job` is over, giving its progress to `progress` as it comes, and
    /// returns how it ended and its result. `None` after `timeout`, with the job still running.
    pub fn wait_job(
        &mut self,
        job: u64,
        timeout: Duration,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Option<(Status, Vec<u8>)> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let wait = match self.mode {
                Mode::Blocking => Some(left.max(Duration::from_millis(1))),
                Mode::Polled => self.wait_slice(),
            };
            if let Some(end) = self.try_job(wait, job, progress) {
                return Some(end);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// One step of waiting for a value. `Some` means finished.
    pub(crate) fn try_value(
        &mut self,
        wait: Option<Duration>,
        into: &mut [u8],
    ) -> Option<Result<ValueInfo, Error>> {
        if let Some(value) = self.pop_value(into) {
            return Some(value);
        }
        self.turn(wait, &mut |_| false);
        self.pop_value(into)
    }

    fn pop_value(&mut self, into: &mut [u8]) -> Option<Result<ValueInfo, Error>> {
        self.shared.values.pop(|subscription, stamp_ns, bytes| {
            copy(bytes, into).map(|len| ValueInfo {
                subscription,
                stamp_ns,
                len,
            })
        })
    }

    /// The next value from any subscription, or `None` after `timeout`. A
    /// value too large for `into` is lost and reported as [`Error::TooSmall`].
    pub fn next_value(
        &mut self,
        into: &mut [u8],
        timeout: Duration,
    ) -> Result<Option<ValueInfo>, Error> {
        let deadline = Instant::now() + timeout;
        loop {
            let wait = match self.mode {
                Mode::Blocking => Some(timeout),
                Mode::Polled => self.wait_slice(),
            };
            if let Some(value) = self.try_value(wait, into) {
                return value.map(Some);
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    /// Sends a call without waiting. The answer comes from [`Session::poll`]
    /// with the returned id.
    pub fn send_call(
        &mut self,
        handle: &Handle,
        method: u16,
        args: &[u8],
        reply_capacity: u32,
    ) -> Result<u32, Error> {
        if handle.generation != self.generation {
            return Err(Error::Stale);
        }
        let id = self.shared.id();
        self.queue(&Frame::Call {
            id,
            device: handle.device,
            contract: handle.contract,
            method,
            reply_capacity,
            args,
        })?;
        if let Some(connection) = self.connection.as_mut()
            && connection.flush().is_err()
        {
            self.lost();
            return Err(Error::Disconnected);
        }
        Ok(id)
    }

    /// Starts a long job without waiting: [`Session::poll`] gives [`Incoming::JobStarted`] or
    /// [`Incoming::Refused`] with the id returned here, then the job's [`Incoming::Job`] events.
    pub fn send_job(&mut self, handle: &Handle, method: u16, args: &[u8]) -> Result<u32, Error> {
        let (id, _) = self.op_start_job(handle, method, args)?;
        if let Some(connection) = self.connection.as_mut()
            && connection.flush().is_err()
        {
            self.lost();
            return Err(Error::Disconnected);
        }
        Ok(id)
    }

    /// Takes a device's frame stream. `name` is the stream's name under that device. A stream
    /// has one consumer, and it isn't restored after a reconnect: take it again.
    ///
    /// A stream whose buffers are dma-bufs is taken the same way and reads the same way. It
    /// needs a service that offers such streams and a `platform` that can map them
    /// ([`StreamPlatform::map_buffer`]); without the second this is
    /// [`Error::StreamUnsupported`], and the stream is given back.
    ///
    /// Waiting for the answer reads the socket with `platform`, so nothing may be
    /// read from this session at the same time, which a `Session` rules out by being owned.
    pub fn open_stream(
        &mut self,
        device: u32,
        name: &str,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.open_stream_as(device, name, false, platform)
    }

    /// Takes a stream this consumer fills as well as reads, as [`open_stream`](Self::open_stream)
    /// takes one it only reads. Its buffers are mapped writable, so
    /// [`FrameStream::write`] works: on a stream of shared memory and on a stream of dma-bufs
    /// alike.
    ///
    /// Which streams those are is the device's contract's to say, and the consumer knows it
    /// from the method that named the stream:
    ///
    /// - the job stream of `image.convert/1`, `image.scaler/1`, `image.blend/1` and `npu/1`:
    ///   the consumer writes a job's input images, or a program and its tensors;
    /// - the config stream and the input stream of `camera.isp/1`: the consumer writes each
    ///   job's configuration, and the raw image where the ISP takes one from memory.
    ///
    /// Every other stream is filled by its producer alone (`camera.receiver/1` and
    /// `camera.frames/1` frames, `camera.isp/1` statistics and outputs) and is taken with
    /// [`open_stream`](Self::open_stream). Nothing is asked of the service that
    /// `open_stream` doesn't ask: the memory is this consumer's and the producer's alone, and
    /// the producer takes nothing in it on trust either way.
    pub fn open_stream_writable(
        &mut self,
        device: u32,
        name: &str,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.open_stream_as(device, name, true, platform)
    }

    fn open_stream_as(
        &mut self,
        device: u32,
        name: &str,
        writable: bool,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        self.settle()?;
        let (id, pick) = self.op_open_stream(device, name, &platform)?;
        let ready = self.wait(id, pick);
        self.finish_open_stream(ready, device, name, writable, platform)
    }

    /// Asks for a stream and starts reading the socket so its descriptors are kept.
    pub(crate) fn op_open_stream(
        &mut self,
        device: u32,
        name: &str,
        platform: &Arc<dyn StreamPlatform>,
    ) -> Result<(u32, impl Pick<StreamAnswer> + use<>), Error> {
        let id = self.shared.id();
        // A service without shared rings answers with the older layout, which this build does not
        // read: it is refused here, before anything is asked.
        if self.shared.features & feature::SHARED_RINGS == 0 {
            return Err(self.shared.unusable(
                feature::SHARED_RINGS,
                "the service's streams are not shared rings",
            ));
        }
        // Only a service that said so knows the second way of asking. To any other it is an
        // unknown kind, which ends the connection.
        let asked_handles = self.shared.features & feature::STREAM_HANDLES != 0;
        if asked_handles {
            self.queue(&Frame::StreamOpenHandles { id, device, name })?;
        } else {
            self.queue(&Frame::StreamOpen { id, device, name })?;
        }
        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
        connection.taker = Some(Taker {
            platform: Arc::clone(platform),
            fds: Vec::new(),
        });
        Ok((id, move |frame: Frame<'_>| {
            let (
                handles,
                buffers,
                descriptors,
                buffer_bytes,
                frame_slots,
                job_slots,
                job_bytes,
                working,
                job_driven,
            ) = match frame {
                Frame::StreamRings {
                    buffers,
                    descriptors,
                    buffer_bytes,
                    frame_slots,
                    job_slots,
                    job_bytes,
                    working: Tail(working),
                    kind: Tail(kind),
                    ..
                } => (
                    None,
                    buffers,
                    descriptors,
                    buffer_bytes,
                    frame_slots,
                    job_slots,
                    job_bytes,
                    working,
                    kind == 1,
                ),
                Frame::StreamRingHandles {
                    buffers,
                    descriptors,
                    buffer_bytes,
                    frame_slots,
                    job_slots,
                    job_bytes,
                    plane_bytes,
                    working: Tail(working),
                    kind: Tail(kind),
                    ..
                } => (
                    Some(Planes::decode(plane_bytes, buffer_bytes)),
                    buffers,
                    descriptors,
                    buffer_bytes,
                    frame_slots,
                    job_slots,
                    job_bytes,
                    working,
                    kind == 1,
                ),
                _ => return None,
            };
            Some(Ok(StreamAnswer {
                layout: Layout {
                    buffers,
                    buffer_bytes,
                    frame_slots,
                    job_slots,
                    job_bytes,
                },
                descriptors,
                handles: handles.is_some(),
                planes: handles.unwrap_or(Some(Planes::one(buffer_bytes))),
                asked_handles,
                working,
                job_driven,
            }))
        }))
    }

    /// Takes the descriptors that came with `ready` and builds the stream. On every path that
    /// fails, the descriptors close here.
    pub(crate) fn finish_open_stream(
        &mut self,
        ready: Result<StreamAnswer, Error>,
        device: u32,
        name: &str,
        writable: bool,
        platform: Arc<dyn StreamPlatform>,
    ) -> Result<FrameStream, Error> {
        let taker = self.connection.as_mut().and_then(|c| c.taker.take());
        let answer = match ready {
            Ok(ready) => ready,
            Err(Error::TimedOut) => {
                // A late StreamRings would carry descriptors that a plain read
                // loses, and a stream nobody can use. End it.
                self.lost();
                return Err(Error::TimedOut);
            }
            Err(error) => return Err(error),
        };
        let fds = taker.ok_or(Error::Disconnected)?.fds;
        let StreamAnswer {
            layout,
            descriptors,
            handles,
            planes,
            asked_handles,
            working,
            job_driven,
        } = answer;
        // Every number here came off the socket. For a descriptor per buffer or plane, the
        // planes must add up to the buffer, the count must be the one the buffers and planes
        // give, no more than one message carries, and the answer one this client asked for.
        let consistent = match planes {
            Some(planes) => {
                !handles
                    || (asked_handles
                        && handle_descriptors(layout.buffers, planes.count()) == Some(descriptors))
            }
            None => false,
        };
        let planes = planes.unwrap_or(Planes::one(layout.buffer_bytes));
        if !consistent || usize::from(descriptors) != fds.len() {
            // `fds` drops, closing whatever did arrive.
            self.lost();
            return Err(Error::Wire(lemnos_wire::WireError::Malformed));
        }
        let built = FrameStream::new(
            device,
            name,
            (layout, planes, working, job_driven),
            !handles,
            fds,
            writable,
            platform,
        );
        if built.is_err() {
            self.give_back(device, name);
        }
        built.map(|stream| {
            let token = self.shared.holds.register(self.generation, device, name);
            stream.with_hold(Hold::new(Arc::clone(&self.shared.holds), token))
        })
    }

    /// Gives back a stream the service handed over and this side couldn't set up, so it isn't
    /// held, unusable, until the connection ends. Nobody waits for the answer.
    fn give_back(&mut self, device: u32, name: &str) {
        let id = self.shared.id();
        if self.queue(&Frame::StreamClose { id, device, name }).is_ok() {
            self.shared.unheard.push(id);
            if let Some(connection) = self.connection.as_mut() {
                let _ = connection.flush();
            }
        }
    }

    pub(crate) fn op_close_stream(
        &mut self,
        stream: &FrameStream,
    ) -> Result<(u32, impl Pick<()> + use<>), Error> {
        let id = self.shared.id();
        self.queue(&Frame::StreamClose {
            id,
            device: stream.device(),
            name: stream.name(),
        })?;
        Ok((id, ack))
    }

    /// Gives a stream back. The host can then give it to someone else. Dropping a [`FrameStream`]
    /// without this does the same: its close is sent with this session's next write, and nothing
    /// waits for it.
    pub fn close_stream(&mut self, stream: FrameStream) -> Result<(), Error> {
        // Released first: the close is sent below, and no second one is owed when this drops.
        stream.release_hold();
        self.settle()?;
        let (id, pick) = self.op_close_stream(&stream)?;
        self.wait(id, pick)
    }

    /// Delivers whatever has arrived, without waiting. Returns how much
    /// happened, 0 when there was nothing to do.
    pub fn poll(&mut self, sink: &mut dyn FnMut(Incoming<'_>)) -> usize {
        while self
            .shared
            .values
            .pop(|subscription, stamp_ns, bytes| {
                sink(Incoming::Value {
                    subscription,
                    stamp_ns,
                    bytes,
                });
            })
            .is_some()
        {}
        // Values need the local id, which the turn's handler can't look up
        // while the session is borrowed, so they take the queue on the way.
        let moved = self.turn(None, &mut |frame| match frame {
            Frame::Reply { id, bytes } => {
                sink(Incoming::Reply { id, bytes });
                true
            }
            Frame::JobStarted { id, job } => {
                sink(Incoming::JobStarted { id, job });
                true
            }
            Frame::Refusal { id, status, text } => {
                sink(Incoming::Refused {
                    id,
                    status: Status(status),
                    text,
                });
                true
            }
            _ => false,
        });
        while let Some(event) = self.shared.job_events.pop_front() {
            sink(Incoming::Job(&event));
        }
        while self
            .shared
            .values
            .pop(|subscription, stamp_ns, bytes| {
                sink(Incoming::Value {
                    subscription,
                    stamp_ns,
                    bytes,
                });
            })
            .is_some()
        {}
        moved
    }
}

fn local_id(subs: &[Sub], server: u64) -> Option<u64> {
    subs.iter()
        .find(|s| s.state == SubState::Live(server))
        .map(|s| s.local)
}

fn copy(bytes: &[u8], into: &mut [u8]) -> Result<usize, Error> {
    let to = into.get_mut(..bytes.len()).ok_or(Error::TooSmall)?;
    to.copy_from_slice(bytes);
    Ok(bytes.len())
}

fn ack(frame: Frame<'_>) -> Option<Result<(), Error>> {
    matches!(frame, Frame::Reply { .. }).then_some(Ok(()))
}
