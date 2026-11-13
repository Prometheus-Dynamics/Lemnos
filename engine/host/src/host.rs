use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use lemnos_core::{
    AddressWidth, BoardDevice, BoardView, CallerId, ClocksHandle, Config, ConfigValue, Decode,
    Description, DmaHandle, Driver, Encode, Health, Hint, HostClock, HostOwned, I2c,
    InterruptHandle, ItemKind, JobId, JobState, Line, LocalCaller, Manifest, MethodId, NeedKind,
    Origin, Provider, Pwm, RawSpiBus, Reader, RegistersHandle, Reply, ResetHandle, Resources,
    Result, Router, Spi, Status, StreamHandle, Tag, Uart, UsbDeviceHandle, UsbFunctionHandle,
};

use crate::board::{BoardError, BoardFile, DeviceSpec, EachSpec, OwnedValue, Owner};
use crate::contain::{Lane, LaneError, Work};
use crate::stream::{
    HeapAllocator, Lease, ProducerEndpoint, StreamAllocator, StreamMemory, StreamStats, Streams,
};
use crate::{Options, guest};

/// A bound driver, as the host runs it: [`Driver`] without its type. Every `#[driver]` type is
/// one. A driver that reaches the host some other way, such as one loaded from a package,
/// implements this and is run exactly as a built-in driver is.
pub trait Instance: Send {
    fn describe(&self, out: &mut Description);
    fn health(&self) -> Health;
    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status;
    fn tick(&mut self, now_ns: u64) -> Option<u64>;
    /// See [`Driver::board`]. Asked only of a driver whose entry says it watches the board.
    fn board(&mut self, _view: &dyn BoardView) {}
    fn caller(&mut self, caller: CallerId);
    fn caller_gone(&mut self, caller: CallerId);
    /// See [`Driver::job`]. Asked only of a driver whose entry says it runs jobs.
    fn job(&mut self, _job: JobId, _result: &mut Reply<'_>) -> JobState {
        JobState::Finished(Status::NOT_SUPPORTED)
    }
    /// See [`Driver::job_cancel`].
    fn job_cancel(&mut self, _job: JobId) {}
}

impl<D: Driver + Send> Instance for D {
    fn describe(&self, out: &mut Description) {
        Driver::describe(self, out);
    }

    fn health(&self) -> Health {
        Driver::health(self)
    }

    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status {
        Driver::dispatch(self, method, args, reply)
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        Driver::tick(self, now_ns)
    }

    fn board(&mut self, view: &dyn BoardView) {
        Driver::board(self, view);
    }

    fn caller(&mut self, caller: CallerId) {
        Driver::caller(self, caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        Driver::caller_gone(self, caller);
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        Driver::job(self, job, result)
    }

    fn job_cancel(&mut self, job: JobId) {
        Driver::job_cancel(self, job);
    }
}

/// Builds a driver whose type the host was not compiled with. See [`DriverEntry::bound_by`].
pub trait Binder: Send + Sync {
    /// What [`Driver::bind`] is to a built-in driver: called on the device's lane, inside the
    /// panic boundary, on every bind and every retry.
    fn bind(&self, res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Box<dyn Instance>>;
}

#[derive(Clone, Copy)]
enum Factory {
    BuiltIn(fn(&mut Resources<'_>, &Config<'_>) -> Result<Box<dyn Instance>>),
    Bound(&'static dyn Binder),
}

impl Factory {
    fn bind(&self, res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Box<dyn Instance>> {
        match self {
            Self::BuiltIn(make) => make(res, cfg),
            Self::Bound(binder) => binder.bind(res, cfg),
        }
    }
}

/// A driver the host can start.
#[derive(Clone, Copy)]
pub struct DriverEntry {
    manifest: Manifest,
    factory: Factory,
    ticks: bool,
    /// [`Driver::WATCHES_BOARD`]: the host gives the driver the board before each tick.
    watches: bool,
    tracks_callers: bool,
    jobs: bool,
    host_owned: HostOwned,
    i2c_register_address: Option<AddressWidth>,
}

impl DriverEntry {
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The entry for a driver that `binder` builds. `ticks` and `tracks_callers` are its
    /// [`Driver::TICKS`] and [`Driver::TRACKS_CALLERS`]. The host treats it as it treats an
    /// [`entry`]: the same lane, panic boundary, deadline, backoff and rebind.
    pub const fn bound_by(
        manifest: Manifest,
        ticks: bool,
        tracks_callers: bool,
        binder: &'static dyn Binder,
    ) -> Self {
        Self {
            manifest,
            factory: Factory::Bound(binder),
            ticks,
            // A driver built elsewhere sees no board: it gets the empty view it always had.
            watches: false,
            tracks_callers,
            jobs: false,
            // Nothing says what a driver built elsewhere does to hardware, so it is never
            // bound to a host-owned device.
            host_owned: HostOwned::Refused,
            i2c_register_address: None,
        }
    }

    /// The same entry, for a driver that runs long jobs ([`Driver::JOBS`]): the host asks it
    /// about them with [`Instance::job`].
    #[must_use]
    pub const fn with_jobs(mut self, jobs: bool) -> Self {
        self.jobs = jobs;
        self
    }
}

/// The entry for a driver type.
pub const fn entry<D: Driver + Send + 'static>() -> DriverEntry {
    fn make<D: Driver + Send + 'static>(
        res: &mut Resources<'_>,
        cfg: &Config<'_>,
    ) -> Result<Box<dyn Instance>> {
        Ok(Box::new(D::bind(res, cfg)?))
    }
    DriverEntry {
        manifest: D::MANIFEST,
        factory: Factory::BuiltIn(make::<D>),
        ticks: D::TICKS,
        watches: D::WATCHES_BOARD,
        tracks_callers: D::TRACKS_CALLERS,
        jobs: D::JOBS,
        host_owned: D::HOST_OWNED,
        i2c_register_address: D::I2C_REGISTER_ADDRESS,
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HostError {
    #[error(transparent)]
    Board(#[from] BoardError),
    #[error("devices use each other in a circle: {0}")]
    Cycle(String),
    #[error("no device {0:?}")]
    NoDevice(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffReason {
    /// `disabled = true` in the board file.
    Disabled,
    /// Someone switched it off.
    Manual,
    /// The board's explicit auto-off policy.
    Policy,
}

/// Where a device stands. Every state is visible to anyone who asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Running,
    /// Waiting to retry. Clears itself when a retry works.
    Backoff {
        attempts: u32,
        next_retry_ns: u64,
        reason: String,
    },
    Off(OffReason),
}

/// A device as seen from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub index: u32,
    pub id: String,
    pub driver: String,
    pub tags: Vec<Tag>,
    pub state: DeviceState,
    pub health: Health,
    /// The device this one is wired to, by the id the board file's `link` names. The host
    /// hands it on as written and never reads it: what the wiring means is the devices'.
    pub link: Option<String>,
    /// Goes up with every change to this device's state, after the change, so a reader that
    /// saw an earlier revision knows what it holds is out of date. It never goes back. Health is
    /// not part of it: see [`Host::look`], which is how a change of health is found.
    pub revision: u64,
}

struct Slot {
    index: u32,
    /// Which lane runs this device: its bus, or itself when it has none.
    lane_key: String,
    spec: DeviceSpec,
    config: Vec<(String, OwnedValue)>,
    /// The board file's arrays of strings, by key.
    lists: Vec<(String, Vec<String>)>,
    entry: Option<DriverEntry>,
    instance: Mutex<Option<Box<dyn Instance>>>,
    state: Mutex<DeviceState>,
    /// Goes up after each change to `state`. See [`DeviceInfo::revision`].
    revision: AtomicU64,
    /// A call into this device never came back.
    hung: AtomicBool,
    errors: AtomicU32,
    /// Optional dependencies that were absent at the last bind.
    missing: Mutex<Vec<String>>,
    /// When the driver next wants a tick. [`NO_TICK`] when it doesn't.
    next_tick_ns: AtomicU64,
    /// When a bind was last tried. A hint never brings on another sooner than
    /// [`Options::hint_gap`] after it.
    tried_ns: AtomicU64,
    /// When the driver last bound.
    bound_ns: AtomicU64,
    /// Runs in a row that ended sooner than the retry cap after their bind. A driver that fails
    /// straight after each bind waits longer each time, as a bind that fails does.
    short_runs: AtomicU32,
    /// A hint has named this running device and its driver has not been ticked since.
    hinted: AtomicBool,
    /// The `[[each]]` entry that made this device, by its place in the board file, and the name
    /// of the resource it was made for. `None` for a device of the board file's own.
    made: Option<(usize, String)>,
    /// Long jobs of this device's driver that are still open.
    open_jobs: AtomicU32,
}

const NO_TICK: u64 = u64::MAX;

/// How long a keep-alive device waits before it is asked again while the daemon is not healthy.
const GATED_POLL_NS: u64 = 100_000_000;

/// The daemon's own liveness: whether it is healthy now. Keep-alive devices are ticked only while
/// it is true, so a daemon that is not healthy stops feeding them.
pub trait Liveness: Send + Sync {
    /// Whether the daemon is healthy now.
    fn alive(&self) -> bool;
}

fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Slot {
    /// A device as it starts: waiting for its first bind, or off when the board says so.
    fn new(
        index: u32,
        spec: DeviceSpec,
        entry: Option<DriverEntry>,
        made: Option<(usize, String)>,
    ) -> core::result::Result<Self, BoardError> {
        let state = if spec.disabled {
            DeviceState::Off(OffReason::Disabled)
        } else {
            DeviceState::Backoff {
                attempts: 0,
                next_retry_ns: 0,
                reason: "starting".to_string(),
            }
        };
        Ok(Self {
            index,
            lane_key: match &spec.bus {
                Some(bus) => format!("bus:{bus}"),
                None => format!("device:{}", spec.id),
            },
            config: spec.owned_config()?,
            lists: spec.owned_lists()?,
            entry,
            spec,
            instance: Mutex::new(None),
            state: Mutex::new(state),
            revision: AtomicU64::new(0),
            hung: AtomicBool::new(false),
            errors: AtomicU32::new(0),
            missing: Mutex::new(Vec::new()),
            next_tick_ns: AtomicU64::new(NO_TICK),
            tried_ns: AtomicU64::new(0),
            bound_ns: AtomicU64::new(0),
            short_runs: AtomicU32::new(0),
            hinted: AtomicBool::new(false),
            made,
            open_jobs: AtomicU32::new(0),
        })
    }

    fn is_running(&self) -> bool {
        matches!(*lock(&self.state), DeviceState::Running)
    }

    fn ticks(&self) -> bool {
        self.entry.is_some_and(|entry| entry.ticks)
    }

    fn tracks_callers(&self) -> bool {
        self.entry.is_some_and(|entry| entry.tracks_callers)
    }

    fn runs_jobs(&self) -> bool {
        self.entry.is_some_and(|entry| entry.jobs)
    }

    /// Whether `method` starts a long job, by the contract's own word. `None` for a method
    /// the driver's contracts don't have.
    fn starts_job(&self, method: MethodId) -> Option<bool> {
        let contract = self
            .entry?
            .manifest
            .provides
            .get(usize::from(method.contract))?;
        contract
            .methods
            .iter()
            .find(|m| m.id == method.method)
            .map(|m| m.job)
    }

    /// Whether the board entry names `name` as its bus or as something it uses.
    fn names(&self, name: &str) -> bool {
        self.spec.bus.as_deref() == Some(name) || self.spec.uses.values().any(|used| used == name)
    }

    fn contract_index(&self, tag: Tag) -> Option<u8> {
        self.entry?
            .manifest
            .provides
            .iter()
            .position(|contract| contract.tag == tag)
            .map(|position| position as u8)
    }
}

/// An `[[each]]` entry of the board file, and when to look for what fits it.
struct Rule {
    spec: EachSpec,
    /// A hint named the pattern, or couldn't say what changed, since the last look.
    hinted: AtomicBool,
    /// When to look again without a hint.
    next_look_ns: AtomicU64,
    /// When the last look was. A hint never brings on another sooner than
    /// [`Options::hint_gap`] after it.
    looked_ns: AtomicU64,
}

struct Inner {
    /// The board file's `[board] id`, given to every driver as it binds.
    board: String,
    /// The daemon's package version, given to every driver as it binds.
    daemon_version: &'static str,
    /// Devices are only ever added, so a device's index stays its own.
    slots: RwLock<Vec<Arc<Slot>>>,
    rules: Vec<Rule>,
    lanes: Mutex<HashMap<String, Arc<Lane>>>,
    /// Goes up whenever any device changes state.
    changes: AtomicU64,
    backend: Mutex<Box<dyn Provider + Send>>,
    clock: Arc<dyn HostClock>,
    options: Options,
    drivers: BTreeMap<&'static str, DriverEntry>,
    /// Called when a device's next tick moves earlier.
    waker: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// The daemon's liveness, which gates the ticks of keep-alive devices. Unset, the gate is
    /// closed: nothing is kept alive until the daemon says it is healthy.
    liveness: OnceLock<Arc<dyn Liveness>>,
    streams: Streams,
    /// How a host-owned device's driver calls the devices it uses: reads only.
    reading: OnceLock<&'static Reading>,
    /// Long jobs still open, and what their starters haven't been told yet.
    jobs: Mutex<Jobs>,
}

/// The most long jobs open at once, across every device.
pub const MAX_JOBS: usize = 64;
/// The largest result a long job can finish with.
pub const JOB_RESULT_MAX: usize = 4096;

/// What changed about a long job, for whoever started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// `done` of `total` (0 when not known), in the contract's unit.
    Progress { done: u64, total: u64 },
    /// The job is over. `result` is what the contract says it finishes with, when `status` is
    /// success; empty otherwise. A job its driver lost (a panic, a rebind, the device switched
    /// off) finishes with [`Status::INTERRUPTED`].
    Finished { status: Status, result: Vec<u8> },
}

/// One [`JobEvent`], with whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobUpdate {
    /// The host's number for the job, as [`Host::start_job`] returned it.
    pub job: u64,
    pub device: u32,
    /// Who started it.
    pub owner: CallerId,
    pub event: JobEvent,
}

struct OpenJob {
    id: u64,
    device: u32,
    driver_job: JobId,
    owner: CallerId,
    /// The last progress passed on, if any was.
    told: Option<(u64, u64)>,
}

struct Jobs {
    /// The high half of every job number this host gives out: different for each host, so a
    /// number from before a restart is told from one never given.
    epoch: u32,
    next: u32,
    open: Vec<OpenJob>,
    updates: VecDeque<JobUpdate>,
}

impl Jobs {
    fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // Mixed with the process id and a count of hosts in this process, so two hosts started
        // at the same moment differ too.
        static HOSTS: AtomicU32 = AtomicU32::new(0);
        let mixed = (now.as_secs() as u32)
            .rotate_left(13)
            .wrapping_add(now.subsec_nanos())
            ^ std::process::id().rotate_left(7)
            ^ HOSTS
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_mul(0x9e37_79b9);
        Self {
            epoch: mixed.max(1),
            next: 0,
            open: Vec::new(),
            updates: VecDeque::new(),
        }
    }

    /// Queues an update. A job's progress that hasn't gone out yet is replaced by newer
    /// progress, so the queue holds at most two updates per job.
    fn push(&mut self, update: JobUpdate) {
        if let JobEvent::Progress { .. } = update.event
            && let Some(queued) = self
                .updates
                .iter_mut()
                .rev()
                .find(|queued| queued.job == update.job)
            && matches!(queued.event, JobEvent::Progress { .. })
        {
            queued.event = update.event;
            return;
        }
        self.updates.push_back(update);
    }
}

/// The way from a host-owned device's driver to another device. Only a method the other
/// device's description names as a value gets through, whoever owns that device: a driver that
/// may change nothing changes nothing through another driver either.
struct Reading(&'static Inner);

impl Router for Reading {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let slot = self.0.slot(device).ok_or(Status::NOT_FOUND)?;
        self.0
            .call_from(&slot, CallerId::LOCAL, true, false, method, args, reply)
    }

    fn health(&self, device: u32) -> Health {
        Router::health(self.0, device)
    }
}

/// Runs a board's drivers. Each driver call goes through a lane, with a panic
/// boundary and a deadline around it.
pub struct Host {
    inner: &'static Inner,
}

/// One driver call, as handed to its lane.
pub(crate) struct CallWork {
    inner: &'static Inner,
    slot: Arc<Slot>,
    method: MethodId,
    caller: CallerId,
    as_job: bool,
}

impl CallWork {
    pub(crate) fn run(self, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        self.inner.call_here(
            &self.slot,
            self.caller,
            self.as_job,
            self.method,
            args,
            reply,
        )
    }
}

/// One tick, as handed to its lane.
pub(crate) struct TickWork {
    inner: &'static Inner,
    slot: Arc<Slot>,
}

impl TickWork {
    pub(crate) fn run(self) -> Result<usize> {
        self.inner.tick_here(&self.slot);
        Ok(0)
    }
}

/// What a call that would change a host-owned device gets.
const NOT_OURS: Status = Status::NOT_PERMITTED;

/// Whether `method` only reads, by the device's own description: an item names it as a value and
/// none as a control or an action. A method the description doesn't name, or a description that
/// overflowed, isn't known to only read. Allocates nothing.
fn only_reads(instance: &dyn Instance, method: MethodId) -> bool {
    let mut description = Description::new();
    instance.describe(&mut description);
    if description.dropped() > 0 {
        return false;
    }
    let mut named = false;
    for item in description.items().filter(|item| item.method == method) {
        if item.kind != ItemKind::Value {
            return false;
        }
        named = true;
    }
    named
}

fn counts_as_gone(status: Status) -> bool {
    status == Status::IO || status == Status::NO_DEVICE || status == Status::TIMED_OUT
}

/// Why the driver wants to be bound again, if it says what it was bound with is dead.
fn asks_rebind(instance: &dyn Instance) -> Option<&'static str> {
    match instance.health() {
        Health::Gone(reason) => Some(reason),
        _ => None,
    }
}

impl Inner {
    /// Records that a device's state has just been changed: its revision and the host's count go
    /// up. Called after the change is written.
    fn moved(&self, slot: &Slot) {
        slot.revision.fetch_add(1, Ordering::Release);
        self.changes.fetch_add(1, Ordering::Release);
    }

    fn jobs(&self) -> MutexGuard<'_, Jobs> {
        lock(&self.jobs)
    }

    /// Ends every open job of `device` with `status`: its driver has gone.
    fn end_jobs(&self, slot: &Slot, status: Status) {
        if slot.open_jobs.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut jobs = self.jobs();
        let mut index = 0;
        while index < jobs.open.len() {
            if jobs.open[index].device != slot.index {
                index += 1;
                continue;
            }
            // In order: another lane may be going through its own jobs by position.
            let job = jobs.open.remove(index);
            slot.open_jobs.fetch_sub(1, Ordering::AcqRel);
            jobs.push(JobUpdate {
                job: job.id,
                device: job.device,
                owner: job.owner,
                event: JobEvent::Finished {
                    status,
                    result: Vec::new(),
                },
            });
        }
        drop(jobs);
        self.wake();
    }

    /// Takes a job `instance` has just started on `caller`'s behalf, and returns its number.
    fn open_job(&self, slot: &Slot, driver_job: JobId, caller: CallerId) -> u64 {
        let mut jobs = self.jobs();
        jobs.next = jobs.next.wrapping_add(1).max(1);
        let id = (u64::from(jobs.epoch) << 32) | u64::from(jobs.next);
        jobs.open.push(OpenJob {
            id,
            device: slot.index,
            driver_job,
            owner: caller,
            told: None,
        });
        slot.open_jobs.fetch_add(1, Ordering::AcqRel);
        id
    }

    /// Asks a driver how each of its open jobs stands, and queues what changed for their
    /// starters. On the driver's lane, with its instance held.
    fn poll_jobs(&self, slot: &Slot, instance: &mut dyn Instance) {
        if !slot.runs_jobs() || slot.open_jobs.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut result = [0u8; JOB_RESULT_MAX];
        let mut changed = false;
        // This device's jobs change only on its lane, which is here, so going through them by
        // position sees each once.
        let mut position = 0;
        loop {
            let next = self
                .jobs()
                .open
                .iter()
                .filter(|job| job.device == slot.index)
                .nth(position)
                .map(|job| (job.id, job.driver_job));
            let Some((id, driver_job)) = next else {
                break;
            };
            let mut writer = Reply::new(&mut result);
            let state = instance.job(driver_job, &mut writer);
            let mut jobs = self.jobs();
            let Some(at) = jobs.open.iter().position(|job| job.id == id) else {
                continue;
            };
            match state {
                JobState::Running { done, total } => {
                    position += 1;
                    let job = &mut jobs.open[at];
                    if job.told == Some((done, total)) {
                        continue;
                    }
                    job.told = Some((done, total));
                    let (device, owner) = (job.device, job.owner);
                    jobs.push(JobUpdate {
                        job: id,
                        device,
                        owner,
                        event: JobEvent::Progress { done, total },
                    });
                }
                JobState::Finished(status) => {
                    // In order: another lane may be going through its own jobs by position.
                    let job = jobs.open.remove(at);
                    slot.open_jobs.fetch_sub(1, Ordering::AcqRel);
                    let result = if status.is_ok() {
                        writer.written().to_vec()
                    } else {
                        Vec::new()
                    };
                    jobs.push(JobUpdate {
                        job: id,
                        device: job.device,
                        owner: job.owner,
                        event: JobEvent::Finished { status, result },
                    });
                }
            }
            changed = true;
        }
        if changed {
            self.wake();
        }
    }

    fn slot(&self, index: u32) -> Option<Arc<Slot>> {
        self.slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index as usize)
            .cloned()
    }

    fn slot_by_id(&self, id: &str) -> Option<Arc<Slot>> {
        self.slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|slot| slot.spec.id == id)
            .cloned()
    }

    fn all(&self) -> Vec<Arc<Slot>> {
        self.slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// When a rule is next due a look: now when a hint named it and the gap since its last look
    /// has passed.
    fn rule_due_ns(&self, rule: &Rule) -> u64 {
        let due = rule.next_look_ns.load(Ordering::Acquire);
        if rule.hinted.load(Ordering::Acquire) {
            let gap = self.options.hint_gap.as_nanos() as u64;
            due.min(rule.looked_ns.load(Ordering::Acquire).saturating_add(gap))
        } else {
            due
        }
    }

    /// Looks for what fits each `[[each]]` entry that is due, and makes and binds a device for
    /// each resource found that no device has yet.
    fn look(&'static self) {
        let now = self.clock.now_ns();
        for (at, rule) in self.rules.iter().enumerate() {
            if self.rule_due_ns(rule) > now {
                continue;
            }
            rule.hinted.store(false, Ordering::Release);
            rule.looked_ns.store(now, Ordering::Release);
            let wait = self.options.retry_cap.as_nanos() as u64;
            rule.next_look_ns
                .store(now.saturating_add(wait), Ordering::Release);
            let mut found = Vec::new();
            {
                let backend = lock(&self.backend);
                let listed = backend.find(&rule.spec.pattern, &mut |name| {
                    found.push(name.to_string());
                });
                if listed.is_err() {
                    continue;
                }
                found.sort();
                found.dedup();
                found.retain(|name| self.free_for(&**backend, rule, name));
            }
            for name in found {
                self.make(at, rule, name);
            }
        }
    }

    /// Whether `rule` may take the resource `found`: none of its exceptions reaches it, no
    /// device has it yet, and no device of the board file's own names it.
    fn free_for(&self, backend: &(dyn Provider + Send), rule: &Rule, found: &str) -> bool {
        if rule
            .spec
            .except
            .iter()
            .any(|name| backend.fits(name, found))
        {
            return false;
        }
        self.all().iter().all(|slot| match &slot.made {
            Some((_, taken)) => taken != found,
            None => !slot
                .spec
                .bus
                .iter()
                .chain(slot.spec.uses.values())
                .any(|name| backend.fits(name, found)),
        })
    }

    /// Makes the device `rule` has for `found`, and binds it.
    fn make(&'static self, at: usize, rule: &Rule, found: String) {
        let spec = rule.spec.device(&found);
        let entry = self.drivers.get(spec.driver.as_str()).copied();
        let slot = {
            let mut slots = self.slots.write().unwrap_or_else(PoisonError::into_inner);
            let index = slots.len() as u32;
            // The board file was checked, so the entry's config is good.
            let Ok(slot) = Slot::new(index, spec, entry, Some((at, found))) else {
                return;
            };
            let slot = Arc::new(slot);
            // Its streams are there before anyone can find the device.
            self.streams.add(index, &slot.spec);
            slots.push(Arc::clone(&slot));
            slot
        };
        self.changes.fetch_add(1, Ordering::Release);
        self.bind(slot);
    }

    /// The device stopped working: let go of it and schedule a retry.
    ///
    /// The wait starts again from the shortest only once a driver has run for as long as the
    /// retry cap. One that fails sooner than that after each bind waits twice as long each time,
    /// up to the cap, so no driver can keep the host binding it.
    fn fault(&self, slot: &Slot, reason: &str) {
        // Whatever the driver was doing for its callers went with it.
        self.end_jobs(slot, Status::INTERRUPTED);
        let mut state = lock(&slot.state);
        let attempts = match &*state {
            DeviceState::Off(_) => return,
            DeviceState::Backoff { attempts, .. } => *attempts,
            DeviceState::Running => {
                let ran = self
                    .clock
                    .now_ns()
                    .saturating_sub(slot.bound_ns.load(Ordering::Acquire));
                if u128::from(ran) < self.options.cap().as_nanos() {
                    slot.short_runs.fetch_add(1, Ordering::AcqRel)
                } else {
                    slot.short_runs.store(0, Ordering::Release);
                    0
                }
            }
        };
        *state = self.next_state(slot, attempts, reason);
        self.moved(slot);
    }

    /// Lets go of a driver that is not in a call, outside its lock, with a panic in its drop
    /// contained.
    fn let_go(mut guard: MutexGuard<'_, Option<Box<dyn Instance>>>) {
        let dead = guard.take();
        drop(guard);
        drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.get() {
            waker();
        }
    }

    /// Brings a waiting device's next bind forward to now, or to [`Options::hint_gap`] after its
    /// last one if that is later. Never moves it later. Allocates nothing.
    fn bring_forward(&self, slot: &Slot) -> bool {
        let mut state = lock(&slot.state);
        let DeviceState::Backoff { next_retry_ns, .. } = &mut *state else {
            return false;
        };
        let gap = self.options.hint_gap.as_nanos() as u64;
        let soonest = slot
            .tried_ns
            .load(Ordering::Acquire)
            .saturating_add(gap)
            .max(self.clock.now_ns());
        if soonest >= *next_retry_ns {
            return false;
        }
        *next_retry_ns = soonest;
        drop(state);
        self.changes.fetch_add(1, Ordering::Release);
        true
    }

    /// Acts on a hint for one device it names: a waiting one is tried sooner, and a running one
    /// that ticks is ticked now, so its driver can look at what it holds.
    fn hinted(&self, slot: &Slot) -> bool {
        if slot.ticks() && slot.is_running() {
            slot.hinted.store(true, Ordering::Release);
            self.set_next_tick(slot, Some(0));
        }
        self.bring_forward(slot)
    }

    /// The driver says what it was bound with is dead: let go of it and bind it again under
    /// the failure rule. If a hint is what made it look, the hardware may already be back, so
    /// the bind is brought forward as the hint would have brought it.
    fn rebind(&self, slot: &Slot, guard: MutexGuard<'_, Option<Box<dyn Instance>>>, why: &str) {
        Self::let_go(guard);
        self.fault(slot, why);
        if slot.hinted.swap(false, Ordering::AcqRel) {
            self.bring_forward(slot);
        }
    }

    /// A device came up: whatever was waiting to use it is tried now, not when its wait ends.
    fn bring_users_forward(&self, id: &str) {
        let mut index = 0;
        let mut any = false;
        while let Some(user) = self.slot(index) {
            index += 1;
            if user.spec.uses.values().any(|used| used == id) {
                any |= self.bring_forward(&user);
            }
        }
        if any {
            self.wake();
        }
    }

    /// Where a device goes after failed attempt number `attempts + 1` in a row: back to waiting,
    /// or off when its board entry opted in to that (`auto_off_after`) and this was the last
    /// attempt it allows. No host-wide setting turns a device off.
    fn next_state(&self, slot: &Slot, attempts: u32, reason: &str) -> DeviceState {
        if let Some(after) = slot.spec.auto_off_after
            && attempts.saturating_add(1) >= after
        {
            return DeviceState::Off(OffReason::Policy);
        }
        let wait = self.options.backoff(attempts).as_nanos() as u64;
        DeviceState::Backoff {
            attempts: attempts.saturating_add(1),
            next_retry_ns: self.clock.now_ns() + wait,
            reason: reason.to_string(),
        }
    }

    fn set_next_tick(&self, slot: &Slot, next: Option<u64>) {
        let next = next.unwrap_or(NO_TICK);
        if next < slot.next_tick_ns.swap(next, Ordering::AcqRel)
            && let Some(waker) = self.waker.get()
        {
            waker();
        }
    }

    /// Whether the daemon is healthy, by the liveness the host was given. Closed without one.
    fn alive(&self) -> bool {
        self.liveness.get().is_some_and(|liveness| liveness.alive())
    }

    /// Whether a device may be ticked now: any device, or a keep-alive one while the daemon is
    /// healthy. Every tick goes through here, so a keep-alive device is never fed by a call.
    fn may_tick(&self, slot: &Slot) -> bool {
        !slot.spec.keepalive || self.alive()
    }

    /// Ticks a device on this thread, inside the panic boundary.
    fn tick_here(&self, slot: &Slot) {
        if slot.hung.load(Ordering::Acquire) {
            return;
        }
        // A keep-alive device is not ticked while the daemon is not healthy. It is asked again in
        // a short while, so it resumes as soon as the daemon is.
        if !self.may_tick(slot) {
            self.set_next_tick(
                slot,
                Some(self.clock.now_ns().saturating_add(GATED_POLL_NS)),
            );
            return;
        }
        let mut guard = lock(&slot.instance);
        let Some(instance) = guard.as_mut() else {
            return;
        };
        let now = self.clock.now_ns();
        let ticked = catch_unwind(AssertUnwindSafe(|| {
            let next = self.tick_instance(slot, instance.as_mut(), now);
            self.poll_jobs(slot, instance.as_mut());
            (next, asks_rebind(instance.as_ref()))
        }));
        match ticked {
            Ok((next, None)) => {
                slot.hinted.store(false, Ordering::Release);
                self.set_next_tick(slot, next);
            }
            Ok((_, Some(reason))) => self.rebind(slot, guard, reason),
            Err(_) => {
                let dead = guard.take();
                drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
                drop(guard);
                self.fault(slot, "the driver panicked");
            }
        }
    }

    /// Ticks a device on its lane. A busy lane leaves the tick due, for next time.
    fn tick(&'static self, slot: Arc<Slot>) {
        let Ok(lane) = self.lane(&slot) else {
            return;
        };
        let work = Work::Tick(TickWork {
            inner: self,
            slot: Arc::clone(&slot),
        });
        if let Err(LaneError::Deadline) = lane.run(&[], &mut [], self.options.call_deadline, work) {
            self.on_deadline(&slot);
        }
    }

    /// Runs a method on this thread, inside the panic boundary.
    fn call_here(
        &self,
        slot: &Slot,
        caller: CallerId,
        as_job: bool,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        self.call_from(slot, caller, false, as_job, method, args, reply)
    }

    /// Runs a method here. `reads_only` is set for a caller that may change nothing: a
    /// host-owned device's driver calling a device it uses. `as_job` starts a long job: the
    /// method must be one its contract marks as starting one, and the reply is then the host's
    /// number for the job, a `u64`, in place of the driver's.
    #[allow(clippy::too_many_arguments)]
    fn call_from(
        &self,
        slot: &Slot,
        caller: CallerId,
        reads_only: bool,
        as_job: bool,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        if slot.hung.load(Ordering::Acquire) {
            return Err(Status::NO_DEVICE);
        }
        match (slot.starts_job(method), as_job) {
            // A job is started with the job form, which keeps it on the books; a plain call
            // would leave it running with no one to tell.
            (Some(true), false) => return Err(Status::NOT_SUPPORTED),
            (Some(false) | None, true) => return Err(Status::INVALID_ARGUMENT),
            (Some(true), true) if !slot.runs_jobs() => return Err(Status::NOT_SUPPORTED),
            (Some(true), true) if self.jobs().open.len() >= MAX_JOBS => {
                return Err(Status::BUSY);
            }
            _ => {}
        }
        let mut guard = lock(&slot.instance);
        let Some(instance) = guard.as_mut() else {
            return Err(Status::NO_DEVICE);
        };
        let mut writer = Reply::new(reply);
        let ticks = slot.ticks();
        let tracks_callers = slot.tracks_callers();
        let guest = reads_only || slot.spec.owner == Owner::Host;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if guest && !only_reads(instance.as_ref(), method) {
                return (NOT_OURS, None);
            }
            if tracks_callers {
                instance.caller(caller);
            }
            let mut status = instance.dispatch(method, args, &mut writer);
            if as_job && status.is_ok() {
                status = match JobId::decode(&mut Reader::new(writer.written())) {
                    Ok(job) if writer.len() == 8 => {
                        let id = self.open_job(slot, job, caller);
                        writer.clear();
                        id.encode(&mut writer)
                            .map_or_else(|status| status, |()| Status::OK)
                    }
                    // The contract says a job's number comes back, and nothing else did.
                    _ => Status::IO,
                };
            }
            if ticks && self.may_tick(slot) {
                let now = self.clock.now_ns();
                self.set_next_tick(slot, self.tick_instance(slot, instance.as_mut(), now));
            }
            self.poll_jobs(slot, instance.as_mut());
            (status, asks_rebind(instance.as_ref()))
        }));
        let status = match outcome {
            Ok((status, None)) => status,
            // The call's own answer stands. The driver goes, and is bound again.
            Ok((status, Some(reason))) => {
                self.rebind(slot, guard, reason);
                return if status.is_ok() {
                    Ok(writer.len())
                } else {
                    Err(status)
                };
            }
            Err(_) => {
                let dead = guard.take();
                drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
                drop(guard);
                self.fault(slot, "the driver panicked");
                return Err(Status::PANIC);
            }
        };
        if status.is_ok() {
            slot.errors.store(0, Ordering::Relaxed);
            return Ok(writer.len());
        }
        if counts_as_gone(status)
            && slot.errors.fetch_add(1, Ordering::Relaxed) + 1 >= self.options.error_limit
        {
            slot.errors.store(0, Ordering::Relaxed);
            let dead = guard.take();
            drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
            drop(guard);
            self.fault(slot, "the device stopped answering");
        }
        Err(status)
    }

    fn lane(&self, slot: &Slot) -> core::result::Result<Arc<Lane>, LaneError> {
        let mut lanes = lock(&self.lanes);
        if let Some(lane) = lanes.get(slot.lane_key.as_str()) {
            return Ok(Arc::clone(lane));
        }
        let name = format!("lemnos-lane-{}", lanes.len());
        let lane = Arc::new(Lane::spawn(name, self.options.lane_stack)?);
        lanes.insert(slot.lane_key.clone(), Arc::clone(&lane));
        Ok(lane)
    }

    fn abandon_lane(&self, slot: &Slot) {
        lock(&self.lanes).remove(slot.lane_key.as_str());
    }

    /// Whichever call was running, the lane is gone. Every device on it
    /// keeps working through a new lane; only `slot` is marked.
    fn on_deadline(&self, slot: &Slot) {
        self.abandon_lane(slot);
        slot.hung.store(true, Ordering::Release);
        self.fault(slot, "a call did not finish in time");
    }

    /// Runs `work` on a device's driver, on its lane and inside the panic boundary, then ticks
    /// it if it ticks and passes on what changed about its jobs.
    fn on_lane(
        &'static self,
        slot: Arc<Slot>,
        work: impl FnOnce(&mut dyn Instance) + Send + 'static,
    ) {
        let Ok(lane) = self.lane(&slot) else {
            return;
        };
        let target = Arc::clone(&slot);
        let work = Work::Once(Box::new(move |_: &[u8], _: &mut [u8]| {
            let mut guard = lock(&target.instance);
            let Some(instance) = guard.as_mut() else {
                return Ok(0);
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                work(instance.as_mut());
                let next = (target.ticks() && self.may_tick(&target)).then(|| {
                    let now = self.clock.now_ns();
                    self.tick_instance(&target, instance.as_mut(), now)
                });
                self.poll_jobs(&target, instance.as_mut());
                (next, asks_rebind(instance.as_ref()))
            }));
            match outcome {
                Ok((_, Some(reason))) => self.rebind(&target, guard, reason),
                Ok((Some(next), None)) => self.set_next_tick(&target, next),
                Ok((None, None)) => {}
                Err(_) => {
                    let dead = guard.take();
                    drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
                    drop(guard);
                    self.fault(&target, "the driver panicked");
                }
            }
            Ok(0)
        }));
        if let Err(LaneError::Deadline) = lane.run(&[], &mut [], self.options.call_deadline, work) {
            self.on_deadline(&slot);
        }
    }

    /// Tells a device that a caller has gone, and cancels the jobs that caller started on it.
    fn caller_gone(&'static self, slot: Arc<Slot>, caller: CallerId) {
        let started: Vec<JobId> = self
            .jobs()
            .open
            .iter()
            .filter(|job| job.device == slot.index && job.owner == caller)
            .map(|job| job.driver_job)
            .collect();
        let tells = slot.tracks_callers();
        self.on_lane(slot, move |instance| {
            for job in started {
                instance.job_cancel(job);
            }
            if tells {
                instance.caller_gone(caller);
            }
        });
    }

    fn call(
        &'static self,
        slot: Arc<Slot>,
        caller: CallerId,
        as_job: bool,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        let lane = self.lane(&slot).map_err(|_| Status::NO_MEMORY)?;
        let work = Work::Call(CallWork {
            inner: self,
            slot: Arc::clone(&slot),
            method,
            caller,
            as_job,
        });
        match lane.run(args, reply, self.options.call_deadline, work) {
            Ok(result) => result,
            Err(LaneError::Deadline) => {
                self.on_deadline(&slot);
                Err(Status::TIMED_OUT)
            }
            Err(LaneError::Busy) => Err(Status::TIMED_OUT),
            Err(LaneError::Spawn) => Err(Status::NO_MEMORY),
        }
    }

    /// Builds the driver, on its lane, inside the panic boundary.
    fn bind(&'static self, slot: Arc<Slot>) {
        slot.tried_ns.store(self.clock.now_ns(), Ordering::Release);
        let Some(entry) = slot.entry else {
            let reason = format!("no driver named {:?}", slot.spec.driver);
            self.record_failed_bind(&slot, &reason);
            return;
        };

        // The operating system's driver has this device. A driver that doesn't say how it would
        // leave the hardware alone is not started on it at all.
        let guest = slot.spec.owner == Owner::Host;
        if guest && entry.host_owned == HostOwned::Refused {
            let reason = format!(
                "driver {:?} can't be bound to a host-owned device: it doesn't say it would \
                 only read it",
                slot.spec.driver
            );
            self.record_failed_bind(&slot, &reason);
            return;
        }

        // A call stuck in the old instance holds its lock until it returns.
        let Ok(mut old) = slot.instance.try_lock() else {
            self.record_failed_bind(&slot, "a call into the old driver is still stuck");
            return;
        };
        let dead = old.take();
        drop(old);
        drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
        slot.hung.store(false, Ordering::Release);

        let Ok(lane) = self.lane(&slot) else {
            self.record_failed_bind(&slot, "no thread for the driver");
            return;
        };
        let target = Arc::clone(&slot);
        let work = Work::Once(Box::new(move |_: &[u8], _: &mut [u8]| {
            let entries: Vec<(&str, ConfigValue<'_>)> = target
                .config
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_value()))
                .collect();
            let lists: Vec<(&str, Vec<&str>)> = target
                .lists
                .iter()
                .map(|(key, items)| (key.as_str(), items.iter().map(String::as_str).collect()))
                .collect();
            let borrowed: Vec<(&str, &[&str])> = lists
                .iter()
                .map(|(key, items)| (*key, items.as_slice()))
                .collect();
            let config = Config::new(&entries)
                .with_lists(&borrowed)
                .on_board(&self.board)
                .on_host_version(self.daemon_version);
            let config = if guest {
                config.for_host_owned()
            } else {
                config
            };
            let mut binding = Binding {
                inner: self,
                slot: &target,
                missing: RefCell::new(Vec::new()),
            };
            let link = target.spec.link.as_deref();
            let built = catch_unwind(AssertUnwindSafe(|| {
                let mut res = Resources::new(&mut binding, link);
                entry.factory.bind(&mut res, &config)
            }));
            let missing = binding.missing.take();
            match built {
                Ok(Ok(instance)) => {
                    *lock(&target.missing) = missing;
                    *lock(&target.instance) = Some(instance);
                    Ok(0)
                }
                Ok(Err(status)) => Err(status),
                Err(_) => Err(Status::PANIC),
            }
        }));
        match lane.run(&[], &mut [], self.options.call_deadline, work) {
            Ok(Ok(_)) => {
                slot.errors.store(0, Ordering::Relaxed);
                slot.bound_ns.store(self.clock.now_ns(), Ordering::Release);
                *lock(&slot.state) = DeviceState::Running;
                if slot.ticks() {
                    self.set_next_tick(&slot, Some(0));
                }
                self.moved(&slot);
                self.bring_users_forward(&slot.spec.id);
            }
            Ok(Err(status)) => {
                self.record_failed_bind(&slot, &format!("bind failed: {status:?}"));
            }
            Err(LaneError::Deadline) => {
                self.abandon_lane(&slot);
                self.record_failed_bind(&slot, "bind did not finish in time");
            }
            Err(LaneError::Busy) => self.record_failed_bind(&slot, "its bus is busy"),
            Err(LaneError::Spawn) => self.record_failed_bind(&slot, "no thread for the driver"),
        }
    }

    fn record_failed_bind(&self, slot: &Slot, reason: &str) {
        let mut state = lock(&slot.state);
        let attempts = match &*state {
            DeviceState::Off(_) => return,
            DeviceState::Backoff { attempts, .. } => *attempts,
            DeviceState::Running => 0,
        };
        *state = self.next_state(slot, attempts, reason);
        self.moved(slot);
    }

    /// Ticks a driver: after giving it the board first when it watches the board.
    fn tick_instance(&self, slot: &Slot, instance: &mut dyn Instance, now_ns: u64) -> Option<u64> {
        if slot.entry.is_some_and(|entry| entry.watches) {
            instance.board(&BoardRead { inner: self });
        }
        instance.tick(now_ns)
    }

    fn device_health(&self, slot: &Slot, depth: u32) -> Health {
        self.health_of(slot, depth).0
    }

    /// A device's health, and whether it is settled: `false` when its driver is in a call right
    /// now, so its own health is not known until the call returns. An unsettled device reads as
    /// [`Health::Ok`], as it always has, and its needs are read as they are. Allocates nothing.
    fn health_of(&self, slot: &Slot, depth: u32) -> (Health, bool) {
        // Read under the state's lock only, which is released before the driver is asked.
        let fixed = match &*lock(&slot.state) {
            DeviceState::Running => None,
            DeviceState::Backoff { .. } => Some(Health::Failed("retrying")),
            DeviceState::Off(_) => Some(Health::Failed("off")),
        };
        let (own, mut settled) = match fixed {
            Some(health) => (health, true),
            None => match slot.instance.try_lock() {
                Ok(mut guard) => match guard.as_mut() {
                    Some(instance) => (
                        catch_unwind(AssertUnwindSafe(|| instance.health()))
                            .unwrap_or(Health::Failed("the driver panicked")),
                        true,
                    ),
                    None => (Health::Failed("not running"), true),
                },
                // A call that never came back holds its driver for good: that is a failure.
                Err(_) if slot.hung.load(Ordering::Acquire) => {
                    (Health::Failed("a call never returned"), true)
                }
                // In a call right now, so it's alive, and what it reports is not known yet.
                Err(_) => (Health::Ok, false),
            },
        };
        if depth == 0 || matches!(own, Health::Failed(_) | Health::Gone(_)) {
            return (own, settled);
        }
        let Some(entry) = slot.entry else {
            return (own, settled);
        };
        let mut combined = own;
        for need in entry.manifest.needs {
            if !matches!(need.kind, NeedKind::Device(_)) {
                continue;
            }
            let child = slot
                .spec
                .uses
                .get(need.name)
                .and_then(|id| self.slot_by_id(id));
            let child_health = match &child {
                Some(child) => {
                    let (health, child_settled) = self.health_of(child, depth - 1);
                    settled &= child_settled;
                    health
                }
                None => Health::Failed("missing"),
            };
            let named = slot.spec.uses.contains_key(need.name);
            match (child_health, need.optional) {
                (Health::Failed(_) | Health::Gone(_), false) => {
                    return (Health::Failed("a required device failed"), settled);
                }
                (Health::Failed(_) | Health::Gone(_), true) if named && combined == Health::Ok => {
                    combined = Health::Degraded("an optional device is missing");
                }
                (Health::Degraded(why), _) if combined == Health::Ok => {
                    combined = Health::Degraded(why);
                }
                _ => {}
            }
        }
        (combined, settled)
    }
}

/// The board as a driver that watches it reads it, from the host's own table. Each device is
/// read by its index, holding the table's lock only long enough to copy the device's pointer, so
/// no lock is held across a call back into the host.
struct BoardRead<'a> {
    inner: &'a Inner,
}

impl BoardView for BoardRead<'_> {
    fn each(&self, f: &mut dyn FnMut(BoardDevice<'_>)) {
        let count = self
            .inner
            .slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        for index in 0..count {
            let Some(slot) = self.inner.slot(index as u32) else {
                continue;
            };
            let bound = matches!(*lock(&slot.state), DeviceState::Running);
            let (health, _) = self.inner.health_of(&slot, MAX_DEPTH);
            f(BoardDevice {
                id: &slot.spec.id,
                origin: if slot.made.is_some() {
                    Origin::Hotplug
                } else {
                    Origin::BuiltIn
                },
                bound,
                health,
            });
        }
    }
}

impl Router for Inner {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let slot = self.slot(device).ok_or(Status::NOT_FOUND)?;
        self.call_here(&slot, CallerId::LOCAL, false, method, args, reply)
    }

    fn health(&self, device: u32) -> Health {
        match self.slot(device) {
            Some(slot) => self.device_health(&slot, MAX_DEPTH),
            None => Health::Failed("missing"),
        }
    }
}

/// Deepest chain of devices health is passed up through.
const MAX_DEPTH: u32 = 16;

/// Gives a binding driver what its board entry says it may have.
struct Binding<'a> {
    inner: &'static Inner,
    slot: &'a Slot,
    missing: RefCell<Vec<String>>,
}

impl Binding<'_> {
    /// Whether the device being bound is the operating system's, to be read and not changed.
    fn guest(&self) -> bool {
        self.slot.spec.owner == Owner::Host
    }

    fn bus_name(&self, need: &str) -> Result<String> {
        self.slot
            .spec
            .uses
            .get(need)
            .or(self.bus_for(need))
            .cloned()
            .ok_or(Status::NOT_FOUND)
    }

    /// `bus`, when it stands in for `need`: for any need the driver doesn't declare optional.
    /// An optional need is filled only by its own name, so one the board doesn't name is absent.
    fn bus_for(&self, need: &str) -> Option<&String> {
        let optional = self.slot.entry.is_some_and(|entry| {
            entry
                .manifest
                .needs
                .iter()
                .any(|declared| declared.name == need && declared.optional)
        });
        if optional {
            None
        } else {
            self.slot.spec.bus.as_ref()
        }
    }

    /// The name in `uses` isn't a bus on this board, so it must be a device.
    fn bus_is_absent(&self, need: &str) -> bool {
        match self.slot.spec.uses.get(need) {
            Some(name) => !lock(&self.inner.backend).provided(name),
            None => true,
        }
    }

    fn target(&self, need: &str) -> Option<Arc<Slot>> {
        let id = self.slot.spec.uses.get(need)?;
        self.inner.slot_by_id(id)
    }

    /// A call route to `target`'s contract `tag`: refused while it is not running.
    fn caller_of(&self, target: &Slot, tag: Tag) -> Result<LocalCaller> {
        if !target.is_running() {
            return Err(Status::NO_DEVICE);
        }
        let contract = target.contract_index(tag).ok_or(Status::INVALID_ARGUMENT)?;
        let router: &'static dyn Router = match self.inner.reading.get() {
            Some(reading) if self.guest() => *reading,
            // Never without one once the host has started; refuse rather than hand a
            // host-owned device a way to change another.
            None if self.guest() => return Err(Status::NOT_PERMITTED),
            _ => self.inner,
        };
        Ok(LocalCaller::new(router, target.index, contract))
    }
}

macro_rules! buses {
    ($($method:ident -> $handle:ty;)*) => {$(
        fn $method(&mut self, need: &str) -> Result<$handle> {
            let bus = self.bus_name(need)?;
            let handle = lock(&self.inner.backend).$method(&bus)?;
            // A host-owned device gets it with every change refused.
            Ok(if self.guest() {
                guest::$method(handle)
            } else {
                handle
            })
        }
    )*};
}

impl Provider for Binding<'_> {
    fn provided(&self, need: &str) -> bool {
        if self.slot.spec.streams.contains_key(need) {
            return true;
        }
        if self.slot.spec.uses.contains_key(need) && self.bus_is_absent(need) {
            let running = self.target(need).is_some_and(|target| target.is_running());
            if !running {
                self.missing.borrow_mut().push(need.to_string());
            }
            return running;
        }
        match self.bus_name(need) {
            Ok(bus) => lock(&self.inner.backend).provided(&bus),
            Err(_) => false,
        }
    }

    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller> {
        let target = self.target(need).ok_or(Status::NOT_FOUND)?;
        self.caller_of(&target, tag)
    }

    fn link_device(&mut self, tag: Tag) -> Result<LocalCaller> {
        let id = self.slot.spec.link.as_deref().ok_or(Status::NOT_FOUND)?;
        let target = self.inner.slot_by_id(id).ok_or(Status::NOT_FOUND)?;
        self.caller_of(&target, tag)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        let bus = self.bus_name(need)?;
        let handle = lock(&self.inner.backend).i2c(&bus)?;
        // A host-owned device's driver reads registers of the width it declares, and writes
        // nothing else.
        Ok(if self.guest() {
            let register = self.slot.entry.and_then(|entry| entry.i2c_register_address);
            guest::i2c(handle, register)
        } else {
            handle
        })
    }

    buses! {
        spi -> Spi;
        raw_spi -> RawSpiBus;
        line -> Line;
        pwm -> Pwm;
        uart -> Uart;
        usb_device -> UsbDeviceHandle;
        usb_function -> UsbFunctionHandle;
        registers -> RegistersHandle;
        interrupt -> InterruptHandle;
        dma -> DmaHandle;
        clocks -> ClocksHandle;
        reset -> ResetHandle;
    }

    fn stream(&mut self, need: &str) -> Result<StreamHandle> {
        let endpoint: StreamHandle = if self.slot.spec.streams.contains_key(need) {
            lemnos_core::Held::boxed(Box::new(
                self.inner.streams.producer(self.slot.index, need)?,
            ))
        } else {
            let bus = self.bus_name(need)?;
            lock(&self.inner.backend).stream(&bus)?
        };
        Ok(if self.guest() {
            guest::stream(endpoint)
        } else {
            endpoint
        })
    }
}

/// Children before parents, by `uses`. A device named by nothing can go anywhere.
fn bind_order(devices: &[DeviceSpec]) -> core::result::Result<Vec<usize>, HostError> {
    fn visit(
        at: usize,
        devices: &[DeviceSpec],
        marks: &mut [u8],
        order: &mut Vec<usize>,
    ) -> core::result::Result<(), HostError> {
        match marks[at] {
            2 => return Ok(()),
            1 => return Err(HostError::Cycle(devices[at].id.clone())),
            _ => {}
        }
        marks[at] = 1;
        for used in devices[at].uses.values() {
            if let Some(child) = devices.iter().position(|d| d.id == *used) {
                visit(child, devices, marks, order)?;
            }
        }
        marks[at] = 2;
        order.push(at);
        Ok(())
    }

    let mut marks = vec![0; devices.len()];
    let mut order = Vec::with_capacity(devices.len());
    for at in 0..devices.len() {
        visit(at, devices, &mut marks, &mut order)?;
    }
    Ok(order)
}

impl Host {
    /// Starts every device. A device that can't start is left retrying, and
    /// never stops the others; only a board that can't be ordered is refused.
    ///
    /// What the board file's `[host]` section sets wins over `options`; what it leaves out is
    /// taken from `options` ([`HostSpec::apply`](crate::board::HostSpec::apply)).
    pub fn start(
        board: &BoardFile,
        drivers: &[DriverEntry],
        backend: Box<dyn Provider + Send>,
        clock: Arc<dyn HostClock>,
        options: Options,
    ) -> core::result::Result<Self, HostError> {
        Self::start_with_streams(
            board,
            drivers,
            backend,
            clock,
            options,
            Arc::new(HeapAllocator),
        )
    }

    /// [`Host::start`], with `allocator` making the memory behind each frame stream.
    pub fn start_with_streams(
        board: &BoardFile,
        drivers: &[DriverEntry],
        backend: Box<dyn Provider + Send>,
        clock: Arc<dyn HostClock>,
        options: Options,
        allocator: Arc<dyn StreamAllocator>,
    ) -> core::result::Result<Self, HostError> {
        Self::start_versioned("", board, drivers, backend, clock, options, allocator)
    }

    /// [`Host::start_with_streams`] for a daemon: `daemon_version` is its package version, which
    /// every driver is given as [`Config::host_version`] (the `version.lemnos` fact).
    pub fn start_versioned(
        daemon_version: &'static str,
        board: &BoardFile,
        drivers: &[DriverEntry],
        backend: Box<dyn Provider + Send>,
        clock: Arc<dyn HostClock>,
        options: Options,
        allocator: Arc<dyn StreamAllocator>,
    ) -> core::result::Result<Self, HostError> {
        let options = board.host.apply(options);
        let order = bind_order(&board.devices)?;
        let table: BTreeMap<_, _> = drivers
            .iter()
            .map(|driver| (driver.manifest.name, *driver))
            .collect();
        let mut slots = Vec::with_capacity(board.devices.len());
        for (index, spec) in board.devices.iter().enumerate() {
            let entry = table.get(spec.driver.as_str()).copied();
            slots.push(Arc::new(Slot::new(
                index as u32,
                spec.clone(),
                entry,
                None,
            )?));
        }
        let rules = board
            .each
            .iter()
            .map(|spec| Rule {
                spec: spec.clone(),
                hinted: AtomicBool::new(false),
                next_look_ns: AtomicU64::new(0),
                looked_ns: AtomicU64::new(0),
            })
            .collect();

        let streams = Streams::new(allocator, &board.devices);

        // Lives as long as the process: drivers hold plain references to it.
        // `shutdown` lets every device go.
        let inner: &'static Inner = Box::leak(Box::new(Inner {
            board: board.board.id.clone(),
            daemon_version,
            slots: RwLock::new(slots),
            rules,
            lanes: Mutex::new(HashMap::new()),
            changes: AtomicU64::new(0),
            backend: Mutex::new(backend),
            clock,
            options,
            drivers: table,
            waker: OnceLock::new(),
            liveness: OnceLock::new(),
            streams,
            reading: OnceLock::new(),
            jobs: Mutex::new(Jobs::new()),
        }));
        // Lives as long as `inner` does, which is for good.
        let _ = inner.reading.set(Box::leak(Box::new(Reading(inner))));
        let host = Self { inner };
        let slots = inner.all();
        for at in order {
            if !board.devices[at].disabled {
                inner.bind(Arc::clone(&slots[at]));
            }
        }
        // After the board's own devices, which keep what they name from every rule.
        inner.look();
        Ok(host)
    }

    fn require(&self, id: &str) -> core::result::Result<Arc<Slot>, HostError> {
        self.inner
            .slot_by_id(id)
            .ok_or_else(|| HostError::NoDevice(id.to_string()))
    }

    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.inner
            .all()
            .iter()
            .map(|slot| self.info(slot))
            .collect()
    }

    fn info(&self, slot: &Slot) -> DeviceInfo {
        // The revision is read first: the state read after it is never older than the revision.
        let revision = slot.revision.load(Ordering::Acquire);
        let state = lock(&slot.state).clone();
        DeviceInfo {
            index: slot.index,
            revision,
            id: slot.spec.id.clone(),
            driver: slot.spec.driver.clone(),
            tags: slot
                .entry
                .map(|e| e.manifest.provides.iter().map(|c| c.tag).collect())
                .unwrap_or_default(),
            state,
            health: self.inner.device_health(slot, MAX_DEPTH),
            link: slot.spec.link.clone(),
        }
    }

    pub fn device(&self, id: &str) -> Option<DeviceInfo> {
        self.inner.slot_by_id(id).map(|slot| self.info(&slot))
    }

    /// Every device providing `tag`.
    pub fn find(&self, tag: &str) -> Vec<DeviceInfo> {
        self.inner
            .all()
            .iter()
            .filter(|slot| {
                slot.entry
                    .is_some_and(|e| e.manifest.provides.iter().any(|c| c.tag.as_str() == tag))
            })
            .map(|slot| self.info(slot))
            .collect()
    }

    /// The `link` a device's board entry names.
    pub fn link(&self, id: &str) -> Option<String> {
        self.inner.slot_by_id(id)?.spec.link.clone()
    }

    /// The manifest of a device's driver.
    pub fn manifest(&self, device: u32) -> Option<Manifest> {
        Some(self.inner.slot(device)?.entry?.manifest)
    }

    /// Asks a running device to describe itself.
    pub fn describe(&self, device: u32) -> Result<Description> {
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        let mut guard = slot.instance.try_lock().map_err(|_| Status::BUSY)?;
        let instance = guard.as_mut().ok_or(Status::NO_DEVICE)?;
        let mut out = Description::new();
        catch_unwind(AssertUnwindSafe(|| instance.describe(&mut out)))
            .map_err(|_| Status::PANIC)?;
        Ok(out)
    }

    /// Runs a method on a device's lane, and returns the reply's length. A device the board
    /// marks `owner = "host"` runs only methods its description names as values; any other call
    /// is `NOT_PERMITTED`, and so is one from another driver. Its driver is bound only if it
    /// says how it leaves such a device alone ([`HostOwned`]), with every need handed to it
    /// read-only, so what it does from `start` and `tick` can change nothing either.
    pub fn call(
        &self,
        device: u32,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        self.call_as(CallerId::LOCAL, device, method, args, reply)
    }

    /// [`Host::call`], for a caller that can go away. Tell the host when it does, with
    /// [`Host::caller_gone`].
    pub fn call_as(
        &self,
        caller: CallerId,
        device: u32,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        self.inner.call(slot, caller, false, method, args, reply)
    }

    /// Starts a long job for `caller`: runs `method`, which its contract marks as starting one,
    /// and returns the host's number for the job. From then on the job's progress and its end
    /// come from [`Host::take_job_updates`], marked as `caller`'s, until it finishes. The job is
    /// cancelled when `caller` goes ([`Host::caller_gone`]) and ends as
    /// [`Status::INTERRUPTED`] if its driver is lost.
    ///
    /// `INVALID_ARGUMENT` for a method that starts no job, `NOT_SUPPORTED` for a driver that
    /// runs none, and `BUSY` while [`MAX_JOBS`] are open. A job method can't be run with
    /// [`Host::call`]: that is `NOT_SUPPORTED`.
    pub fn start_job(
        &self,
        caller: CallerId,
        device: u32,
        method: MethodId,
        args: &[u8],
    ) -> Result<u64> {
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        let mut reply = [0u8; 8];
        let n = self
            .inner
            .call(slot, caller, true, method, args, &mut reply)?;
        u64::decode(&mut Reader::new(&reply[..n]))
    }

    /// Cancels a job `caller` started. The driver stops it and it finishes as
    /// [`Status::CANCELLED`], or as it really ended if it got there first. `NOT_FOUND` for a job
    /// that has finished or never was, `INTERRUPTED` for one a host before this one started (a
    /// restart ended it), and `NOT_PERMITTED` for one another caller started.
    /// [`CallerId::LOCAL`] may cancel any.
    pub fn cancel_job(&self, caller: CallerId, job: u64) -> Result<()> {
        let (device, driver_job) = {
            let jobs = self.inner.jobs();
            let Some(open) = jobs.open.iter().find(|open| open.id == job) else {
                return Err(if (job >> 32) as u32 == jobs.epoch {
                    Status::NOT_FOUND
                } else {
                    Status::INTERRUPTED
                });
            };
            if open.owner != caller && caller != CallerId::LOCAL {
                return Err(Status::NOT_PERMITTED);
            }
            (open.device, open.driver_job)
        };
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        self.inner
            .on_lane(slot, move |instance| instance.job_cancel(driver_job));
        Ok(())
    }

    /// Moves what changed about long jobs, oldest first, into `into`. The host's waker
    /// ([`Host::set_waker`]) is called when there is something to take.
    pub fn take_job_updates(&self, into: &mut Vec<JobUpdate>) {
        into.extend(self.inner.jobs().updates.drain(..));
    }

    /// How many long jobs are open.
    pub fn open_jobs(&self) -> usize {
        self.inner.jobs().open.len()
    }

    /// Tells every driver that tracks callers that `caller` has gone, and cancels every job it
    /// started. Blocks until each driver has answered or its call deadline has passed.
    pub fn caller_gone(&self, caller: CallerId) {
        self.inner.streams.caller_gone(caller);
        let mut index = 0;
        while let Some(slot) = self.inner.slot(index) {
            index += 1;
            let started = slot.open_jobs.load(Ordering::Acquire) > 0
                && self
                    .inner
                    .jobs()
                    .open
                    .iter()
                    .any(|job| job.device == slot.index && job.owner == caller);
            if (slot.tracks_callers() || started) && slot.is_running() {
                self.inner.caller_gone(slot, caller);
            }
        }
    }

    /// Takes the consumer's end of a device's frame stream for `caller`, until it detaches or
    /// [`Host::caller_gone`]. A stream has one consumer: another gets `BUSY`. An unknown device or
    /// name is `NOT_FOUND`.
    pub fn stream_attach(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
    ) -> core::result::Result<Lease, Status> {
        self.inner.streams.attach(device, name, caller)
    }

    /// Gives up a stream `caller` holds. `NOT_FOUND` if it doesn't hold it.
    pub fn stream_detach(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
    ) -> core::result::Result<(), Status> {
        self.inner.streams.detach(device, name, caller)
    }

    /// Takes the producer's end of a stream without a driver: a software producer. A stream
    /// has one producer at a time, and a driver that needs the stream takes it the same way.
    pub fn stream_producer(
        &self,
        device: u32,
        name: &str,
    ) -> core::result::Result<ProducerEndpoint, Status> {
        self.inner.streams.producer(device, name)
    }

    /// What the board file says a stream's buffers are, or `None` if there's no such stream.
    /// Whoever attaches a consumer can know before it does whether the lease will carry one
    /// descriptor per buffer.
    #[must_use]
    pub fn stream_memory(&self, device: u32, name: &str) -> Option<StreamMemory> {
        self.inner.streams.memory(device, name)
    }

    /// How a stream stands, or `None` if there's no such stream.
    #[must_use]
    pub fn stream_stats(&self, device: u32, name: &str) -> Option<StreamStats> {
        self.inner.streams.stats(device, name)
    }

    /// Retries what is due and rebinds devices whose optional parts have
    /// arrived. Call this regularly.
    pub fn tick(&self) {
        self.inner.look();
        let now = self.inner.clock.now_ns();
        for slot in self.inner.all() {
            let due = matches!(
                &*lock(&slot.state),
                DeviceState::Backoff { next_retry_ns, .. } if *next_retry_ns <= now
            );
            if due {
                self.inner.bind(Arc::clone(&slot));
                continue;
            }
            if slot.is_running() && self.an_optional_part_arrived(&slot) {
                self.inner.bind(Arc::clone(&slot));
            }
        }
        self.run_ticks();
    }

    /// Takes a backend's word that hardware came or went. See [`Hint`].
    ///
    /// This only marks what is to be done and wakes whoever calls [`Host::tick`], which does it:
    /// any number of hints before the next tick bring on one bind of a device, and never one
    /// sooner than [`Options::hint_gap`] after its last. Callable from any thread. Allocates
    /// nothing.
    pub fn hint(&self, hint: Hint<'_>) {
        // Whatever came may fit an `[[each]]` entry: a backend names what it saw, which can be
        // a resource the entry's pattern reaches as well as the pattern.
        let mut any = false;
        for rule in &self.inner.rules {
            rule.hinted.store(true, Ordering::Release);
            any = true;
        }
        let mut index = 0;
        while let Some(slot) = self.inner.slot(index) {
            index += 1;
            // A device a rule made is named by the rule's pattern too: the backend may know
            // only that something under it changed.
            let by_rule = |name: &str| {
                slot.made
                    .as_ref()
                    .is_some_and(|(at, _)| self.inner.rules[*at].spec.pattern == name)
            };
            any |= match hint {
                Hint::Any => self.inner.hinted(&slot),
                Hint::Changed(name) => {
                    (slot.names(name) || by_rule(name)) && self.inner.hinted(&slot)
                }
            };
        }
        if any {
            self.inner.wake();
        }
    }

    /// Ticks every device whose time has come. Allocates nothing, so it can run on every wake.
    pub fn run_ticks(&self) {
        let now = self.inner.clock.now_ns();
        let mut index = 0;
        while let Some(slot) = self.inner.slot(index) {
            index += 1;
            if slot.next_tick_ns.load(Ordering::Acquire) <= now && slot.is_running() {
                self.inner.tick(slot);
            }
        }
    }

    /// When the next tick is due, if any running device wants one.
    pub fn next_tick_ns(&self) -> Option<u64> {
        self.inner
            .slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|slot| slot.next_tick_ns.load(Ordering::Acquire) != NO_TICK)
            .filter(|slot| slot.is_running())
            .map(|slot| slot.next_tick_ns.load(Ordering::Acquire))
            .min()
    }

    /// Sets what to call when a device's next tick moves earlier than it was, so a caller
    /// sleeping until [`Host::next_tick_ns`] can look again. Only the first call takes effect.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        drop(self.inner.waker.set(Box::new(waker)));
    }

    /// Gives the host the daemon's liveness. Keep-alive devices (see
    /// [`DeviceSpec::keepalive`](crate::board::DeviceSpec::keepalive)) are ticked only while it
    /// says the daemon is healthy. Set once; a second call is ignored.
    pub fn set_liveness(&self, liveness: Arc<dyn Liveness>) {
        drop(self.inner.liveness.set(liveness));
    }

    fn an_optional_part_arrived(&self, slot: &Slot) -> bool {
        let Some(entry) = slot.entry else {
            return false;
        };
        let Ok(guard) = slot.instance.try_lock() else {
            return false;
        };
        let bound_without: Vec<_> = entry
            .manifest
            .needs
            .iter()
            .filter(|n| n.optional && matches!(n.kind, NeedKind::Device(_)))
            .filter_map(|n| slot.spec.uses.get(n.name).map(|id| (n.name, id)))
            .collect();
        drop(guard);
        let missing = lock(&slot.missing);
        bound_without.iter().any(|(need, id)| {
            missing.iter().any(|m| m == need)
                && self
                    .inner
                    .slot_by_id(id)
                    .is_some_and(|child| child.is_running())
        })
    }

    /// How this host treats its drivers: what it was started with, and the board file's
    /// `[host]` section over that.
    #[must_use]
    pub fn options(&self) -> Options {
        self.inner.options
    }

    /// When the next retry is due, if one is waiting, or the next look for what fits an
    /// `[[each]]` entry: [`Host::tick`] does both.
    pub fn next_retry_ns(&self) -> Option<u64> {
        let looks = self
            .inner
            .rules
            .iter()
            .map(|rule| self.inner.rule_due_ns(rule));
        self.inner
            .slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(|slot| match &*lock(&slot.state) {
                DeviceState::Backoff { next_retry_ns, .. } => Some(*next_retry_ns),
                _ => None,
            })
            .chain(looks)
            .min()
    }

    /// Goes up whenever any device changes state, so a caller can tell that
    /// [`Host::devices`] is worth reading again.
    pub fn changes(&self) -> u64 {
        self.inner.changes.load(Ordering::Acquire)
    }

    /// How many devices the host runs. Their indexes are `0..device_count()`.
    pub fn device_count(&self) -> usize {
        self.inner
            .slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// The device's state revision (see [`DeviceInfo::revision`]) and its health as the host
    /// reports it now, for a caller that watches for a change of health. `None` when there is no
    /// such device, or when its driver is in a call right now: its health is not known until the
    /// call returns, so the caller looks again then. Health is read from the driver at each look,
    /// so the caller decides how often to look: the host never looks on its own, and a look
    /// allocates nothing.
    pub fn look(&self, index: u32) -> Option<(u64, Health)> {
        let slot = self.inner.slot(index)?;
        // The revision is read first, as in `info`.
        let revision = slot.revision.load(Ordering::Acquire);
        match self.inner.health_of(&slot, MAX_DEPTH) {
            (health, true) => Some((revision, health)),
            (_, false) => None,
        }
    }

    /// Switches a device off and lets go of its hardware.
    pub fn off(&self, id: &str) -> core::result::Result<(), HostError> {
        let slot = self.require(id)?;
        *lock(&slot.state) = DeviceState::Off(OffReason::Manual);
        self.inner.moved(&slot);
        self.inner.end_jobs(&slot, Status::INTERRUPTED);
        if let Ok(mut guard) = slot.instance.try_lock() {
            let dead = guard.take();
            drop(guard);
            drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
        }
        Ok(())
    }

    /// Switches a device on, whatever turned it off, and binds it now.
    pub fn on(&self, id: &str) -> core::result::Result<(), HostError> {
        let slot = self.require(id)?;
        slot.short_runs.store(0, Ordering::Release);
        *lock(&slot.state) = DeviceState::Backoff {
            attempts: 0,
            next_retry_ns: 0,
            reason: "switched on".to_string(),
        };
        self.inner.moved(&slot);
        self.inner.bind(slot);
        Ok(())
    }

    /// Lets go of every device, parents first, so each releases its hardware.
    pub fn shutdown(&self) {
        self.inner.streams.close_all();
        let slots = self.inner.all();
        for slot in slots.iter().rev() {
            *lock(&slot.state) = DeviceState::Off(OffReason::Manual);
            self.inner.moved(slot);
            self.inner.end_jobs(slot, Status::INTERRUPTED);
            if let Ok(mut guard) = slot.instance.try_lock() {
                let dead = guard.take();
                drop(guard);
                drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
            }
        }
        lock(&self.inner.lanes).clear();
    }

    /// The drivers this host can start, by name.
    pub fn driver_names(&self) -> Vec<&'static str> {
        self.inner.drivers.keys().copied().collect()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}
