use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use lemnos_core::{
    AddressWidth, BoardDevice, BoardView, BusKind, CallerId, ClocksHandle, Config, ConfigError,
    ConfigValue, Decode, Description, DmaHandle, Driver, Encode, Figures, Health, Hint, HostClock,
    HostOwned, I2c, InterruptHandle, Item, ItemKind, JobId, JobState, Line, LocalCaller, Manifest,
    MethodId, NeedKind, Origin, Pio, Polled, Provider, Pwm, RawSpiBus, Reader, RefusalKind,
    RefusalSink, RegistersHandle, Reply, ResetHandle, Resources, Result, ResumeRecord, Retry,
    Router, Shape, Spi, Status, StreamHandle, Tag, Uart, UsbDeviceHandle, UsbFunctionHandle,
    Watchdog,
};

use crate::board::{
    BoardError, BoardFile, DeviceSpec, EACH_SEPARATOR, EachSpec, OwnedValue, Owner, resolve_kinds,
};
use crate::contain::{Lane, LaneError, Work};
use crate::stream::{
    Lease, MemfdAllocator, ProducerEndpoint, ReaderTicket, StreamAllocator, StreamMemory,
    StreamStats, Streams, TicketRefused, TicketState,
};
use crate::{Options, guest};

/// A bound driver, as the host runs it: [`Driver`] without its type. Every `#[driver]` type is
/// one. A driver that reaches the host some other way, such as one loaded from a package,
/// implements this and is run exactly as a built-in driver is.
pub trait Instance: Send {
    fn describe(&self, out: &mut Description);
    fn health(&self) -> Health;
    /// See [`Driver::figures`].
    fn figures(&self) -> Figures {
        Figures::NONE
    }
    /// See [`Driver::retrying`].
    fn retrying(&self) -> Option<Retry> {
        None
    }
    /// See [`Driver::resume_record`].
    fn resume_record(&self) -> Option<ResumeRecord> {
        None
    }
    /// See [`Driver::notes`].
    fn notes(&mut self) -> Option<&mut lemnos_core::Notes> {
        None
    }
    /// See [`Driver::resume`].
    fn resume(&mut self, _record: &ResumeRecord) -> bool {
        false
    }
    /// See [`Driver::rebinding`].
    fn rebinding(&mut self) -> bool {
        false
    }
    /// See [`Driver::took_up_link`].
    fn took_up_link(&self) -> core::result::Result<(), &'static str> {
        Err(lemnos_core::NOT_TAKEN_UP)
    }
    /// See [`Driver::refusal`].
    fn refusal(&mut self, _status: Status, _words: &mut Reply<'_>) {}
    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status;
    fn tick(&mut self, now_ns: u64) -> Option<u64>;
    /// See [`Driver::board`]. Asked only of a driver whose entry says it watches the board.
    fn board(&mut self, _view: &dyn BoardView) {}
    fn caller(&mut self, caller: CallerId);
    fn caller_gone(&mut self, caller: CallerId);
    /// See [`Driver::endpoints`]: the descriptors, duplicated for the caller to keep.
    fn endpoints(&mut self, _caller: CallerId) -> Result<[OwnedFd; 2]> {
        Err(Status::NOT_SUPPORTED)
    }
    /// See [`Driver::release_endpoints`].
    fn release_endpoints(&mut self, _caller: CallerId) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    /// See [`Driver::endpoints_ended`].
    fn endpoints_ended(&mut self) -> bool {
        false
    }
    /// See [`Driver::job`]. Asked only of a driver whose entry says it runs jobs.
    fn job(&mut self, _job: JobId, _result: &mut Reply<'_>) -> JobState {
        JobState::Finished(Status::NOT_SUPPORTED)
    }
    /// See [`Driver::job_cancel`].
    fn job_cancel(&mut self, _job: JobId) {}
    /// See [`Driver::HOLDS_OFF`]: whether [`Instance::off`] keeps this instance.
    fn holds_off(&self) -> bool {
        false
    }
    /// See [`Driver::off`].
    fn off(&mut self) -> lemnos_core::Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    /// See [`Driver::on`].
    fn on(&mut self) -> lemnos_core::Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    /// See [`Driver::wait_fd`]: a duplicate of it, for the host to wait on.
    fn wait_fd(&self) -> Option<OwnedFd> {
        None
    }
}

impl<D: Driver + Send> Instance for D {
    fn describe(&self, out: &mut Description) {
        Driver::describe(self, out);
    }

    fn health(&self) -> Health {
        Driver::health(self)
    }

    fn figures(&self) -> Figures {
        Driver::figures(self)
    }

    fn retrying(&self) -> Option<Retry> {
        Driver::retrying(self)
    }

    fn resume_record(&self) -> Option<ResumeRecord> {
        Driver::resume_record(self)
    }

    fn resume(&mut self, record: &ResumeRecord) -> bool {
        Driver::resume(self, record)
    }

    fn rebinding(&mut self) -> bool {
        Driver::rebinding(self)
    }

    fn took_up_link(&self) -> core::result::Result<(), &'static str> {
        Driver::took_up_link(self)
    }

    fn refusal(&mut self, status: Status, words: &mut Reply<'_>) {
        Driver::refusal(self, status, words);
    }

    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status {
        Driver::dispatch(self, method, args, reply)
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        Driver::tick(self, now_ns)
    }

    fn holds_off(&self) -> bool {
        <D as Driver>::HOLDS_OFF
    }

    fn off(&mut self) -> lemnos_core::Result<()> {
        Driver::off(self)
    }

    fn on(&mut self) -> lemnos_core::Result<()> {
        Driver::on(self)
    }

    fn wait_fd(&self) -> Option<OwnedFd> {
        Driver::wait_fd(self).and_then(|handle| handle.as_fd().try_clone_to_owned().ok())
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

    fn notes(&mut self) -> Option<&mut lemnos_core::Notes> {
        Driver::notes(self)
    }

    fn endpoints(&mut self, caller: CallerId) -> Result<[OwnedFd; 2]> {
        let [out, input] = Driver::endpoints(self, caller)?;
        let dup = |fd: lemnos_core::BufferHandle<'_>| {
            fd.as_fd().try_clone_to_owned().map_err(|_| Status::IO)
        };
        Ok([dup(out)?, dup(input)?])
    }

    fn release_endpoints(&mut self, caller: CallerId) -> Result<()> {
        Driver::release_endpoints(self, caller)
    }

    fn endpoints_ended(&mut self) -> bool {
        Driver::endpoints_ended(self)
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
    /// [`Driver::check_config`]: the settings check, run with no hardware before any device is made.
    check: Check,
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
            // Nothing says what a driver built elsewhere reads, so it takes every setting.
            check: accept,
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

/// A driver's settings check. See [`Driver::check_config`].
type Check = for<'a> fn(&Config<'a>) -> core::result::Result<(), ConfigError<'a>>;

fn accept<'a>(_: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
    Ok(())
}

/// Each device the board file gives, and each `[[each]]` entry, whose settings a driver refuses,
/// by its id, with the driver's reason (naming the key). Checked with no hardware and no device
/// made. `lemnos-ctl validate` refuses a board that has one; the daemon starts without that
/// device, which shows failed with this reason.
pub fn settings_refusals(board: &BoardFile, drivers: &[DriverEntry]) -> Vec<(String, String)> {
    let mut refused = Vec::new();
    for device in &board.devices {
        if let Some(problem) = refusal_for(board, device, drivers, None) {
            refused.push((device.id.clone(), problem));
        }
    }
    for each in &board.each {
        if let Some(problem) = refusal_for(
            board,
            &each.device(&each.pattern),
            drivers,
            each.need.as_deref(),
        ) {
            refused.push((each.id.clone(), problem));
        }
    }
    refused
}

/// The whole reason a device is refused, naming the device, or `None`: the reason the board or the
/// daemon gives it (already whole), else the reason its driver refuses its settings.
fn refusal_for(
    board: &BoardFile,
    spec: &DeviceSpec,
    drivers: &[DriverEntry],
    found: Option<&str>,
) -> Option<String> {
    match &spec.refused {
        Some(reason) => Some(reason.clone()),
        None => refusal_of(board, spec, drivers, found)
            .map(|problem| format!("device {:?}: {problem}", spec.id)),
    }
}

/// The first refusal of [`settings_refusals`] as an error, for `lemnos-ctl validate`.
///
/// # Errors
///
/// The first setting a driver refuses, by the device's id and the key.
pub fn check_settings(
    board: &BoardFile,
    drivers: &[DriverEntry],
) -> core::result::Result<(), BoardError> {
    match settings_refusals(board, drivers).into_iter().next() {
        Some((id, reason)) => Err(BoardError::Setting(id, reason)),
        None => Ok(()),
    }
}

/// The reason the device's driver refuses its settings, or `None` when it takes them, or has no
/// driver the host knows (that is named at bind).
fn refusal_of(
    board: &BoardFile,
    spec: &DeviceSpec,
    drivers: &[DriverEntry],
    found: Option<&str>,
) -> Option<String> {
    let entry = drivers
        .iter()
        .rev()
        .find(|driver| driver.manifest.name == spec.driver)?;
    if let Some(problem) = uses_refusal(board, spec, entry, drivers, found) {
        return Some(problem);
    }
    let tags: Vec<&str> = entry
        .manifest
        .provides
        .iter()
        .map(|c| c.tag.as_str())
        .collect();
    let taken = with_device_config(board, spec, |config| {
        if config.get("keys").is_some() && takes_trust_root(&tags) {
            return Some(
                "setting \"keys\": the board has one trust root, `[security] keys`, which the host gives to this driver; name no key list of its own".to_string(),
            );
        }
        (entry.check)(config).err().map(|error| error.to_string())
    });
    match taken {
        Ok(problem) => problem,
        Err(error) => Some(error),
    }
}

/// Runs `f` with the settings of the board's device `spec` as its driver is given them at bind (the
/// same [`Config`] the host builds). The daemon's root phase uses it for the devices whose drivers
/// take a step before the drop.
///
/// # Errors
///
/// The device's settings can't be read as values or lists; the text names why.
pub fn with_device_config<R>(
    board: &BoardFile,
    spec: &DeviceSpec,
    f: impl FnOnce(&Config<'_>) -> R,
) -> core::result::Result<R, String> {
    let owned = spec.owned_config().map_err(|error| error.to_string())?;
    let entries: Vec<(&str, ConfigValue<'_>)> = owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_value()))
        .collect();
    let lists = spec.owned_lists().map_err(|error| error.to_string())?;
    let refs: Vec<(&str, Vec<&str>)> = lists
        .iter()
        .map(|(key, items)| (key.as_str(), items.iter().map(String::as_str).collect()))
        .collect();
    let borrowed: Vec<(&str, &[&str])> = refs
        .iter()
        .map(|(key, items)| (*key, items.as_slice()))
        .collect();
    let config = Config::new(&entries)
        .with_lists(&borrowed)
        .on_board(&board.board.id);
    Ok(f(&config))
}

/// The first `uses` entry of the device that its driver does not take, or that names a device the
/// board does not have, or one that does not serve what the need asks for; `None` when every entry
/// is taken. Known from each driver's manifest and the board file alone, with no hardware.
///
/// A name a driver takes may also be a bus or a block, which only the backend knows (the host never
/// learns what hardware is), so a need that is not a device is not checked against the board here: if
/// the name is wrong, the bind fails and says so. `found` is the key an `[[each]]` entry fills with
/// the resource it found, whose value is a pattern and not a device id.
fn uses_refusal(
    board: &BoardFile,
    spec: &DeviceSpec,
    entry: &DriverEntry,
    drivers: &[DriverEntry],
    found: Option<&str>,
) -> Option<String> {
    let needs = entry.manifest.needs;
    for (key, value) in &spec.uses {
        let Some(need) = needs.iter().find(|need| need.name == key) else {
            let takes = if needs.is_empty() {
                "it takes none".to_string()
            } else {
                let names: Vec<String> = needs.iter().map(|n| format!("{:?}", n.name)).collect();
                format!("it takes: {}", names.join(", "))
            };
            return Some(format!(
                "`uses` key {key:?} is not a resource this driver takes ({takes})"
            ));
        };
        let NeedKind::Device(tag) = need.kind else {
            continue;
        };
        if found == Some(key.as_str()) {
            continue;
        }
        let Some(target) = device_driver(board, value) else {
            return Some(format!(
                "`uses` key {key:?} names device {value:?}, which the board does not have"
            ));
        };
        let serves = drivers
            .iter()
            .rev()
            .find(|driver| driver.manifest.name == target)
            .is_none_or(|driver| driver.manifest.provides.iter().any(|c| c.tag == tag));
        if !serves {
            return Some(format!(
                "`uses` key {key:?} names device {value:?}, which does not serve {}",
                tag.as_str()
            ));
        }
    }
    None
}

/// The driver of the device `id` names: a device of the board, or one an `[[each]]` entry makes
/// (its id before the `@`). `None` when the board has no such device.
fn device_driver<'a>(board: &'a BoardFile, id: &str) -> Option<&'a str> {
    if let Some(device) = board.devices.iter().find(|device| device.id == id) {
        return Some(&device.driver);
    }
    board
        .each
        .iter()
        .find(|each| id.starts_with(&format!("{}{EACH_SEPARATOR}", each.id)))
        .map(|each| each.driver.as_str())
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
        check: D::check_config,
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
    /// An off request reached a driver that keeps its hardware (`Driver::HOLDS_OFF`), and its
    /// `off` returned this error. The instance was dropped, as an off always drops it.
    Failed(Status),
}

/// Where a device stands. Every state is visible to anyone who asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Running,
    /// Waiting to retry. Clears itself when a retry works. `reason` is the sentence; `kind` says
    /// whether the device is absent or failing, and is `None` where no refusal is behind the wait
    /// (a device just switched on).
    Backoff {
        attempts: u32,
        next_retry_ns: u64,
        reason: String,
        kind: Option<RefusalKind>,
    },
    Off(OffReason),
}

/// The last time a device held its streams back (their producer read retrying), kept after it
/// ended, so why a stream stalled can be read after the device recovered: a device's reason is
/// cleared when it works again. A stream's stall reports `since_ns` as its producer state does, on
/// the host's clock, so a reader matches the two exactly. A stall that a linked device caused is
/// that device's (see `docs/streams.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    /// When it began, on the host's clock.
    pub since_ns: u64,
    /// When it ended: `None` while it stands.
    pub ended_ns: Option<u64>,
    /// Why: the driver's failed health, or why the host waits to bind it again.
    pub reason: Cow<'static, str>,
}

/// A device as seen from outside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub index: u32,
    pub id: String,
    /// The board's `label`, or the id when the board names none.
    pub label: String,
    /// The bus or block the board names for the device, as written.
    pub bus: Option<String>,
    pub driver: String,
    pub tags: Vec<Tag>,
    pub state: DeviceState,
    pub health: Health,
    /// The figures the driver measured for its health reason, shown after it (see [`Figures`]).
    /// Empty unless the device is failed or degraded and its driver is not in a call.
    pub figures: Figures,
    /// Set while a running driver retries a failed step itself (see [`Retry`]). A device the host
    /// backs off keeps its attempts in [`DeviceState::Backoff`] instead.
    pub retry: Option<Retry>,
    /// The device this one is wired to, by the id the board file's `link` names. The host
    /// hands it on as written and never reads it: what the wiring means is the devices'.
    pub link: Option<String>,
    /// Goes up with every change to this device's state, after the change, so a reader that
    /// saw an earlier revision knows what it holds is out of date. It never goes back. Health is
    /// not part of it: see [`Host::look`], which is how a change of health is found.
    pub revision: u64,
    /// The device's last stall, standing or ended. `None` until its streams first stall.
    pub last_stall: Option<Stall>,
}

/// Whether the device's driver needs another device (a `uses` entry), whose health its own health
/// is combined with.
fn has_device_needs(slot: &Slot) -> bool {
    slot.entry.is_some_and(|entry| {
        entry
            .manifest
            .needs
            .iter()
            .any(|need| matches!(need.kind, NeedKind::Device(_)))
    })
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
    /// What the last rebind kept for the next instance (see [`Inner::rebind`]). Held from the
    /// rebind until a bind takes it up or the device is switched off or on.
    resume: Mutex<Option<ResumeRecord>>,
    state: Mutex<DeviceState>,
    /// Goes up after each change to `state`. See [`DeviceInfo::revision`].
    revision: AtomicU64,
    /// A call into this device never came back.
    hung: AtomicBool,
    /// The driver is in its tick (with what the host asks of it around the tick): set as the tick
    /// starts and cleared as it ends, and cleared as other work starts. A deadline's line says
    /// whether the work it abandoned was in the tick a call or the host's work runs after itself.
    in_tick: AtomicBool,
    errors: AtomicU32,
    /// Optional dependencies that were absent at the last bind.
    missing: Mutex<Vec<String>>,
    /// Why the last bind was refused for its link: the linked device does not serve the contract
    /// the driver links to. Set by the link's resolution, and reported as the failed bind's reason.
    link_refusal: Mutex<Option<String>>,
    /// The reason a driver gave for refusing its last bind, with its kind (see [`Config::refuse`]).
    bind_refusal: Mutex<Option<(RefusalKind, String)>>,
    /// When the driver next wants a tick. [`NO_TICK`] when it doesn't.
    next_tick_ns: AtomicU64,
    /// When a bind was last tried. A hint never brings on another sooner than
    /// [`Options::hint_gap`] after it.
    tried_ns: AtomicU64,
    /// When the driver last bound.
    bound_ns: AtomicU64,
    /// When this bind's device first ran with health Ok, on the host's clock, plus one: zero
    /// until this bind stamps it, and each bind sets it back to zero. The first stamp wins. See
    /// [`Inner::set_running_ok`].
    running_ok_ns: AtomicU64,
    /// Whether this bind's first tick has been said to the start trace (see [`set_start_trace`]).
    first_tick_traced: AtomicBool,
    /// Runs in a row that ended sooner than the retry cap after their bind. A driver that fails
    /// straight after each bind waits longer each time, as a bind that fails does.
    short_runs: AtomicU32,
    /// The driver's own health last said failed (its streams read retrying for that reason), so a
    /// tick only tells the streams when it changes.
    stalled: AtomicBool,
    /// The last stall of this device's streams, kept after it ends (see [`Stall`]).
    last_stall: Mutex<Option<Stall>>,
    /// A hint has named this running device and its driver has not been ticked since.
    hinted: AtomicBool,
    /// The driver's wait descriptor, as the watcher reads it. See [`Inner::watch_wait`].
    wait: Mutex<Option<Arc<File>>>,
    /// Goes up when the watcher must end: its descriptor was replaced, or the driver went.
    wait_gen: AtomicU64,
    /// A wake of the wait descriptor came in: a tick that is running sets its next tick to now as well.
    kicked: AtomicBool,
    /// The `[[each]]` entry that made this device, by its place in the board file, and the name
    /// of the resource it was made for. `None` for a device of the board file's own.
    made: Option<(usize, String)>,
    /// Why the driver refuses this device's settings, as its state says. A refused device is
    /// never made: the host records this as its failed bind, and retries it under the failure rule.
    refusal: Option<String>,
    /// Long jobs of this device's driver that are still open.
    open_jobs: AtomicU32,
    /// The END descriptor of the hold the caller has, if it was lent with one. See [`EndHold`].
    end: Mutex<Option<EndHold>>,
    /// The caller an END hold was lent to and has not given up, for a switch-off to wait on (see
    /// [`Host::off`]), with the condition its release signals.
    lent_to: Mutex<Option<CallerId>>,
    released: Condvar,
    /// The instance a rebind let go of when it parked what the holder of its endpoints uses
    /// ([`Driver::rebinding`]): kept until the new instance's first tick has run, then dropped and
    /// the holder's pair lent to the new one. Never ticked or called while kept.
    kept: Mutex<Option<Kept>>,
    /// The words of each caller's last refused call into this device ([`Driver::refusal`]),
    /// newest last, at most [`MAX_REFUSALS`]. Taken by [`Host::take_refusal`].
    refusals: Mutex<Vec<(CallerId, String)>>,
    /// `refusals` has an entry: a call clears its caller's words only then.
    refused: AtomicBool,
    /// The callers that have called this device and have not gone, for [`Router::others_use`]. A
    /// full set holds at least `MAX_USERS - 1` callers, so a caller that does not fit is not kept:
    /// the answer for any asker is the same.
    users: Mutex<[Option<CallerId>; MAX_USERS]>,
}

/// The END descriptor of a hold: a pipe whose write end the host keeps for the hold. Each copy
/// of the read end a caller gets is readable (EOF, a hang-up) once the write end is dropped,
/// which is how the host tells the holder the hold is over for good.
struct EndHold {
    caller: CallerId,
    read: OwnedFd,
    /// Kept open, and never written to: dropping it is what ends the hold for the holder.
    _write: OwnedFd,
}

/// How long a switch-off waits for the holder of an END hold to give it up once END is closed.
const RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
/// The most bytes of words a refusal carries ([`Driver::refusal`]).
pub const REFUSAL_WORDS: usize = 256;
/// The most callers whose last refusal's words a device keeps.
const MAX_REFUSALS: usize = 8;

/// An instance kept across a rebind, and the holder whose hold it kept.
struct Kept {
    /// Held for its drop: it is never ticked or called while kept.
    _instance: Box<dyn Instance>,
    holder: CallerId,
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

/// The instance of `slot`, once the tick or call holding it lets go. A teardown (a switch-off, or
/// the host's shutdown) must not be skipped because a tick is running, so it waits. A call that
/// never returns holds the instance for good; its deadline marks the device hung, and then there is
/// nothing to wait for, so `None`. Polled, so a teardown waits no longer than the call it waits on.
fn instance_for_teardown(slot: &Slot) -> Option<MutexGuard<'_, Option<Box<dyn Instance>>>> {
    loop {
        match slot.instance.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if slot.hung.load(Ordering::Acquire) {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

/// The instance of `slot` for a call or the host's work on its lane, or `None` once the device is
/// hung. Uncontended, as a lane runs one thing at a time; held only by a teardown or rebind (brief)
/// or by a call abandoned at its deadline, which marks the device hung. Polled, so work queued on a
/// new lane behind an abandoned call is refused within a millisecond of the mark instead of
/// waiting out a deadline of its own.
fn instance_unless_hung(slot: &Slot) -> Option<MutexGuard<'_, Option<Box<dyn Instance>>>> {
    loop {
        match slot.instance.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if slot.hung.load(Ordering::Acquire) {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

/// What ran on a device's lane, for the line its deadline writes.
#[derive(Debug, Clone, Copy)]
enum Ran {
    /// The driver's tick.
    Tick,
    /// A method of the driver's contracts, for a caller.
    Call { method: MethodId, caller: CallerId },
    /// The host's own work on the driver for a caller, `what` naming it (`describe`, `caller_gone`).
    Work {
        what: &'static str,
        caller: CallerId,
    },
}

/// How many distinct callers a device keeps track of for [`Router::others_use`].
const MAX_USERS: usize = 16;

/// Whether a driver that provides these contracts is given the board's trust root.
fn takes_trust_root(provides: &[&str]) -> bool {
    ["system.security/", "system.slots/", "system.flash/"]
        .iter()
        .any(|prefix| provides.iter().any(|tag| tag.starts_with(prefix)))
}

/// The board file's way back (`[boot] loop_return`), as the device config it is given as: to every device that serves
/// `system.reboot/`, which offers the restart into it (`reboot_to`), and to no other. A device that names it itself is
/// refused, so the board has one way back, the one slot-init also reads.
pub(crate) const LOOP_RETURN: &str = "loop_return";

/// One trust root per board. The `[security]` facts go to the device that serves `system.security/1`,
/// and to no other. The trust root's key file (`[security] keys`) also goes to the devices that serve
/// `system.slots/` and `system.flash/`: the keys that verify a slot bundle or a flash image are the keys
/// that verify an authorization to turn the switch off. A device that names `keys` itself is refused, so
/// there is no second key list.
fn add_security_facts(
    id: &str,
    config: &mut Vec<(String, OwnedValue)>,
    provides: &[&str],
    security: &[(String, OwnedValue)],
) -> core::result::Result<(), BoardError> {
    let serves_security = provides
        .iter()
        .any(|tag| tag.starts_with("system.security/"));
    let takes_keys = takes_trust_root(provides);
    let restarts = provides.iter().any(|tag| tag.starts_with("system.reboot/"));
    if takes_keys && config.iter().any(|(own, _)| own == "keys") {
        return Err(BoardError::Config(id.to_string(), "keys".to_string()));
    }
    if restarts && config.iter().any(|(own, _)| own == LOOP_RETURN) {
        return Err(BoardError::Config(id.to_string(), LOOP_RETURN.to_string()));
    }
    for (key, value) in security {
        let given = if key == LOOP_RETURN {
            restarts
        } else {
            serves_security || (takes_keys && key == "keys")
        };
        if !given {
            continue;
        }
        if config.iter().any(|(own, _)| own == key) {
            return Err(BoardError::Config(id.to_string(), key.clone()));
        }
        config.push((key.clone(), value.clone()));
    }
    Ok(())
}

impl Slot {
    /// Ends the watcher of the wait descriptor, if any: its file is written so the watcher's read
    /// returns, and it sees its generation is gone. Called whenever the driver goes.
    fn stop_wait(&self) {
        self.wait_gen.fetch_add(1, Ordering::AcqRel);
        if let Some(file) = lock(&self.wait).take() {
            let _ = (&*file).write(&1u64.to_ne_bytes());
        }
    }

    /// `caller` has called this device, so it is a user until it goes.
    fn note_user(&self, caller: CallerId) {
        let mut users = lock(&self.users);
        if users.contains(&Some(caller)) {
            return;
        }
        if let Some(free) = users.iter_mut().find(|user| user.is_none()) {
            *free = Some(caller);
        }
    }

    /// `caller` has gone: it is no longer a user of this device.
    fn forget_user(&self, caller: CallerId) {
        for user in lock(&self.users).iter_mut() {
            if *user == Some(caller) {
                *user = None;
            }
        }
    }

    /// Whether a caller other than `caller` is a user of this device.
    fn others_use(&self, caller: CallerId) -> bool {
        lock(&self.users)
            .iter()
            .flatten()
            .any(|user| *user != caller)
    }

    /// The reason a link to this device is refused when it does not serve `tag`: what the link
    /// needs, what this device serves, and where the link goes.
    fn serves_message(&self, id: &str, tag: Tag) -> String {
        let served: Vec<&str> = self
            .entry
            .map(|entry| {
                entry
                    .manifest
                    .provides
                    .iter()
                    .map(|contract| contract.tag.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let serves = if served.is_empty() {
            "no contract".to_string()
        } else {
            served.join(", ")
        };
        format!(
            "the link to `{id}` needs a {} device, but `{id}` serves {serves}: a sensor links to \
             a camera.receiver/1 device, and a receiver and each ISP to a camera.isp/1 device",
            tag.as_str()
        )
    }

    /// A device as it starts: waiting for its first bind, or off when the board says so.
    fn new(
        index: u32,
        spec: DeviceSpec,
        entry: Option<DriverEntry>,
        made: Option<(usize, String)>,
        refusal: Option<String>,
        security: &[(String, OwnedValue)],
    ) -> core::result::Result<Self, BoardError> {
        // A refused device is never bound, so the settings it cannot read are not needed.
        let refused = refusal.is_some();
        let mut config = spec
            .owned_config()
            .or_else(|error| if refused { Ok(Vec::new()) } else { Err(error) })?;
        let lists = spec
            .owned_lists()
            .or_else(|error| if refused { Ok(Vec::new()) } else { Err(error) })?;
        let state = if spec.disabled {
            DeviceState::Off(OffReason::Disabled)
        } else {
            DeviceState::Backoff {
                attempts: 0,
                next_retry_ns: 0,
                reason: "starting".to_string(),
                kind: None,
            }
        };
        let tags: Vec<&str> = entry
            .iter()
            .flat_map(|e| e.manifest.provides.iter().map(|c| c.tag.as_str()))
            .collect();
        add_security_facts(&spec.id, &mut config, &tags, security)?;
        Ok(Self {
            index,
            lane_key: match &spec.bus {
                Some(bus) => format!("bus:{bus}"),
                None => format!("device:{}", spec.id),
            },
            config,
            lists,
            entry,
            refusal,
            spec,
            instance: Mutex::new(None),
            resume: Mutex::new(None),
            state: Mutex::new(state),
            revision: AtomicU64::new(0),
            hung: AtomicBool::new(false),
            in_tick: AtomicBool::new(false),
            end: Mutex::new(None),
            lent_to: Mutex::new(None),
            released: Condvar::new(),
            kept: Mutex::new(None),
            refusals: Mutex::new(Vec::new()),
            refused: AtomicBool::new(false),
            errors: AtomicU32::new(0),
            missing: Mutex::new(Vec::new()),
            link_refusal: Mutex::new(None),
            bind_refusal: Mutex::new(None),
            next_tick_ns: AtomicU64::new(NO_TICK),
            tried_ns: AtomicU64::new(0),
            bound_ns: AtomicU64::new(0),
            running_ok_ns: AtomicU64::new(0),
            first_tick_traced: AtomicBool::new(false),
            short_runs: AtomicU32::new(0),
            stalled: AtomicBool::new(false),
            last_stall: Mutex::new(None),
            hinted: AtomicBool::new(false),
            wait: Mutex::new(None),
            wait_gen: AtomicU64::new(0),
            kicked: AtomicBool::new(false),
            made,
            open_jobs: AtomicU32::new(0),
            users: Mutex::new([None; MAX_USERS]),
        })
    }

    fn is_running(&self) -> bool {
        matches!(*lock(&self.state), DeviceState::Running)
    }

    fn ticks(&self) -> bool {
        self.entry.is_some_and(|entry| entry.ticks)
    }

    /// A copy of the END descriptor for `caller`'s hold, made on the first ask and shared after.
    /// `BUSY` while another caller's hold has one.
    fn end_for(&self, caller: CallerId) -> Result<OwnedFd> {
        let mut held = lock(&self.end);
        if let Some(hold) = held.as_ref() {
            if hold.caller != caller {
                return Err(Status::BUSY);
            }
            return hold.read.try_clone().map_err(|_| Status::IO);
        }
        let (read, write) = std::io::pipe().map_err(|_| Status::IO)?;
        let read = OwnedFd::from(read);
        let copy = read.try_clone().map_err(|_| Status::IO)?;
        *held = Some(EndHold {
            caller,
            read,
            _write: OwnedFd::from(write),
        });
        Ok(copy)
    }

    /// Ends the hold END belongs to, if `caller` has one: the write end goes, so every copy of
    /// END reads EOF.
    fn end_close_for(&self, caller: CallerId) {
        let mut held = lock(&self.end);
        if held.as_ref().is_some_and(|hold| hold.caller == caller) {
            *held = None;
        }
        drop(held);
        self.holder_released(caller);
    }

    /// `caller` gave up its hold, or went: a switch-off waiting on it goes on.
    fn holder_released(&self, caller: CallerId) {
        let mut lent = lock(&self.lent_to);
        if *lent == Some(caller) {
            *lent = None;
            self.released.notify_all();
        }
    }

    /// Waits at most `within` for the holder of an END hold to give it up: the holder's copies of
    /// the pair are its own to close before the function goes.
    fn wait_for_release(&self, within: std::time::Duration) {
        let lent = lock(&self.lent_to);
        let (mut lent, _) = self
            .released
            .wait_timeout_while(lent, within, |lent| lent.is_some())
            .unwrap_or_else(PoisonError::into_inner);
        *lent = None;
    }

    /// The instance kept across a rebind, let go of: the bind failed, or the device is switched
    /// off or shut down before the new instance's first tick. What it kept for the next instance
    /// and nothing took up goes with it, and so does the hold.
    fn drop_kept(&self, why: &str) {
        let Some(kept) = lock(&self.kept).take() else {
            return;
        };
        warn(|| {
            format!(
                "device {} (driver {}): the instance kept across the rebind is let go ({why}): what it kept for the holder's link is taken down, and the hold ends",
                self.spec.id, self.spec.driver
            )
        });
        self.end_close();
        drop(catch_unwind(AssertUnwindSafe(move || drop(kept))));
    }

    /// Forgets `caller`'s refusal words: a new call of its is under way.
    fn clear_refusal(&self, caller: CallerId) {
        if self.refused.load(Ordering::Acquire) {
            let mut refusals = lock(&self.refusals);
            refusals.retain(|(who, _)| *who != caller);
            self.refused.store(!refusals.is_empty(), Ordering::Release);
        }
    }

    /// Keeps `words` as `caller`'s last refusal's.
    fn keep_refusal(&self, caller: CallerId, words: String) {
        let mut refusals = lock(&self.refusals);
        refusals.retain(|(who, _)| *who != caller);
        if refusals.len() >= MAX_REFUSALS {
            refusals.remove(0);
        }
        refusals.push((caller, words));
        self.refused.store(true, Ordering::Release);
    }

    /// Ends whatever hold has an END descriptor, whoever has it.
    fn end_close(&self) {
        *lock(&self.end) = None;
    }

    fn tracks_callers(&self) -> bool {
        self.entry.is_some_and(|entry| entry.tracks_callers)
    }

    /// `method` as its contract names it, `contract.method`, for a line about the call. A method
    /// the driver's contracts don't have is given by its numbers.
    fn method_text(&self, method: MethodId) -> String {
        let named = self.entry.and_then(|entry| {
            let contract = entry.manifest.provides.get(usize::from(method.contract))?;
            let info = contract.methods.iter().find(|m| m.id == method.method)?;
            Some(format!("{}.{}", contract.tag.name(), info.name))
        });
        named.unwrap_or_else(|| format!("method {} of contract {}", method.method, method.contract))
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

    /// The stall bound a job method declares, in seconds; `None` for a method the driver's
    /// contracts don't have. Zero for a method that is not a job.
    fn job_stall_secs(&self, method: MethodId) -> Option<u32> {
        let contract = self
            .entry?
            .manifest
            .provides
            .get(usize::from(method.contract))?;
        contract
            .methods
            .iter()
            .find(|m| m.id == method.method)
            .map(|m| m.stall_secs)
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
    /// The refusal of the entry's settings, which every device it makes carries.
    refusal: Option<String>,
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
    /// The board's `[security]` facts, given to the device that serves `system.security/1`.
    security: Vec<(String, OwnedValue)>,
    /// Called when a device's next tick moves earlier.
    waker: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// The daemon's liveness, which gates the ticks of keep-alive devices. Unset, the gate is
    /// closed: nothing is kept alive until the daemon says it is healthy.
    liveness: OnceLock<Arc<dyn Liveness>>,
    streams: Streams,
    /// How a host-owned device's driver calls the devices it uses: reads only.
    reading: OnceLock<&'static Reading>,
    /// How a device that is not host-owned calls another device it uses, and starts its jobs.
    routes: OnceLock<&'static Routes>,
    /// Long jobs still open, and what their starters haven't been told yet.
    jobs: Mutex<Jobs>,
    /// The names callers gave themselves ([`Host::name_caller`]), until they go: what a line
    /// about a call calls its caller by.
    caller_names: Mutex<Vec<(CallerId, String)>>,
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
    /// success; otherwise the failure's words, UTF-8 at most [`REFUSAL_WORDS`] bytes, or empty
    /// when the driver gave none. A job its driver lost (a panic, a rebind, the device switched
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

/// A long job open on a device, as the host's job table has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobInfo {
    /// The host's number for the job, as [`Host::start_job`] returned it.
    pub job: u64,
    /// The last progress the driver gave, in the contract's unit; both 0 until it gives one.
    pub done: u64,
    pub total: u64,
}

struct OpenJob {
    id: u64,
    device: u32,
    driver_job: JobId,
    owner: CallerId,
    /// The last progress passed on, if any was.
    told: Option<(u64, u64)>,
    /// How long the job may go without progress, in nanoseconds: the bound its method declares.
    stall_ns: u64,
    /// When the job is over its bound, on the host's clock, unless progress renews it first. Each
    /// progress report moves it to a stall bound after that report.
    due_ns: u64,
}

struct Jobs {
    /// The high half of every job number this host gives out: different for each host, so a
    /// number from before a restart is told from one never given.
    epoch: u32,
    next: u32,
    open: Vec<OpenJob>,
    updates: VecDeque<JobUpdate>,
    /// What changed about jobs a device started on another device (owned by [`CallerId::device`]).
    /// Read by [`Router::job_poll`], never by [`Host::take_job_updates`].
    local: VecDeque<JobUpdate>,
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
            local: VecDeque::new(),
        }
    }

    /// Queues an update. A job's progress that hasn't gone out yet is replaced by newer
    /// progress, so the queue holds at most two updates per job.
    fn push(&mut self, update: JobUpdate) {
        let queue = if update.owner.is_device() {
            &mut self.local
        } else {
            &mut self.updates
        };
        if let JobEvent::Progress { .. } = update.event
            && let Some(queued) = queue
                .iter_mut()
                .rev()
                .find(|queued| queued.job == update.job)
            && matches!(queued.event, JobEvent::Progress { .. })
        {
            queued.event = update.event;
            return;
        }
        queue.push_back(update);
    }
}

/// The way from a host-owned device's driver to another device. Only a method the other
/// device's description names as a value gets through, whoever owns that device: a driver that
/// may change nothing changes nothing through another driver either.
struct Reading(&'static Inner);

impl Router for Reading {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let slot = self.0.slot(device).ok_or(Status::NOT_FOUND)?;
        self.0.call_from(
            &slot,
            CallerId::LOCAL,
            true,
            false,
            method,
            args,
            reply,
            None,
        )
    }

    fn health(&self, device: u32) -> Health {
        Router::health(self.0, device)
    }
}

/// The way from a device that is not host-owned to the devices it uses: calls as the caller it
/// names, and the long jobs it starts there. The host keeps one for its lifetime.
struct Routes(&'static Inner);

impl Router for Routes {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        Router::call(self.0, device, method, args, reply)
    }

    fn health(&self, device: u32) -> Health {
        Router::health(self.0, device)
    }

    fn call_as(
        &self,
        caller: CallerId,
        device: u32,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        if caller == CallerId::LOCAL {
            return Router::call(self.0, device, method, args, reply);
        }
        let slot = self.0.slot(device).ok_or(Status::NOT_FOUND)?;
        self.0.call_here(&slot, caller, false, method, args, reply)
    }

    fn start_job(
        &self,
        caller: CallerId,
        device: u32,
        method: MethodId,
        args: &[u8],
    ) -> Result<u64> {
        let slot = self.0.slot(device).ok_or(Status::NOT_FOUND)?;
        let mut reply = [0u8; 8];
        let n = self
            .0
            .call_here(&slot, caller, true, method, args, &mut reply)?;
        u64::decode(&mut Reader::new(&reply[..n]))
    }

    fn job_poll(&self, caller: CallerId, job: u64, result: &mut [u8]) -> Result<Polled> {
        self.0.job_poll_for(caller, job, result)
    }

    fn cancel_job(&self, caller: CallerId, job: u64) -> Result<()> {
        self.0.cancel_for(caller, job)
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
    trace: Option<CallTrace>,
}

impl CallWork {
    pub(crate) fn run(self, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let result = self.inner.call_from(
            &self.slot,
            self.caller,
            false,
            self.as_job,
            self.method,
            args,
            reply,
            self.trace.as_ref(),
        );
        if let Some(trace) = &self.trace {
            trace.sink.finished(trace.ticket, status_of(&result));
        }
        result
    }
}

/// What became of one call, as the caller's [`CallSink`] hears it.
fn status_of(result: &Result<usize>) -> Status {
    match result {
        Ok(_) => Status::OK,
        Err(status) => *status,
    }
}

/// Reports a call the lane never ran (or never could): it is over with `status`, and it did not apply.
fn report_refused(trace: Option<&CallTrace>, status: Status) {
    if let Some(trace) = trace {
        trace.sink.finished(trace.ticket, status);
    }
}

/// Told how a traced call goes (see [`Host::call_traced`]). Both are called on the call's lane, so a
/// sink must not wait on the caller. The host never reports a call it started and then abandoned
/// at its deadline: the driver's method is still running, and the sink hears `finished` when it
/// returns, which can be after the caller has its timeout.
pub trait CallSink: Send + Sync {
    /// The driver's method is about to run for the call `ticket` names.
    fn started(&self, ticket: u64);
    /// The call is over with `status`: the status the driver's method returned, or the refusal that
    /// stopped the call before its method ran. Which of the two it was is for the sink to keep, from
    /// whether `started` came first.
    fn finished(&self, ticket: u64, status: Status);
}

/// A call's trace: where to report it, and the number the caller gave it.
#[derive(Clone)]
pub struct CallTrace {
    sink: Arc<dyn CallSink>,
    ticket: u64,
}

impl CallTrace {
    /// The trace of the call `ticket` names, reported to `sink`.
    pub fn new(sink: Arc<dyn CallSink>, ticket: u64) -> Self {
        Self { sink, ticket }
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
/// Whether the device is off. Decided by its state, which `off` sets first.
fn is_off(slot: &Slot) -> bool {
    matches!(*lock(&slot.state), DeviceState::Off(_))
}

/// What a bind refuses in a driver's description, named: two items with one name (a client finds
/// an item by its name, so the second could never be reached), or a real value with a nonzero
/// exponent. A real value is already in its unit, so its exponent is 0; an integer item's exponent
/// is its scale (`raw * 10^exponent`).
///
/// A value is real when the description says so (a real range, step or limit), or when the contract
/// says its method returns a float: the manifest's contracts carry every method's return shape, so
/// the host knows at bind, before any read, what an item reads. An item whose contract the manifest
/// does not provide is judged by its description alone.
fn describe_fault(instance: &dyn Instance, manifest: &Manifest) -> Option<String> {
    let mut description = Description::new();
    instance.describe(&mut description);
    let items: Vec<&Item> = description.items().collect();
    for (at, item) in items.iter().enumerate() {
        if items[..at].iter().any(|earlier| earlier.name == item.name) {
            return Some(format!("item {}: two items have this name", item.name));
        }
    }
    items
        .into_iter()
        .find(|item| {
            item.exponent != 0
                && (item.real_min.is_some()
                    || item.real_max.is_some()
                    || item.real_step.is_some()
                    || item.real_warn_low.is_some()
                    || item.real_warn_high.is_some()
                    || reads_a_float(manifest, item))
        })
        .map(|item| {
            format!(
                "item {}: a real value carries exponent 0 (it has exponent {})",
                item.name, item.exponent
            )
        })
}

/// Whether the item's value is a float, by the shape its contract gives its method: the whole reply
/// when the item has no field, else the named field of the reply record. Looks through an option or
/// an array to its element.
fn reads_a_float(manifest: &Manifest, item: &Item) -> bool {
    let Some(contract) = manifest.provides.get(usize::from(item.method.contract)) else {
        return false;
    };
    let Some(method) = contract
        .methods
        .iter()
        .find(|method| method.id == item.method.method)
    else {
        return false;
    };
    shape_is_float(&method.returns, item.field)
}

fn shape_is_float(shape: &Shape, field: Option<&str>) -> bool {
    match (shape, field) {
        (Shape::Record(fields), Some(name)) => fields
            .iter()
            .find(|found| found.name == name)
            .is_some_and(|found| shape_is_float(&found.shape, None)),
        (Shape::F32 | Shape::F64, None) => true,
        (Shape::Option(inner) | Shape::Array(inner, _), None) => shape_is_float(inner, None),
        _ => false,
    }
}

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

/// Where the start trace goes: set once by the daemon when asked for (see [`set_start_trace`]).
static START_TRACE: OnceLock<fn(&str)> = OnceLock::new();

/// Where the host's warnings go: a call past its deadline, a job that ends interrupted, a rebind
/// refused because the old instance is still in a call, and that instance released. Off unless set.
static WARNING_SINK: OnceLock<fn(&str)> = OnceLock::new();

/// Sends the host's warnings to `sink`. The daemon sets it, so each is a journal line at warning
/// priority. Set once; a second setting is ignored.
pub fn set_warning_sink(sink: fn(&str)) {
    let _ = WARNING_SINK.set(sink);
}

/// The words a failed job ends with: the driver's bytes when they are UTF-8, cut to
/// [`REFUSAL_WORDS`] at a character boundary; nothing otherwise.
fn failed_job_words(written: &[u8]) -> Vec<u8> {
    let Ok(text) = core::str::from_utf8(written) else {
        return Vec::new();
    };
    let mut end = text.len().min(REFUSAL_WORDS);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.as_bytes()[..end].to_vec()
}

/// One warning, made only when a sink is set.
fn warn(event: impl FnOnce() -> String) {
    if let Some(sink) = WARNING_SINK.get() {
        sink(&event());
    }
}

/// The job ids as the warnings name them: `0x…` each, or `none`.
fn jobs_text(jobs: &[u64]) -> String {
    if jobs.is_empty() {
        return "none".into();
    }
    jobs.iter()
        .map(|id| format!("{id:#x}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Sends the host's start events to `sink`: each device's bind as it begins and ends, and its first
/// tick after each bind. The daemon stamps them; a board run and the QEMU watchdog check read the
/// gaps between them (how long after the start the watchdog is first fed). Off unless set. A
/// second call is ignored.
pub fn set_start_trace(sink: fn(&str)) {
    let _ = START_TRACE.set(sink);
}

/// One start event, made only when the trace is on.
fn start_trace(event: impl FnOnce() -> String) {
    if let Some(sink) = START_TRACE.get() {
        sink(&event());
    }
}

/// A stall's reason when a device switched off or on by hand lets go of what a rebind kept.
const LET_GO: &str = "switched off or on by hand: what the last instance kept was let go";

/// A stall's reason when a new instance did not take up what the last one kept.
const NOT_TAKEN_UP: &str = "the new instance did not take up what the last one kept";

/// Why the driver wants to be bound again, if it says what it was bound with is dead.
fn asks_rebind(instance: &dyn Instance) -> Option<&'static str> {
    match instance.health() {
        Health::Gone(reason) => Some(reason),
        _ => None,
    }
}

impl Inner {
    /// Lets go of what a rebind kept, when the device is switched off or on by hand: the next
    /// instance is a fresh start, so a stall that was waiting on the record reads lost.
    fn forget_record(&self, slot: &Slot) {
        if lock(&slot.resume).take().is_some() {
            self.stall(slot, true, || Cow::Borrowed(LET_GO));
        }
    }

    /// The streams of `slot`'s device stall from now, and of each device that links to it: a
    /// stall that already stands keeps its start and its reason, as the streams keep theirs.
    /// `why` is only asked for a new stall. `lost` is as `Streams::producer_failed` takes it.
    fn stall(&self, slot: &Slot, lost: bool, why: impl FnOnce() -> Cow<'static, str>) {
        let now = self.clock.now_ns();
        {
            let mut last = lock(&slot.last_stall);
            if !last.as_ref().is_some_and(|stall| stall.ended_ns.is_none()) {
                *last = Some(Stall {
                    since_ns: now,
                    ended_ns: None,
                    reason: why(),
                });
            }
        }
        self.streams.producer_failed(slot.index, now, lost);
    }

    /// `slot`'s device works again: its stall, if one stands, ends now and is kept.
    fn unstall(&self, slot: &Slot) {
        if let Some(stall) = lock(&slot.last_stall).as_mut()
            && stall.ended_ns.is_none()
        {
            stall.ended_ns = Some(self.clock.now_ns());
        }
        self.streams.producer_back(slot.index);
    }

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
        self.end_jobs_because(slot, status, &[]);
    }

    /// `end_jobs` with the reason the device was lost, as UTF-8 in each job's end result:
    /// the words the caller and the daemon's log show, rather than an empty result.
    fn end_jobs_because(&self, slot: &Slot, status: Status, reason: &[u8]) {
        self.end_started(slot);
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
                    result: reason.to_vec(),
                },
            });
        }
        drop(jobs);
        self.wake();
    }

    /// Ends the jobs a device started on other devices, now its driver is gone: each is taken off
    /// the books and cancelled on the device it runs on, and what was queued for the starter is
    /// dropped. Nothing is queued for a starter after this.
    fn end_started(&self, starter: &Slot) {
        let owner = CallerId::device(starter.index);
        let mut cancels = Vec::new();
        {
            let mut jobs = self.jobs();
            jobs.open.retain(|job| {
                if job.owner == owner {
                    cancels.push((job.device, job.driver_job));
                    false
                } else {
                    true
                }
            });
            jobs.local.retain(|update| update.owner != owner);
        }
        for (device, driver_job) in cancels {
            let Some(target) = self.slot(device) else {
                continue;
            };
            target.open_jobs.fetch_sub(1, Ordering::AcqRel);
            // Teardown holds no lock of the starter's, and the device it runs on never waits for
            // its starter, so this takes its instance directly, as a call does.
            let mut guard = lock(&target.instance);
            if let Some(instance) = guard.as_mut() {
                let _ = catch_unwind(AssertUnwindSafe(|| instance.job_cancel(driver_job)));
            }
        }
    }

    /// Takes a job `instance` has just started on `caller`'s behalf, and returns its number.
    fn open_job(&self, slot: &Slot, driver_job: JobId, caller: CallerId, stall_ns: u64) -> u64 {
        let mut jobs = self.jobs();
        jobs.next = jobs.next.wrapping_add(1).max(1);
        let id = (u64::from(jobs.epoch) << 32) | u64::from(jobs.next);
        let due_ns = self.clock.now_ns().saturating_add(stall_ns);
        jobs.open.push(OpenJob {
            id,
            device: slot.index,
            driver_job,
            owner: caller,
            told: None,
            stall_ns,
            due_ns,
        });
        slot.open_jobs.fetch_add(1, Ordering::AcqRel);
        drop(jobs);
        // The device's next look may be later than this job's bound: wake whoever sleeps until it.
        self.wake();
        id
    }

    /// Cancels job `job` for its owner `caller`. See [`Host::cancel_job`].
    fn cancel_for(&'static self, caller: CallerId, job: u64) -> Result<()> {
        let (device, driver_job) = {
            let jobs = self.jobs();
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
        let slot = self.slot(device).ok_or(Status::NOT_FOUND)?;
        let ran = Ran::Work {
            what: "job_cancel",
            caller,
        };
        self.on_lane(slot, ran, move |instance| instance.job_cancel(driver_job));
        Ok(())
    }

    /// How job `job`, a device-owned one, stands for its owner `caller`. See [`Router::job_poll`].
    /// What changed since the last poll is taken from the queue; a finished job's result is copied
    /// only when it fits, so a poll that is too small for it can be repeated.
    fn job_poll_for(&self, caller: CallerId, job: u64, result: &mut [u8]) -> Result<Polled> {
        let mut jobs = self.jobs();
        let open = jobs
            .open
            .iter()
            .find(|open| open.id == job)
            .map(|open| (open.owner, open.told));
        let queued = jobs
            .local
            .iter()
            .find(|update| update.job == job)
            .map(|u| u.owner);
        match (open, queued) {
            (Some((owner, _)), _) | (None, Some(owner)) if owner != caller => {
                return Err(Status::NOT_PERMITTED);
            }
            (None, None) => return Err(Status::NOT_FOUND),
            _ => {}
        }
        if let Some(at) = jobs.local.iter().position(|update| {
            update.job == job && matches!(update.event, JobEvent::Finished { .. })
        }) && let JobEvent::Finished {
            status,
            result: bytes,
        } = &jobs.local[at].event
        {
            if bytes.len() > result.len() {
                return Err(Status::BUFFER_TOO_SMALL);
            }
            let (status, len) = (*status, bytes.len());
            result[..len].copy_from_slice(bytes);
            jobs.local.remove(at);
            // Anything queued after the end is for a job the host has already let go.
            jobs.local.retain(|update| update.job != job);
            return Ok(Polled::Finished { status, len });
        }
        let mut told = open.and_then(|(_, told)| told);
        while let Some(at) = jobs.local.iter().position(|update| update.job == job) {
            if let Some(update) = jobs.local.remove(at)
                && let JobEvent::Progress { done, total } = update.event
            {
                told = Some((done, total));
            }
        }
        match open {
            Some(_) => {
                let (done, total) = told.unwrap_or((0, 0));
                if let Some(at) = jobs.open.iter().position(|open| open.id == job) {
                    jobs.open[at].told = Some((done, total));
                }
                Ok(Polled::Running { done, total })
            }
            None => Err(Status::NOT_FOUND),
        }
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
            let now = self.clock.now_ns();
            let mut jobs = self.jobs();
            let Some(at) = jobs.open.iter().position(|job| job.id == id) else {
                continue;
            };
            match state {
                JobState::Running { done, total } => {
                    let job = &mut jobs.open[at];
                    if job.told == Some((done, total)) {
                        if now < job.due_ns {
                            position += 1;
                            continue;
                        }
                        // Silent past its bound: the host ends it as timed out, and the driver is
                        // told to stop it once the table is let go. The next job takes this position.
                        let job = jobs.open.remove(at);
                        slot.open_jobs.fetch_sub(1, Ordering::AcqRel);
                        jobs.push(JobUpdate {
                            job: id,
                            device: job.device,
                            owner: job.owner,
                            event: JobEvent::Finished {
                                status: Status::TIMED_OUT,
                                result: Vec::new(),
                            },
                        });
                        drop(jobs);
                        let _ = catch_unwind(AssertUnwindSafe(|| instance.job_cancel(driver_job)));
                        changed = true;
                        continue;
                    }
                    // Progress: told, and the bound starts again from now.
                    job.told = Some((done, total));
                    job.due_ns = now.saturating_add(job.stall_ns);
                    let (device, owner) = (job.device, job.owner);
                    jobs.push(JobUpdate {
                        job: id,
                        device,
                        owner,
                        event: JobEvent::Progress { done, total },
                    });
                    position += 1;
                }
                JobState::Finished(status) => {
                    // In order: another lane may be going through its own jobs by position.
                    let job = jobs.open.remove(at);
                    slot.open_jobs.fetch_sub(1, Ordering::AcqRel);
                    // A failed job's bytes are its words: UTF-8 the driver wrote on the failed
                    // Finished, bounded; anything else it left is not passed on.
                    let result = if status.is_ok() {
                        writer.written().to_vec()
                    } else {
                        failed_job_words(writer.written())
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

    /// When the earliest open job of `slot` is over its bound, if it has one the host must look
    /// at. Read from the job table alone. None for a device that is not ticked now (see
    /// [`Inner::may_tick`]): its jobs are looked at when it is.
    fn job_due(&self, slot: &Slot) -> Option<u64> {
        if !slot.runs_jobs() || slot.open_jobs.load(Ordering::Acquire) == 0 || !self.may_tick(slot)
        {
            return None;
        }
        self.jobs()
            .open
            .iter()
            .filter(|job| job.device == slot.index)
            .map(|job| job.due_ns)
            .min()
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
        let mut spec = rule.spec.device(&found);
        let entry = self.drivers.get(spec.driver.as_str()).copied();
        // Checked at start for the template; a made device takes the same kinds.
        if resolve_kinds(
            &spec.driver,
            entry.as_ref().map(|e| &e.manifest),
            &mut spec.streams,
        )
        .is_err()
        {
            return;
        }
        let slot = {
            let mut slots = self.slots.write().unwrap_or_else(PoisonError::into_inner);
            let index = slots.len() as u32;
            // The board file was checked, so the entry's config is good.
            let refusal = self.rules[at].refusal.clone();
            let Ok(slot) = Slot::new(
                index,
                spec,
                entry,
                Some((at, found)),
                refusal,
                &self.security,
            ) else {
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
        slot.stop_wait();
        // Whatever the driver was doing for its callers went with it, and the job's end says why.
        let jobs = self.open_job_ids(slot);
        if !jobs.is_empty() {
            warn(|| {
                format!(
                    "device {} (driver {}): job(s) {} ended interrupted: {reason}",
                    slot.spec.id,
                    slot.spec.driver,
                    jobs_text(&jobs),
                )
            });
        }
        self.end_jobs_because(slot, Status::INTERRUPTED, reason.as_bytes());
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
        // A driver that was running and failed: no refusal behind the wait, so no kind.
        let next = self.next_state(slot, attempts, reason, None);
        *state = next.clone();
        drop(state);
        self.moved(slot);
        self.streams_follow(slot, &next);
    }

    /// What a device's failure means to the consumers of its streams. Retrying: the stream stays
    /// attached and its producer reads as retrying, and frames resume on the same attachment when
    /// the device is back (a bind after a failure is a new driver instance: what the caller gave
    /// the old one is lost). Off for good (`auto_off_after`): the stream ends, device failed.
    fn streams_follow(&self, slot: &Slot, next: &DeviceState) {
        match next {
            DeviceState::Off(_) => {
                // Off for good: nothing a rebind kept survives it.
                *lock(&slot.resume) = None;
                self.streams
                    .end_device(slot.index, lemnos_wire::ring::STATE_DEVICE_FAILED);
            }
            DeviceState::Backoff { reason, .. } => {
                // With a record held, the next bind decides whether the stall is lost: it is
                // `Lost` only if the new instance does not take the record up (see `bind`). Without
                // one, a new instance is a fresh start from the first failure.
                let held = lock(&slot.resume).is_some();
                self.stall(slot, !held, || Cow::Owned(reason.clone()));
            }
            DeviceState::Running => {}
        }
    }

    /// Lets go of a driver that is not in a call, outside its lock, with a panic in its drop
    /// contained.
    fn wake(&self) {
        if let Some(waker) = self.waker.get() {
            waker();
        }
    }

    /// Watches a driver's wait descriptor: each wake of it makes the driver's next tick due now.
    /// One thread per descriptor. It reads the descriptor to clear each wake, so a driver's tick
    /// that runs after it finds nothing left to clear. It ends when its generation goes stale
    /// (see [`Slot::stop_wait`]) or the slot is gone.
    fn watch_wait(&'static self, slot: &Arc<Slot>, fd: OwnedFd) {
        let file = Arc::new(File::from(fd));
        let generation = slot.wait_gen.load(Ordering::Acquire);
        *lock(&slot.wait) = Some(Arc::clone(&file));
        let weak = Arc::downgrade(slot);
        let spawned = std::thread::Builder::new()
            .name("lemnos-wait".into())
            .spawn(move || {
                let mut buf = [0u8; 8];
                loop {
                    match (&*file).read(&mut buf) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                    let Some(slot) = weak.upgrade() else {
                        break;
                    };
                    if slot.wait_gen.load(Ordering::Acquire) != generation {
                        break;
                    }
                    self.kick(&slot);
                }
            });
        // Without a thread the driver still ticks at its deadlines: a fallback, not a failure.
        drop(spawned);
    }

    /// Makes a driver's next tick due now, from its wait descriptor's watcher. Marks the wake, so a
    /// tick that is running at the time sets its next tick to now as well (see [`Inner::settle`]).
    fn kick(&self, slot: &Slot) {
        slot.kicked.store(true, Ordering::SeqCst);
        self.set_next_tick(slot, Some(self.clock.now_ns()));
    }

    /// Records a tick's next time. A wake that came while the tick ran is not lost: it is checked
    /// after the write, and the tick is due at once.
    fn settle(&self, slot: &Slot, next: Option<u64>) {
        self.set_next_tick(slot, next);
        if slot.kicked.swap(false, Ordering::SeqCst) {
            self.set_next_tick(slot, Some(self.clock.now_ns()));
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
    ///
    /// What the driver keeps for its next instance is read first, while the instance is still held
    /// (`Driver::resume_record`), and the next bind takes it up. A driver that panics while it
    /// answers keeps nothing: a panic is a fault, not a rebind.
    ///
    /// An instance that parks what the holder of its endpoints uses ([`Driver::rebinding`]) is
    /// kept, not dropped, and the hold goes on (see [`Inner::keep_for_rebind`]).
    fn rebind(&self, slot: &Slot, mut guard: MutexGuard<'_, Option<Box<dyn Instance>>>, why: &str) {
        let record = guard.as_ref().and_then(|instance| {
            catch_unwind(AssertUnwindSafe(|| instance.resume_record()))
                .ok()
                .flatten()
        });
        *lock(&slot.resume) = record;
        let instance = guard.take();
        drop(guard);
        if let Some(instance) = instance
            && let Some(dead) = self.keep_for_rebind(slot, instance)
        {
            drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
        }
        self.fault(slot, why);
        // A device the failure rule's policy turned off keeps nothing for a bind that won't come.
        if matches!(*lock(&slot.state), DeviceState::Off(_)) {
            slot.drop_kept("the device is off");
        }
        if slot.hinted.swap(false, Ordering::AcqRel) {
            self.bring_forward(slot);
        }
    }

    /// Asks an instance the host is letting go of to rebind whether it parked what the holder of
    /// its endpoints uses ([`Driver::rebinding`]), inside the panic boundary. Kept when it did and
    /// a caller holds the endpoints with END: `None` then, and the hold goes on. Otherwise the
    /// instance comes back for the caller to drop. An instance kept before is let go first: it
    /// was the one before this one, and this one has taken up what it kept or never will.
    fn keep_for_rebind(
        &self,
        slot: &Slot,
        mut instance: Box<dyn Instance>,
    ) -> Option<Box<dyn Instance>> {
        let earlier = lock(&slot.kept).take();
        drop(catch_unwind(AssertUnwindSafe(move || drop(earlier))));
        let Some(holder) = lock(&slot.end).as_ref().map(|hold| hold.caller) else {
            return Some(instance);
        };
        let parked = catch_unwind(AssertUnwindSafe(|| instance.rebinding())).unwrap_or(false);
        if !parked {
            return Some(instance);
        }
        warn(|| {
            format!(
                "device {} (driver {}): the instance is kept across the rebind: it parked the endpoints' link, and the hold goes on",
                slot.spec.id, slot.spec.driver
            )
        });
        *lock(&slot.kept) = Some(Kept {
            _instance: instance,
            holder,
        });
        None
    }

    /// The new instance's first tick has run: the instance kept across the rebind goes, and the
    /// holder's pair is lent to the new one. A lend that fails ends the hold.
    fn relend_after_rebind(&self, slot: &Slot, instance: &mut dyn Instance) {
        let Some(kept) = lock(&slot.kept).take() else {
            return;
        };
        let holder = kept.holder;
        drop(catch_unwind(AssertUnwindSafe(move || drop(kept))));
        let taken = catch_unwind(AssertUnwindSafe(|| instance.took_up_link()))
            .unwrap_or(Err("the new instance panicked when asked"));
        if let Err(why) = taken {
            warn(|| {
                format!(
                    "device {} (driver {}): the link kept across the rebind is dropped: {why}; the hold ends",
                    slot.spec.id, slot.spec.driver
                )
            });
            slot.end_close();
            return;
        }
        let lent = catch_unwind(AssertUnwindSafe(|| instance.endpoints(holder)));
        match lent {
            // The holder has its copies already: the new instance only learns who holds them.
            Ok(Ok(fds)) => drop(fds),
            Ok(Err(status)) => {
                warn(|| {
                    format!(
                        "device {} (driver {}): the new instance did not take up the holder's endpoints ({}): the hold ends",
                        slot.spec.id,
                        slot.spec.driver,
                        status_words(status)
                    )
                });
                slot.end_close();
            }
            Err(_) => slot.end_close(),
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
    fn next_state(
        &self,
        slot: &Slot,
        attempts: u32,
        reason: &str,
        kind: Option<RefusalKind>,
    ) -> DeviceState {
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
            kind,
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
        let began = std::time::Instant::now();
        let ticked = catch_unwind(AssertUnwindSafe(|| {
            let next = self.tick_instance(slot, instance.as_mut(), now);
            self.poll_jobs(slot, instance.as_mut());
            (next, asks_rebind(instance.as_ref()))
        }));
        // Each tick's duration, for the start trace: the test reads the longest tick of an install from it.
        start_trace(|| {
            format!(
                "SLOT-TICK device {} us {}",
                slot.spec.id,
                began.elapsed().as_micros()
            )
        });
        match ticked {
            Ok((next, None)) => {
                slot.hinted.store(false, Ordering::Release);
                self.settle(slot, next);
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
            self.on_deadline(&slot, Ran::Tick);
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
        self.call_from(slot, caller, false, as_job, method, args, reply, None)
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
        trace: Option<&CallTrace>,
    ) -> Result<usize> {
        slot.clear_refusal(caller);
        if slot.hung.load(Ordering::Acquire) {
            return Err(Status::NO_DEVICE);
        }
        slot.note_user(caller);
        match (slot.starts_job(method), as_job) {
            // A job is started with the job form, which keeps it on the books; a plain call
            // would leave it running with no one to tell.
            (Some(true), false) => return Err(Status::NOT_SUPPORTED),
            (Some(false) | None, true) => return Err(Status::INVALID_ARGUMENT),
            (Some(true), true) if !slot.runs_jobs() => return Err(Status::NOT_SUPPORTED),
            // The contract declares every job's bound; a job with none is not one the host will run.
            (Some(true), true) if slot.job_stall_secs(method).unwrap_or(0) == 0 => {
                return Err(Status::INVALID_ARGUMENT);
            }
            (Some(true), true) if self.jobs().open.len() >= MAX_JOBS => {
                return Err(Status::BUSY);
            }
            _ => {}
        }
        // Off is decided by the state: a device that went off is not called, even when an instance
        // is still there (a call held it when `off` ran).
        if is_off(slot) {
            return Err(Status::NO_DEVICE);
        }
        // A call that went hung after the check above holds the instance: refused, not waited on.
        let Some(mut guard) = instance_unless_hung(slot) else {
            return Err(Status::NO_DEVICE);
        };
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
            if let Some(trace) = trace {
                trace.sink.started(trace.ticket);
            }
            slot.in_tick.store(false, Ordering::Release);
            let mut status = instance.dispatch(method, args, &mut writer);
            if !status.is_ok() {
                let mut words = [0u8; REFUSAL_WORDS];
                let mut said = Reply::new(&mut words);
                instance.refusal(status, &mut said);
                if let Ok(text) = core::str::from_utf8(said.written())
                    && !text.is_empty()
                {
                    slot.keep_refusal(caller, text.to_string());
                }
            }
            self.stamp_running_ok(slot, instance.as_ref(), slot.is_running());
            if as_job && status.is_ok() {
                status = match JobId::decode(&mut Reader::new(writer.written())) {
                    Ok(job) if writer.len() == 8 => {
                        let stall_ns = u64::from(slot.job_stall_secs(method).unwrap_or(0))
                            .saturating_mul(1_000_000_000);
                        let id = self.open_job(slot, job, caller, stall_ns);
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
                self.settle(slot, self.tick_instance(slot, instance.as_mut(), now));
            }
            self.poll_jobs(slot, instance.as_mut());
            (status, asks_rebind(instance.as_ref()))
        }));
        let status = match outcome {
            Ok((status, None)) => {
                // The device went off while this call ran: `off` could not let go of the instance
                // then, so it is let go now.
                if is_off(slot) {
                    let dead = guard.take();
                    drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
                }
                status
            }
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
    /// keeps working through a new lane; only `slot` is marked. The line names what ran: the
    /// tick, the method and its caller, or the host's own work and whose it was.
    fn on_deadline(&self, slot: &Slot, ran: Ran) {
        self.abandon_lane(slot);
        slot.hung.store(true, Ordering::Release);
        let jobs = self.open_job_ids(slot);
        warn(|| {
            format!(
                "device {} (driver {}): {} ran past its {} ms deadline; its driver is abandoned until the call returns; job(s) {} end interrupted",
                slot.spec.id,
                slot.spec.driver,
                self.ran_text(slot, ran),
                self.options.call_deadline.as_millis(),
                jobs_text(&jobs),
            )
        });
        self.fault(slot, "a call did not finish in time");
    }

    /// What ran, in words: `its tick`, `the call usb.function.control_in from caller 7
    /// (lemnos-orion)`, `caller_gone for caller 7 (lemnos-orion)`.
    fn ran_text(&self, slot: &Slot, ran: Ran) -> String {
        let after = if slot.in_tick.load(Ordering::Acquire) {
            ", in the tick after it,"
        } else {
            ""
        };
        match ran {
            Ran::Tick => "its tick".to_string(),
            Ran::Call { method, caller } => format!(
                "the call {} from {}{after}",
                slot.method_text(method),
                self.caller_text(caller)
            ),
            Ran::Work { what, caller } => {
                format!("{what} for {}{after}", self.caller_text(caller))
            }
        }
    }

    /// A caller as a line names it: its number, and the name it gave when it gave one.
    fn caller_text(&self, caller: CallerId) -> String {
        if caller == CallerId::LOCAL {
            return "the daemon".to_string();
        }
        match lock(&self.caller_names)
            .iter()
            .find(|(id, _)| *id == caller)
        {
            Some((_, name)) => format!("caller {} ({name})", caller.0),
            None => format!("caller {}", caller.0),
        }
    }

    /// The ids of the open jobs of `slot`'s device.
    fn open_job_ids(&self, slot: &Slot) -> Vec<u64> {
        self.jobs()
            .open
            .iter()
            .filter(|open| open.device == slot.index)
            .map(|open| open.id)
            .collect()
    }

    /// Runs `work` on a device's driver, on its lane and inside the panic boundary, then ticks
    /// it if it ticks and passes on what changed about its jobs.
    ///
    /// A hung device is not given the work: its instance is the abandoned call's until that call
    /// returns, and the rebind after it starts a new one with nothing to tell. Waiting on it would
    /// only run out a second deadline on the new lane.
    fn on_lane(
        &'static self,
        slot: Arc<Slot>,
        ran: Ran,
        work: impl FnOnce(&mut dyn Instance) + Send + 'static,
    ) {
        if slot.hung.load(Ordering::Acquire) {
            return;
        }
        let Ok(lane) = self.lane(&slot) else {
            return;
        };
        let target = Arc::clone(&slot);
        let work = Work::Once(Box::new(move |_: &[u8], _: &mut [u8]| {
            let Some(mut guard) = instance_unless_hung(&target) else {
                return Ok(0);
            };
            let Some(instance) = guard.as_mut() else {
                return Ok(0);
            };
            target.in_tick.store(false, Ordering::Release);
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                work(instance.as_mut());
                self.stamp_running_ok(&target, instance.as_ref(), target.is_running());
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
            self.on_deadline(&slot, ran);
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
        let ran = Ran::Work {
            what: "caller_gone",
            caller,
        };
        self.on_lane(slot, ran, move |instance| {
            for job in started {
                instance.job_cancel(job);
            }
            if tells {
                instance.caller_gone(caller);
            }
        });
    }

    /// Runs `work` on the device's lane for `caller`, with the driver told who is calling when
    /// it tracks callers, and returns what `work` answered. A call that runs over its deadline
    /// is `TIMED_OUT`, as a call is. `work` must not block.
    ///
    /// A hung device is `NO_DEVICE`, as a call to it is, and never waits on the abandoned call's
    /// instance.
    fn on_lane_answer<T: Send + 'static>(
        &'static self,
        slot: Arc<Slot>,
        caller: CallerId,
        what: &'static str,
        work: impl FnOnce(&mut dyn Instance) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if slot.hung.load(Ordering::Acquire) {
            return Err(Status::NO_DEVICE);
        }
        let lane = self.lane(&slot).map_err(|_| Status::NO_MEMORY)?;
        let answer: Arc<Mutex<Option<Result<T>>>> = Arc::new(Mutex::new(None));
        let cell = Arc::clone(&answer);
        let target = Arc::clone(&slot);
        let lane_work = Work::Once(Box::new(move |_: &[u8], _: &mut [u8]| {
            let Some(mut guard) = instance_unless_hung(&target) else {
                *lock(&cell) = Some(Err(Status::NO_DEVICE));
                return Ok(0);
            };
            let Some(instance) = guard.as_mut() else {
                *lock(&cell) = Some(Err(Status::NO_DEVICE));
                return Ok(0);
            };
            let tells = target.tracks_callers();
            target.in_tick.store(false, Ordering::Release);
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                if tells {
                    instance.caller(caller);
                }
                work(instance.as_mut())
            }));
            match outcome {
                Ok(result) => *lock(&cell) = Some(result),
                Err(_) => {
                    let dead = guard.take();
                    drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
                    drop(guard);
                    self.fault(&target, "the driver panicked");
                    *lock(&cell) = Some(Err(Status::PANIC));
                }
            }
            Ok(0)
        }));
        match lane.run(&[], &mut [], self.options.call_deadline, lane_work) {
            Ok(_) => lock(&answer).take().unwrap_or(Err(Status::NO_DEVICE)),
            Err(LaneError::Deadline) => {
                self.on_deadline(&slot, Ran::Work { what, caller });
                Err(Status::TIMED_OUT)
            }
            Err(LaneError::Busy) => Err(Status::TIMED_OUT),
            Err(LaneError::Spawn) => Err(Status::NO_MEMORY),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        &'static self,
        slot: Arc<Slot>,
        caller: CallerId,
        as_job: bool,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
        trace: Option<CallTrace>,
    ) -> Result<usize> {
        let lane = match self.lane(&slot) {
            Ok(lane) => lane,
            Err(_) => {
                report_refused(trace.as_ref(), Status::NO_MEMORY);
                return Err(Status::NO_MEMORY);
            }
        };
        // Kept here: the work takes its own copy, and a lane that never takes it must still report.
        let refused = trace.clone();
        let work = Work::Call(CallWork {
            inner: self,
            slot: Arc::clone(&slot),
            method,
            caller,
            as_job,
            trace,
        });
        match lane.run(args, reply, self.options.call_deadline, work) {
            Ok(result) => result,
            Err(LaneError::Deadline) => {
                // The work may already be running on the lane: it reports its own end when it ends.
                self.on_deadline(&slot, Ran::Call { method, caller });
                Err(Status::TIMED_OUT)
            }
            Err(LaneError::Busy) => {
                report_refused(refused.as_ref(), Status::TIMED_OUT);
                Err(Status::TIMED_OUT)
            }
            Err(LaneError::Spawn) => {
                report_refused(refused.as_ref(), Status::NO_MEMORY);
                Err(Status::NO_MEMORY)
            }
        }
    }

    /// Builds the driver, on its lane, inside the panic boundary.
    fn bind(&'static self, slot: Arc<Slot>) {
        slot.tried_ns.store(self.clock.now_ns(), Ordering::Release);
        slot.running_ok_ns.store(0, Ordering::Release);
        slot.first_tick_traced.store(false, Ordering::Release);
        start_trace(|| {
            format!(
                "bind {} ({}) begins on lane {}",
                slot.spec.id, slot.spec.driver, slot.lane_key
            )
        });
        // A device whose settings its driver refuses is never made. It shows failed, with the
        // reason, and is retried under the failure rule like any device that fails to bind.
        if let Some(reason) = &slot.refusal {
            self.record_failed_bind(&slot, reason);
            return;
        }
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
            warn(|| {
                format!(
                    "device {} (driver {}): rebind refused: a call into the old instance is still running; retried with backoff, capped at 8 s",
                    slot.spec.id, slot.spec.driver
                )
            });
            self.record_failed_bind(&slot, "a call into the old driver is still stuck");
            return;
        };
        if slot.hung.load(Ordering::Acquire) {
            warn(|| {
                format!(
                    "device {} (driver {}): the abandoned call returned; the old instance is released and the device rebinds",
                    slot.spec.id, slot.spec.driver
                )
            });
        }
        let dead = old.take();
        drop(old);
        // An instance that parked the holder's link is kept, and the hold with it; one kept by the
        // rebind before this bind keeps it too. Otherwise the hold ends here.
        let dead = dead.and_then(|dead| self.keep_for_rebind(&slot, dead));
        if lock(&slot.kept).is_none() {
            slot.end_close();
        }
        slot.stop_wait();
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
            // The reason a driver gives for a refusal, or a bus it could not open, is kept here.
            let refused: std::sync::Mutex<Option<(RefusalKind, String)>> =
                std::sync::Mutex::new(None);
            let sink = |kind: RefusalKind, reason: core::fmt::Arguments<'_>| {
                let mut text = String::new();
                let _ = core::fmt::Write::write_fmt(&mut text, reason);
                *refused.lock().unwrap_or_else(|e| e.into_inner()) = Some((kind, text));
            };
            let config = Config::new(&entries)
                .with_lists(&borrowed)
                .on_board(&self.board)
                .on_host_version(self.daemon_version)
                .with_call_deadline_ms(
                    u64::try_from(self.options.call_deadline.as_millis()).unwrap_or(u64::MAX),
                )
                .with_refusals(RefusalSink::new(&sink));
            let config = if guest {
                config.for_host_owned()
            } else {
                config
            };
            let mut binding = Binding {
                inner: self,
                slot: &target,
                missing: RefCell::new(Vec::new()),
                refused: &refused,
            };
            let link = target.spec.link.as_deref();
            // What the device's last rebind kept, if anything: the new instance is asked to take it
            // up before it is ever ticked (`Driver::resume`).
            let record = *lock(&target.resume);
            let built = catch_unwind(AssertUnwindSafe(|| -> Result<(Box<dyn Instance>, bool)> {
                let mut res = Resources::new(&mut binding, link);
                let mut instance = entry.factory.bind(&mut res, &config)?;
                let kept = record.is_some_and(|record| instance.resume(&record));
                Ok((instance, kept))
            }));
            let missing = binding.missing.take();
            *lock(&target.bind_refusal) = refused.lock().unwrap_or_else(|e| e.into_inner()).take();
            // A real-valued item must carry exponent 0: the renderer scales a float by the exponent,
            // so a nonzero one shows the value wrong. Refused by name, before the device runs.
            let built = match built {
                Ok(Ok((instance, kept))) => {
                    match describe_fault(instance.as_ref(), entry.manifest()) {
                        Some(reason) => {
                            *lock(&target.bind_refusal) = Some((RefusalKind::Failed, reason));
                            Ok(Err(Status::INVALID_ARGUMENT))
                        }
                        None => Ok(Ok((instance, kept))),
                    }
                }
                other => other,
            };
            match built {
                Ok(Ok((instance, kept))) => {
                    *lock(&target.missing) = missing;
                    *lock(&target.instance) = Some(instance);
                    Ok(usize::from(kept))
                }
                Ok(Err(status)) => Err(status),
                Err(_) => Err(Status::PANIC),
            }
        }));
        let ran = lane.run(&[], &mut [], self.options.call_deadline, work);
        start_trace(|| {
            format!(
                "bind {} {}",
                slot.spec.id,
                if matches!(ran, Ok(Ok(_))) {
                    "ok"
                } else {
                    "failed"
                }
            )
        });
        match ran {
            Ok(Ok(kept)) => {
                slot.errors.store(0, Ordering::Relaxed);
                slot.bound_ns.store(self.clock.now_ns(), Ordering::Release);
                // The record was for this bind, and is spent either way. One the new instance did
                // not take up makes the stall a new instance's, so it reads lost.
                if lock(&slot.resume).take().is_some() && kept == 0 {
                    self.stall(&slot, true, || Cow::Borrowed(NOT_TAKEN_UP));
                }
                // Before the device shows as running, so a look that finds it running finds the stamp too.
                if let Some(instance) = lock(&slot.instance).as_ref() {
                    self.stamp_running_ok(&slot, instance.as_ref(), true);
                }
                *lock(&slot.state) = DeviceState::Running;
                // A new instance starts not stalled: its own first failure is an edge.
                slot.stalled.store(false, Ordering::Release);
                self.unstall(&slot);
                if slot.ticks() {
                    self.set_next_tick(&slot, Some(0));
                    let wait = lock(&slot.instance)
                        .as_ref()
                        .and_then(|instance| instance.wait_fd());
                    if let Some(fd) = wait {
                        self.watch_wait(&slot, fd);
                    }
                }
                self.moved(&slot);
                self.bring_users_forward(&slot.spec.id);
            }
            Ok(Err(status)) => {
                // The driver's own reason, then a bus it could not open, then the link, then the
                // status named in words: the first one there says why the bind was refused.
                let (kind, reason) = lock(&slot.bind_refusal)
                    .take()
                    .or_else(|| {
                        lock(&slot.link_refusal)
                            .take()
                            .map(|text| (RefusalKind::Failed, text))
                    })
                    .unwrap_or_else(|| (status_kind(status), status_words(status).to_string()));
                self.record_refusal(&slot, &format!("bind failed: {reason}"), Some(kind));
            }
            Err(LaneError::Deadline) => {
                self.abandon_lane(&slot);
                self.record_failed_bind(&slot, "bind did not finish in time");
            }
            Err(LaneError::Busy) => self.record_failed_bind(&slot, "its bus is busy"),
            Err(LaneError::Spawn) => self.record_failed_bind(&slot, "no thread for the driver"),
        }
    }

    /// A bind refused for a reason the host itself has (no driver named, a stuck call): a failure.
    fn record_failed_bind(&self, slot: &Slot, reason: &str) {
        self.record_refusal(slot, reason, Some(RefusalKind::Failed));
    }

    /// A bind refused with `reason`, of `kind`: the device waits and is retried under the failure rule.
    fn record_refusal(&self, slot: &Slot, reason: &str, kind: Option<RefusalKind>) {
        slot.drop_kept("the bind failed");
        let mut state = lock(&slot.state);
        let attempts = match &*state {
            DeviceState::Off(_) => return,
            DeviceState::Backoff { attempts, .. } => *attempts,
            DeviceState::Running => 0,
        };
        let next = self.next_state(slot, attempts, reason, kind);
        *state = next.clone();
        drop(state);
        self.moved(slot);
        self.streams_follow(slot, &next);
    }

    /// Ticks a driver: after giving it the board first when it watches the board.
    fn tick_instance(&self, slot: &Slot, instance: &mut dyn Instance, now_ns: u64) -> Option<u64> {
        // A wake that came before this tick is what the tick is about to find: only a wake that
        // comes while it runs needs another (see `settle`).
        slot.kicked.store(false, Ordering::SeqCst);
        slot.in_tick.store(true, Ordering::Release);
        if slot.entry.is_some_and(|entry| entry.watches) {
            instance.board(&BoardRead { inner: self });
        }
        let next = instance.tick(now_ns);
        if !slot.first_tick_traced.swap(true, Ordering::AcqRel) {
            start_trace(|| format!("first tick {} returned", slot.spec.id));
        }
        self.relend_after_rebind(slot, instance);
        self.stamp_running_ok(slot, instance, slot.is_running());
        if instance.endpoints_ended() {
            slot.end_close();
        }
        // The same instance's own report: failed or gone is a stall that this driver recovers from
        // itself (what it was given is kept); anything else, including degraded, is not one.
        let failed = match instance.health() {
            Health::Failed(why) | Health::Gone(why) => Some(why),
            _ => None,
        };
        if slot.stalled.swap(failed.is_some(), Ordering::AcqRel) != failed.is_some() {
            match failed {
                Some(why) => self.stall(slot, false, || Cow::Borrowed(why)),
                None => self.unstall(slot),
            }
        }
        slot.in_tick.store(false, Ordering::Release);
        next
    }

    /// Stamps the moment this bind's device first runs with health Ok, from the driver's own run:
    /// the bind, a tick, or a call that the host has just made, with the instance still held, so a
    /// look that finds the device running finds the stamp too. Exact to the run. Skipped for a
    /// device with device needs: its health also depends on its children, which the host does not
    /// watch at that moment, so [`Host::look`] stamps it (see [`Host::running_ok_ns`]).
    fn stamp_running_ok(&self, slot: &Slot, instance: &dyn Instance, running: bool) {
        if !running || slot.running_ok_ns.load(Ordering::Acquire) != 0 || has_device_needs(slot) {
            return;
        }
        let ok = catch_unwind(AssertUnwindSafe(|| instance.health()))
            .is_ok_and(|health| health == Health::Ok);
        if ok {
            self.set_running_ok(slot);
        }
    }

    /// Stamps the first moment this bind's device is seen running with health Ok. The first stamp of
    /// a bind wins, so a look never replaces the driver's exact one. Allocates nothing.
    fn set_running_ok(&self, slot: &Slot) {
        let now = self.clock.now_ns().saturating_add(1);
        let _ = slot
            .running_ok_ns
            .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Whether a device comes and goes: an `[[each]]` entry made it, or it sits on a bus the
    /// backend says comes and goes, or on a device that does (`uses`), to `depth` levels down.
    fn hotplug_of(&self, slot: &Slot, depth: u32) -> bool {
        if slot.made.is_some() {
            return true;
        }
        if slot
            .spec
            .bus
            .as_ref()
            .is_some_and(|bus| lock(&self.backend).hotplug(bus))
        {
            return true;
        }
        depth > 0
            && slot
                .spec
                .uses
                .values()
                .filter_map(|id| self.slot_by_id(id))
                .any(|child| self.hotplug_of(&child, depth - 1))
    }

    /// The driver's own account of its retries, when it is running and says it is retrying. Read
    /// with `try_lock`: a driver in a call right now says nothing until the call returns.
    fn device_retry(&self, slot: &Slot) -> Option<Retry> {
        if !matches!(*lock(&slot.state), DeviceState::Running) {
            return None;
        }
        let guard = slot.instance.try_lock().ok()?;
        let instance = guard.as_ref()?;
        catch_unwind(AssertUnwindSafe(|| instance.retrying()))
            .ok()
            .flatten()
    }

    fn device_health(&self, slot: &Slot, depth: u32) -> Health {
        self.health_of(slot, depth).0
    }

    /// A device's health, and whether it is settled: `false` when its driver is in a call right
    /// now, so its own health is not known until the call returns. An unsettled device reads as
    /// [`Health::Ok`], as it always has, and its needs are read as they are. Allocates nothing.
    /// The figures of a device's driver, read only while it is running and not in a call. A driver
    /// that panics, or is in a call, has none to give, so it has none.
    fn device_figures(&self, slot: &Slot) -> Figures {
        if !matches!(*lock(&slot.state), DeviceState::Running) {
            return Figures::NONE;
        }
        match slot.instance.try_lock() {
            Ok(guard) => match guard.as_ref() {
                Some(instance) => {
                    catch_unwind(AssertUnwindSafe(|| instance.figures())).unwrap_or(Figures::NONE)
                }
                None => Figures::NONE,
            },
            Err(_) => Figures::NONE,
        }
    }

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
            let origin = if self.inner.hotplug_of(&slot, MAX_DEPTH) {
                Origin::Hotplug
            } else {
                Origin::BuiltIn
            };
            f(BoardDevice {
                id: &slot.spec.id,
                origin,
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

    fn others_use(&self, caller: CallerId, device: u32) -> Result<bool> {
        let slot = self.slot(device).ok_or(Status::NOT_FOUND)?;
        Ok(slot.others_use(caller))
    }
}

/// Deepest chain of devices health is passed up through.
const MAX_DEPTH: u32 = 16;

/// Gives a binding driver what its board entry says it may have.
struct Binding<'a> {
    inner: &'static Inner,
    slot: &'a Slot,
    missing: RefCell<Vec<String>>,
    /// The first reason a bus this bind needs is absent, or a driver's own (see `Slot::bind_refusal`).
    refused: &'a std::sync::Mutex<Option<(RefusalKind, String)>>,
}

impl Binding<'_> {
    /// A bus the board names that the backend cannot open: absent when there is no such bus here.
    /// The first such reason is kept, unless the driver gives its own, which replaces it.
    fn absent_bus(&self, status: Status, words: &str, bus: &str) -> Status {
        if matches!(status, Status::NO_DEVICE | Status::NOT_FOUND) {
            let mut refused = self.refused.lock().unwrap_or_else(|e| e.into_inner());
            if refused.is_none() {
                *refused = Some((RefusalKind::Absent, format!("no {words} {bus}")));
            }
        }
        status
    }

    /// A device this one needs that is not running: absent for now, and this bind says so.
    fn not_running(&self, status: Status, id: &str) -> Status {
        if status == Status::NO_DEVICE {
            let mut refused = self.refused.lock().unwrap_or_else(|e| e.into_inner());
            if refused.is_none() {
                *refused = Some((RefusalKind::Absent, format!("device {id} is not running")));
            }
        }
        status
    }

    /// Whether the device being bound is the operating system's, to be read and not changed.
    fn guest(&self) -> bool {
        self.slot.spec.owner == Owner::Host
    }

    /// The kind of bus the driver declares for `need`, if it declares a bus need by that name.
    fn kind_of(&self, need: &str) -> Option<BusKind> {
        let entry = self.slot.entry?;
        entry.manifest.needs.iter().find_map(|declared| {
            match (declared.name == need, declared.kind) {
                (true, NeedKind::Bus(bus)) => Some(bus),
                _ => None,
            }
        })
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
            _ => *self.inner.routes.get().ok_or(Status::NO_MEMORY)?,
        };
        Ok(LocalCaller::new(router, target.index, contract))
    }
}

/// A status a bind was refused with, said in words: the name of the errno or code, as a person
/// reads it. A driver that gives its own reason never reaches this.
fn status_words(status: Status) -> &'static str {
    match status {
        Status::NOT_PERMITTED => "not permitted",
        Status::NOT_FOUND => "not found",
        Status::INTERRUPTED => "interrupted",
        Status::IO => "an I/O error",
        Status::AGAIN => "try again later",
        Status::NO_MEMORY => "out of memory",
        Status::ACCESS => "permission denied",
        Status::BUSY => "busy",
        Status::NO_DEVICE => "no such device",
        Status::INVALID_ARGUMENT => "invalid argument",
        Status::NOT_SUPPORTED => "not supported",
        Status::TIMED_OUT => "timed out",
        Status::CANCELLED => "cancelled",
        Status::STALE_HANDLE => "stale handle",
        Status::VERSION_MISMATCH => "version mismatch",
        Status::BUFFER_TOO_SMALL => "buffer too small",
        Status::PANIC => "the driver panicked",
        Status::LAYOUT_MISMATCH => "layout mismatch",
        Status::TORN => "a frame's buffer was reused while it was read",
        Status::FAILED_PRECONDITION => "a step the device needs first has not been taken",
        _ => "failed with an unnamed error",
    }
}

/// Absent for a device that is not here (no such device, not found); failed for any other status.
fn status_kind(status: Status) -> RefusalKind {
    match status {
        Status::NO_DEVICE | Status::NOT_FOUND => RefusalKind::Absent,
        _ => RefusalKind::Failed,
    }
}

macro_rules! buses {
    ($($method:ident -> $handle:ty, $words:literal;)*) => {$(
        fn $method(&mut self, need: &str) -> Result<$handle> {
            let bus = self.bus_name(need)?;
            let handle = lock(&self.inner.backend)
                .$method(&bus)
                .map_err(|status| self.absent_bus(status, $words, &bus))?;
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
        // The host clock is the backend's, whatever the board names: it is provided when the backend
        // gives one, and a board's `bus` or `uses` never reaches it.
        if self.kind_of(need) == Some(BusKind::HostClock) {
            return lock(&self.inner.backend).host_clock(need).is_ok();
        }
        // A lend need is provided when the board names the stream it reads (`lend`), and no other way:
        // an optional lend need is not a bus, so the name rules below never reach it.
        let lend_need = self.slot.entry.is_some_and(|entry| {
            entry
                .manifest
                .needs
                .iter()
                .any(|n| n.name == need && matches!(n.kind, NeedKind::Bus(BusKind::Lend)))
        });
        if lend_need {
            return self.slot.spec.lend.is_some();
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

    fn host_clock(&mut self, need: &str) -> Result<lemnos_core::HostClockHandle> {
        lock(&self.inner.backend).host_clock(need).inspect_err(|_| {
            let mut refused = self.refused.lock().unwrap_or_else(|e| e.into_inner());
            if refused.is_none() {
                *refused = Some((
                    RefusalKind::Absent,
                    format!("this backend has no host clock for {need:?}"),
                ));
            }
        })
    }

    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller> {
        let target = self.target(need).ok_or(Status::NOT_FOUND)?;
        self.caller_of(&target, tag)
            .map_err(|status| self.not_running(status, &target.spec.id))
    }

    fn member(&mut self, id: &str, tag: Tag) -> Result<LocalCaller> {
        let target = self.inner.slot_by_id(id).ok_or(Status::NOT_FOUND)?;
        // A device calling itself would wait on its own lane.
        if target.index == self.slot.index {
            return Err(Status::INVALID_ARGUMENT);
        }
        let contract = target.contract_index(tag).ok_or(Status::INVALID_ARGUMENT)?;
        let router: &'static dyn Router = *self.inner.routes.get().ok_or(Status::NO_MEMORY)?;
        Ok(LocalCaller::new(router, target.index, contract).by(CallerId::device(self.slot.index)))
    }

    fn members(&mut self, tag: Tag, found: &mut dyn FnMut(&str)) -> Result<()> {
        let mut index = 0;
        while let Some(slot) = self.inner.slot(index) {
            index += 1;
            if slot.index != self.slot.index && slot.contract_index(tag).is_some() {
                found(&slot.spec.id);
            }
        }
        Ok(())
    }

    fn link_device(&mut self, tag: Tag) -> Result<LocalCaller> {
        let id = self.slot.spec.link.as_deref().ok_or(Status::NOT_FOUND)?;
        let target = self.inner.slot_by_id(id).ok_or(Status::NOT_FOUND)?;
        let caller = self
            .caller_of(&target, tag)
            .map_err(|status| self.not_running(status, id));
        // A linked device that does not serve the contract can never be bound to this link: say which
        // contract is wanted and which the device serves, rather than leave a bare status.
        *lock(&self.slot.link_refusal) = match &caller {
            Err(Status::INVALID_ARGUMENT) => Some(target.serves_message(id, tag)),
            _ => None,
        };
        caller
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        let bus = self.bus_name(need)?;
        let handle = lock(&self.inner.backend)
            .i2c(&bus)
            .map_err(|status| self.absent_bus(status, "I2C bus", &bus))?;
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
        spi -> Spi, "SPI bus";
        raw_spi -> RawSpiBus, "SPI bus";
        line -> Line, "GPIO line";
        pwm -> Pwm, "PWM channel";
        uart -> Uart, "UART";
        usb_device -> UsbDeviceHandle, "USB device";
        usb_function -> UsbFunctionHandle, "USB function";
        registers -> RegistersHandle, "register block";
        interrupt -> InterruptHandle, "interrupt line";
        dma -> DmaHandle, "DMA channel";
        clocks -> ClocksHandle, "clock";
        reset -> ResetHandle, "reset line";
        watchdog -> Watchdog, "watchdog";
        pio -> Pio, "PIO output";
    }

    fn lend(&mut self, _need: &str) -> Result<lemnos_core::LendHandle> {
        // The upstream is the one device whose `link` names this one, and the stream is the one this
        // device's board entry names in `lend`: a block reads exactly that stream. Neither is taken from
        // the consumer. No `lend` named, no upstream, or more than one: no lend.
        let stream = self.slot.spec.lend.as_deref().ok_or(Status::NOT_FOUND)?;
        let me = self.slot.spec.id.as_str();
        let slots = self
            .inner
            .slots
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let mut linked = slots
            .iter()
            .filter(|slot| slot.spec.link.as_deref() == Some(me))
            .map(|slot| slot.index);
        let upstream = linked.next().ok_or(Status::NOT_FOUND)?;
        if linked.next().is_some() {
            return Err(Status::NOT_FOUND);
        }
        drop(slots);
        let view = self.inner.streams.lend_view(upstream, stream)?;
        Ok(lemnos_core::Held::boxed(Box::new(view)))
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
            Arc::new(MemfdAllocator),
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
        let table: BTreeMap<_, _> = drivers
            .iter()
            .map(|driver| (driver.manifest.name, *driver))
            .collect();
        // Each device's streams take the kinds its driver declares (`resolve_kinds`). One whose kinds
        // cannot be resolved is refused by name, and the rest of the board runs.
        let mut devices = board.devices.clone();
        for spec in &mut devices {
            let entry = table.get(spec.driver.as_str()).copied();
            if let Err(error) = resolve_kinds(
                &spec.driver,
                entry.as_ref().map(|e| &e.manifest),
                &mut spec.streams,
            ) && spec.refused.is_none()
            {
                spec.refused = Some(format!("device {:?}: {error}", spec.id));
            }
        }
        let mut each = board.each.clone();
        for spec in &mut each {
            let entry = table.get(spec.driver.as_str()).copied();
            let mut streams = spec.streams.clone();
            if let Err(error) = resolve_kinds(
                &spec.driver,
                entry.as_ref().map(|e| &e.manifest),
                &mut streams,
            ) && spec.refused.is_none()
            {
                spec.refused = Some(format!("device {:?}: {error}", spec.id));
            }
        }
        let order = crate::board::bind_order(&devices);
        let refusals: Vec<Option<String>> = devices
            .iter()
            .map(|spec| refusal_for(board, spec, drivers, None))
            .collect();
        let each_refusals: Vec<Option<String>> = each
            .iter()
            .map(|spec| {
                refusal_for(
                    board,
                    &spec.device(&spec.pattern),
                    drivers,
                    spec.need.as_deref(),
                )
            })
            .collect();
        let security = board.given_facts();
        let mut slots = Vec::with_capacity(devices.len());
        for (index, spec) in devices.iter().enumerate() {
            let entry = table.get(spec.driver.as_str()).copied();
            slots.push(Arc::new(Slot::new(
                index as u32,
                spec.clone(),
                entry,
                None,
                refusals[index].clone(),
                &security,
            )?));
        }
        let rules = each
            .iter()
            .zip(&each_refusals)
            .map(|(spec, refusal)| Rule {
                spec: spec.clone(),
                refusal: refusal.clone(),
                hinted: AtomicBool::new(false),
                next_look_ns: AtomicU64::new(0),
                looked_ns: AtomicU64::new(0),
            })
            .collect();

        let streams = Streams::new(allocator, &devices);

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
            security,
            waker: OnceLock::new(),
            liveness: OnceLock::new(),
            streams,
            reading: OnceLock::new(),
            routes: OnceLock::new(),
            jobs: Mutex::new(Jobs::new()),
            caller_names: Mutex::new(Vec::new()),
        }));
        // Lives as long as `inner` does, which is for good.
        let _ = inner.reading.set(Box::leak(Box::new(Reading(inner))));
        let _ = inner.routes.set(Box::leak(Box::new(Routes(inner))));
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
            label: slot
                .spec
                .label
                .clone()
                .unwrap_or_else(|| slot.spec.id.clone()),
            bus: slot.spec.bus.clone(),
            driver: slot.spec.driver.clone(),
            tags: slot
                .entry
                .map(|e| e.manifest.provides.iter().map(|c| c.tag).collect())
                .unwrap_or_default(),
            state,
            health: self.inner.device_health(slot, MAX_DEPTH),
            figures: self.inner.device_figures(slot),
            retry: self.inner.device_retry(slot),
            link: slot.spec.link.clone(),
            last_stall: lock(&slot.last_stall).clone(),
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

    /// Each stream the board declares for `device`, with the bytes it holds on standby
    /// (`buffers * buffer_bytes` while memory waits for a consumer, else 0) and the bytes a driver's
    /// handle still holds open after the last consumer left (0 when none). Empty for an unknown
    /// device.
    #[must_use]
    pub fn stream_standby(&self, device: u32) -> Vec<(String, u64, u64)> {
        let Some(slot) = self.inner.slot(device) else {
            return Vec::new();
        };
        slot.spec
            .streams
            .keys()
            .map(|name| {
                let (standby, held) = self
                    .inner
                    .streams
                    .stats(device, name)
                    .map_or((0, 0), |stats| (stats.standby_bytes, stats.held_by_driver));
                (name.clone(), standby, held)
            })
            .collect()
    }

    /// Each stream the board declares for `device`: its name, buffer count and buffer size, as the
    /// board file gives them. Nothing needs to be attached or allocated. Empty for an unknown device.
    #[must_use]
    pub fn stream_layouts(&self, device: u32) -> Vec<(String, u32, u32)> {
        let Some(slot) = self.inner.slot(device) else {
            return Vec::new();
        };
        slot.spec
            .streams
            .iter()
            .map(|(name, spec)| (name.clone(), spec.buffers, spec.buffer_bytes))
            .collect()
    }

    /// Asks a running device to describe itself. The ask takes its turn on the device's lane like
    /// a call does: while the driver is in a tick or another call it waits, up to the call
    /// deadline, and is never answered `BUSY` for that. A driver that doesn't come back in time
    /// is `TIMED_OUT` (and contained, as a call's would be); a device that is off, failed, or
    /// whose driver is gone is `NO_DEVICE`.
    pub fn describe(&self, device: u32) -> Result<Description> {
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        if slot.hung.load(Ordering::Acquire) || is_off(&slot) {
            return Err(Status::NO_DEVICE);
        }
        let mut out = self.inner.on_lane_answer(
            Arc::clone(&slot),
            CallerId::LOCAL,
            "describe",
            |instance| {
                let mut out = Description::new();
                instance.describe(&mut out);
                Ok(out)
            },
        )?;
        if is_off(&slot) {
            return Err(Status::NO_DEVICE);
        }
        // A device whose driver names no location gets the board's own words for what it uses
        // (`line gpio7`, `accel imu`): the host knows them, the driver need not.
        if out.location().is_empty() && !slot.spec.uses.is_empty() {
            let words: Vec<String> = slot
                .spec
                .uses
                .iter()
                .map(|(role, name)| format!("{role} {name}"))
                .collect();
            out.set_location(&words.join(", "));
        }
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

    /// Gives `caller` a name for the host's lines about its calls: a line about a call that ran past
    /// its deadline says `caller 7 (name)`. Kept until [`Host::caller_gone`]; naming it again replaces
    /// the name. The daemon names each client by the name it gave when it connected.
    pub fn name_caller(&self, caller: CallerId, name: &str) {
        let mut names = lock(&self.inner.caller_names);
        match names.iter_mut().find(|(id, _)| *id == caller) {
            Some((_, kept)) => name.clone_into(kept),
            None => names.push((caller, name.to_string())),
        }
    }

    /// Whether a caller other than `caller` is using `device`: one that has called it and has not
    /// gone. See [`Router::others_use`].
    pub fn others_use(&self, caller: CallerId, device: u32) -> Result<bool> {
        Router::others_use(self.inner, caller, device)
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
        self.inner
            .call(slot, caller, false, method, args, reply, None)
    }

    /// As [`Host::call_as`], and `trace` hears how the call goes: `started` when the driver's method
    /// is about to run, and `finished` with the status it ends with. A call that the caller gave up
    /// on (its [`Status::TIMED_OUT`] here) may still run on its lane; `finished` says so when it ends,
    /// so the caller can learn what happened after its timeout. A device that is not there is
    /// `finished` at once, with [`Status::NOT_FOUND`].
    pub fn call_traced(
        &self,
        caller: CallerId,
        device: u32,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
        trace: CallTrace,
    ) -> Result<usize> {
        let Some(slot) = self.inner.slot(device) else {
            report_refused(Some(&trace), Status::NOT_FOUND);
            return Err(Status::NOT_FOUND);
        };
        self.inner
            .call(slot, caller, false, method, args, reply, Some(trace))
    }

    /// The words the driver gave for `caller`'s last refused call or job start into `device`
    /// ([`Driver::refusal`]), taken: asked once, right after the refusal. `None` when it said none,
    /// or when another call of `caller`'s into the device has begun since.
    pub fn take_refusal(&self, device: u32, caller: CallerId) -> Option<String> {
        let slot = self.inner.slot(device)?;
        if !slot.refused.load(Ordering::Acquire) {
            return None;
        }
        let mut refusals = lock(&slot.refusals);
        let at = refusals.iter().position(|(who, _)| *who == caller)?;
        let (_, words) = refusals.remove(at);
        slot.refused.store(!refusals.is_empty(), Ordering::Release);
        Some(words)
    }

    /// Borrows the two bulk endpoints of a device's function for `caller`, as `[OUT, IN]`
    /// descriptors duplicated for `caller` to keep: the caller reads and writes them itself, and
    /// the driver's own traffic on them stops while they are held. One caller holds them at a
    /// time: another gets `BUSY`. They are held until `caller` gives them up with
    /// [`Host::usb_endpoints_release`] or goes ([`Host::caller_gone`]). `UNAVAILABLE` while the
    /// function is not enabled, `NOT_SUPPORTED` for a device whose driver can't lend them, and
    /// `NOT_FOUND` for no such device.
    pub fn usb_endpoints(&self, caller: CallerId, device: u32) -> Result<[OwnedFd; 2]> {
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        self.inner
            .on_lane_answer(slot, caller, "the endpoints' lend", move |instance| {
                instance.endpoints(caller)
            })
    }

    /// [`Host::usb_endpoints`] with a third descriptor, END, as `[OUT, IN, END]`. END is the read
    /// end of a pipe whose write end the host keeps for the hold, and it reads EOF (or hangs up)
    /// once the hold is over for good: when `caller` releases the endpoints or goes, when the
    /// device is switched off, reloaded or shut down, and when its driver says its endpoints have
    /// ended ([`Driver::endpoints_ended`], asked after each tick). A host going away or a bus
    /// reset is not that: the pair stays the same. The same caller asking again gets the same
    /// END.
    pub fn usb_endpoints_end(&self, caller: CallerId, device: u32) -> Result<[OwnedFd; 3]> {
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        let [out, input] = self.inner.on_lane_answer(
            Arc::clone(&slot),
            caller,
            "the endpoints' lend",
            move |instance| instance.endpoints(caller),
        )?;
        let end = slot.end_for(caller)?;
        *lock(&slot.lent_to) = Some(caller);
        Ok([out, input, end])
    }

    /// Gives up the endpoints `caller` holds on a device. `NOT_FOUND` if it holds none.
    pub fn usb_endpoints_release(&self, caller: CallerId, device: u32) -> Result<()> {
        let slot = self.inner.slot(device).ok_or(Status::NOT_FOUND)?;
        let done = self.inner.on_lane_answer(
            Arc::clone(&slot),
            caller,
            "the endpoints' release",
            move |instance| instance.release_endpoints(caller),
        );
        slot.end_close_for(caller);
        done
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
            .call(slot, caller, true, method, args, &mut reply, None)?;
        u64::decode(&mut Reader::new(&reply[..n]))
    }

    /// Cancels a job `caller` started. The driver stops it and it finishes as
    /// [`Status::CANCELLED`], or as it really ended if it got there first. `NOT_FOUND` for a job
    /// that has finished or never was, `INTERRUPTED` for one a host before this one started (a
    /// restart ended it), and `NOT_PERMITTED` for one another caller started.
    /// [`CallerId::LOCAL`] may cancel any.
    pub fn cancel_job(&self, caller: CallerId, job: u64) -> Result<()> {
        self.inner.cancel_for(caller, job)
    }

    /// Moves what changed about long jobs, oldest first, into `into`. The host's waker
    /// ([`Host::set_waker`]) is called when there is something to take.
    pub fn take_job_updates(&self, into: &mut Vec<JobUpdate>) {
        into.extend(self.inner.jobs().updates.drain(..));
    }

    /// The long jobs open on `device`, oldest first, with the progress last told. Read from the
    /// host's job table alone: the driver is not entered, so this answers while it is in a step.
    /// Allocates only when the device has a job.
    #[must_use]
    pub fn jobs_of(&self, device: u32) -> Vec<JobInfo> {
        let Some(slot) = self.inner.slot(device) else {
            return Vec::new();
        };
        if slot.open_jobs.load(Ordering::Acquire) == 0 {
            return Vec::new();
        }
        self.inner
            .jobs()
            .open
            .iter()
            .filter(|job| job.device == device)
            .map(|job| {
                let (done, total) = job.told.unwrap_or((0, 0));
                JobInfo {
                    job: job.id,
                    done,
                    total,
                }
            })
            .collect()
    }

    /// How many long jobs are open.
    pub fn open_jobs(&self) -> usize {
        self.inner.jobs().open.len()
    }

    /// Tells every driver that tracks callers that `caller` has gone, and cancels every job it
    /// started. Blocks until each driver has answered or its call deadline has passed.
    pub fn caller_gone(&self, caller: CallerId) {
        lock(&self.inner.caller_names).retain(|(id, _)| *id != caller);
        self.inner.streams.caller_gone(caller);
        let mut index = 0;
        while let Some(slot) = self.inner.slot(index) {
            index += 1;
            slot.forget_user(caller);
            slot.end_close_for(caller);
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

    /// As [`stream_attach`](Self::stream_attach), for a consumer that declares it holds `hold`
    /// buffers at once. The ticket math reserves exactly `hold` for it, and its own holds are
    /// bounded by it. A declaration that does not fit (`hold` is 0, or it leaves the producer
    /// fewer than the producer's two of the stream's buffers) is refused with
    /// its reason in the text and nothing is attached. Nothing is trimmed.
    ///
    /// # Errors
    ///
    /// [`TicketRefused`]: `NOT_FOUND` (no such stream), `BUSY` (a consumer is attached, or the
    /// declaration does not fit), `INVALID_ARGUMENT` (`hold` is 0).
    pub fn stream_attach_declared(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
        hold: u32,
    ) -> core::result::Result<Lease, TicketRefused> {
        self.inner
            .streams
            .attach_declared(device, name, caller, hold)
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

    /// Gives `caller`, who has a stream of `device` open, a reader slot on it: the descriptors and
    /// the slot for a third process to read the stream with, read-only (see [`ReaderTicket`]).
    /// `held` and `hold_ms` are asked for; zero asks for 500 ms and the default held count, which takes
    /// what fits (up to 2); an explicit `held` is all or refused. The stream's maxima bound what is
    /// granted (the grant is in the answer).
    ///
    /// # Errors
    ///
    /// [`TicketRefused`] with the status and a text that names the stream: `NOT_FOUND` (no such
    /// stream, or no consumer), `NOT_PERMITTED` (`caller` is not the consumer), `BUSY` (every reader
    /// slot is taken, or the holds would leave the producer too few buffers), `NOT_SUPPORTED`
    /// (`readers` is 0, or the stream's dma-bufs are writable).
    pub fn stream_ticket(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
        held: u32,
        hold_ms: u32,
        import: bool,
    ) -> core::result::Result<ReaderTicket, TicketRefused> {
        self.inner
            .streams
            .ticket(device, name, caller, held, hold_ms, import)
    }

    /// Sets the daemon's signed mode, which decides whether a reader may import a dma-buf stream's
    /// buffers unless the stream says so (`reader_import`). A host that is not told stays signed.
    pub fn set_signed_mode(&self, signed: bool) {
        self.inner.streams.set_signed(signed);
    }

    /// What the host knows of reader `slot` of a stream `caller` has open: whether it is live, and
    /// why and when it was cut off.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` (no stream, no consumer, or `slot` is not a reader's), `NOT_PERMITTED`.
    pub fn stream_ticket_state(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
        slot: u32,
    ) -> core::result::Result<TicketState, Status> {
        self.inner.streams.ticket_state(device, name, caller, slot)
    }

    /// Gives back a reader slot `caller` was granted on a stream. The reader is cut off and its
    /// slot is freed once its liveness pipe hangs up.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` if there is no such stream, or `slot` isn't a reader's on it. `NOT_PERMITTED` if
    /// `caller` isn't the stream's consumer.
    pub fn stream_unticket(
        &self,
        device: u32,
        name: &str,
        caller: CallerId,
        slot: u32,
    ) -> core::result::Result<(), Status> {
        self.inner.streams.unticket(device, name, caller, slot)
    }

    /// Applies every stream's reader limits and frees the slots of readers that have gone. Call
    /// this on each pass of the daemon's loop, which is what bounds how long a dead reader's holds
    /// last. Allocates nothing and makes no call per frame.
    pub fn streams_tick(&self) {
        self.inner.streams.tick();
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
            // A job over its bound is due too, so a silent job ends on time whether or not its
            // driver ticks.
            let next = slot
                .next_tick_ns
                .load(Ordering::Acquire)
                .min(self.inner.job_due(&slot).unwrap_or(NO_TICK));
            if next <= now && slot.is_running() {
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
            .filter(|slot| slot.is_running())
            .map(|slot| {
                slot.next_tick_ns
                    .load(Ordering::Acquire)
                    .min(self.inner.job_due(slot).unwrap_or(NO_TICK))
            })
            .filter(|next| *next != NO_TICK)
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
    /// When the device at `index` was last bound, on the host's clock (ns). Zero when it never was.
    /// Read by the daemon to say how long a device took to come up (see [`Host::look`]).
    pub fn bound_ns(&self, index: u32) -> Option<u64> {
        let slot = self.inner.slot(index)?;
        Some(slot.bound_ns.load(Ordering::Acquire))
    }

    /// When the device at `index` first ran with health Ok in its current bind, on the host's clock
    /// (ns), and how exactly. A device with no device needs is stamped by its own run: at its bind,
    /// tick or call, the moment the host ran the driver and saw Ok. A device whose Ok depends on its
    /// children is stamped by the first [`Host::look`] that finds it running ok, so the stamp is the
    /// time of that look (up to one look interval after the transition). `None` until the bind is
    /// stamped, and again from each bind. Read by the daemon to say how long bring-up took.
    pub fn running_ok_ns(&self, index: u32) -> Option<u64> {
        let slot = self.inner.slot(index)?;
        slot.running_ok_ns.load(Ordering::Acquire).checked_sub(1)
    }

    pub fn look(&self, index: u32) -> Option<(u64, Health)> {
        let slot = self.inner.slot(index)?;
        // The revision is read first, as in `info`.
        let revision = slot.revision.load(Ordering::Acquire);
        match self.inner.health_of(&slot, MAX_DEPTH) {
            (health, true) => {
                // The fallback stamp: a device found running ok that no driver run has stamped in
                // this bind is stamped now. The driver's own stamp, where it exists, is already set.
                if health == Health::Ok && slot.running_ok_ns.load(Ordering::Acquire) == 0 {
                    self.inner.set_running_ok(&slot);
                }
                Some((revision, health))
            }
            (_, false) => None,
        }
    }

    /// Hands `each` the notes device `index`'s driver has reported since the last take, oldest first,
    /// and returns how many were dropped since then. `None` when there is nothing to take now: the
    /// driver is in a call (its notes wait for the next take), it has no notes, or the device is not
    /// running. Allocates nothing.
    pub fn take_notes(&self, index: u32, each: &mut dyn FnMut(lemnos_core::Note)) -> Option<u32> {
        let slot = self.inner.slot(index)?;
        let mut guard = slot.instance.try_lock().ok()?;
        let notes = guard.as_mut()?.notes()?;
        while let Some(note) = notes.pop() {
            each(note);
        }
        Some(notes.take_dropped())
    }

    /// Switches a device off. A driver that keeps its hardware across off (`HOLDS_OFF`) is asked
    /// to switch off in place: the instance and every line it claimed stay as they are, and only
    /// [`Host::on`] reaches it again. Any other driver, and a driver whose `off` fails, has its
    /// instance dropped, so it lets go of its hardware.
    pub fn off(&self, id: &str) -> core::result::Result<(), HostError> {
        let slot = self.require(id)?;
        self.inner.forget_record(&slot);
        *lock(&slot.state) = DeviceState::Off(OffReason::Manual);
        self.inner.moved(&slot);
        self.inner.end_jobs(&slot, Status::INTERRUPTED);
        slot.drop_kept("the device is switched off");
        // The hold ends first, and its holder has a moment to close its copies of the pair
        // before the function goes: a holder that lets go in time never sees its endpoints fail
        // under it.
        slot.end_close();
        slot.wait_for_release(RELEASE_WAIT);
        slot.stop_wait();
        let mut failed = None;
        // The instance is waited for, not tried. A tick or a call holds it while it runs, and a
        // `try_lock` that found it held skipped the switch-off altogether: the device reported
        // off, its END hung up, and the driver (and the hardware it held) stayed up. The request
        // is answered once the driver has been told, so the answer means the device is off.
        // A call that never returned holds the driver for good: there is nothing left to switch off.
        let Some(mut guard) = instance_for_teardown(&slot) else {
            return Ok(());
        };
        let held = match guard.as_mut() {
            Some(instance) if instance.holds_off() => {
                match catch_unwind(AssertUnwindSafe(|| instance.off())) {
                    Ok(Ok(())) => true,
                    Ok(Err(status)) => {
                        failed = Some(status);
                        false
                    }
                    // A driver that panics is never trusted again: it is dropped below.
                    Err(_) => {
                        failed = Some(Status::PANIC);
                        false
                    }
                }
            }
            _ => false,
        };
        if !held {
            let dead = guard.take();
            drop(guard);
            drop(catch_unwind(AssertUnwindSafe(move || drop(dead))));
        }
        if let Some(status) = failed {
            *lock(&slot.state) = DeviceState::Off(OffReason::Failed(status));
            self.inner.moved(&slot);
        }
        Ok(())
    }

    /// Switches a device on, whatever turned it off, and binds it now. A kept instance (see
    /// [`Host::off`]) is switched on in place, with no rebind; if its `on` fails, it is dropped and
    /// the device is bound again under the failure rule.
    pub fn on(&self, id: &str) -> core::result::Result<(), HostError> {
        let slot = self.require(id)?;
        // A switch on is a fresh start: what a rebind kept is not carried over it.
        self.inner.forget_record(&slot);
        slot.short_runs.store(0, Ordering::Release);
        if matches!(*lock(&slot.state), DeviceState::Off(_)) {
            let switched = match slot.instance.try_lock() {
                Ok(mut guard) => match guard.as_mut() {
                    Some(instance) => catch_unwind(AssertUnwindSafe(|| instance.on()))
                        .map(|result| result.is_ok())
                        .unwrap_or(false),
                    None => false,
                },
                Err(_) => false,
            };
            if switched {
                *lock(&slot.state) = DeviceState::Running;
                slot.bound_ns
                    .store(self.inner.clock.now_ns(), Ordering::Release);
                self.inner.moved(&slot);
                self.inner
                    .set_next_tick(&slot, Some(self.inner.clock.now_ns()));
                return Ok(());
            }
        }
        // Not announced: the bind that follows announces the device once, as running, or as
        // retrying with the reason its bind gave. Announcing "switched on" first would be a
        // second state change for one power-on.
        *lock(&slot.state) = DeviceState::Backoff {
            attempts: 0,
            next_retry_ns: 0,
            reason: "switched on".to_string(),
            kind: None,
        };
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
            slot.drop_kept("the host shuts down");
            slot.end_close();
            slot.stop_wait();
            // Waited for: a tick running now must not keep the driver (and its hardware) up.
            if let Some(mut guard) = instance_for_teardown(slot) {
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

    /// The board file's `[board] id`: the board this host serves.
    pub fn board_id(&self) -> &str {
        &self.inner.board
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod security_facts_tests {
    use super::*;

    fn facts() -> Vec<(String, OwnedValue)> {
        vec![
            ("record".into(), OwnedValue::Str("/r".into())),
            ("keys".into(), OwnedValue::Str("/k".into())),
            ("orion".into(), OwnedValue::Bool(false)),
        ]
    }

    fn names(config: &[(String, OwnedValue)]) -> Vec<&str> {
        config.iter().map(|(key, _)| key.as_str()).collect()
    }

    #[test]
    fn the_security_device_gets_every_fact_and_slots_and_flash_get_only_the_keys() {
        let mut security = Vec::new();
        add_security_facts("s", &mut security, &["system.security/1"], &facts()).unwrap();
        assert_eq!(names(&security), ["record", "keys", "orion"]);
        for tag in ["system.slots/1", "system.flash/1"] {
            let mut config = Vec::new();
            add_security_facts("d", &mut config, &[tag], &facts()).unwrap();
            assert_eq!(names(&config), ["keys"], "{tag}");
            assert_eq!(config[0].1, OwnedValue::Str("/k".into()));
        }
        let mut other = Vec::new();
        add_security_facts("o", &mut other, &["power.sensor/1"], &facts()).unwrap();
        assert!(other.is_empty(), "no other device gets a security fact");
    }

    #[test]
    fn the_way_back_goes_to_the_restart_devices_only_and_one_of_their_own_is_refused() {
        let mut given = facts();
        given.push((LOOP_RETURN.into(), OwnedValue::Str("rpiboot".into())));
        let mut reboot = Vec::new();
        add_security_facts("r", &mut reboot, &["system.reboot/1"], &given).unwrap();
        assert_eq!(names(&reboot), [LOOP_RETURN]);
        assert_eq!(reboot[0].1, OwnedValue::Str("rpiboot".into()));
        for tag in [
            "system.security/1",
            "system.slots/1",
            "system.flash/1",
            "power.sensor/1",
        ] {
            let mut config = Vec::new();
            add_security_facts("d", &mut config, &[tag], &given).unwrap();
            assert!(
                !names(&config).contains(&LOOP_RETURN),
                "{tag}: {:?}",
                names(&config)
            );
        }
        let mut own = vec![(LOOP_RETURN.to_string(), OwnedValue::Str("rpiboot".into()))];
        assert_eq!(
            add_security_facts("r", &mut own, &["system.reboot/1"], &facts()),
            Err(BoardError::Config("r".into(), LOOP_RETURN.into()))
        );
    }

    #[test]
    fn a_device_that_names_its_own_key_list_is_refused_even_with_no_trust_root() {
        let own = vec![("keys".to_string(), OwnedValue::Str("/mine".into()))];
        for tag in ["system.slots/1", "system.flash/1", "system.security/1"] {
            for security in [facts(), Vec::new()] {
                let mut config = own.clone();
                let refused = add_security_facts("d", &mut config, &[tag], &security);
                assert_eq!(
                    refused,
                    Err(BoardError::Config("d".into(), "keys".into())),
                    "{tag}"
                );
            }
        }
        let mut config = own;
        add_security_facts("o", &mut config, &["power.sensor/1"], &facts()).unwrap();
        assert_eq!(config.len(), 1, "another device's own key is its own");
    }
}
