use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use lemnos_core::{
    CallerId, ClocksHandle, Config, ConfigValue, Description, DmaHandle, Driver, Health, Hint,
    HostClock, I2c, InterruptHandle, ItemKind, Line, LocalCaller, Manifest, MethodId, NeedKind,
    Provider, Pwm, RawSpiBus, RegistersHandle, Reply, ResetHandle, Resources, Result, Router, Spi,
    Status, StreamHandle, Tag, Uart, UsbDeviceHandle, UsbFunctionHandle,
};

use crate::Options;
use crate::board::{BoardError, BoardFile, DeviceSpec, OwnedValue, Owner};
use crate::contain::{Lane, LaneError, Work};
use crate::stream::{
    HeapAllocator, Lease, ProducerEndpoint, StreamAllocator, StreamMemory, StreamStats, Streams,
};

trait Instance: Send {
    fn describe(&self, out: &mut Description);
    fn health(&self) -> Health;
    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status;
    fn tick(&mut self, now_ns: u64) -> Option<u64>;
    fn caller(&mut self, caller: CallerId);
    fn caller_gone(&mut self, caller: CallerId);
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

    fn caller(&mut self, caller: CallerId) {
        Driver::caller(self, caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        Driver::caller_gone(self, caller);
    }
}

type Factory = fn(&mut Resources<'_>, &Config<'_>) -> Result<Box<dyn Instance>>;

/// A driver the host can start.
#[derive(Clone, Copy)]
pub struct DriverEntry {
    manifest: Manifest,
    factory: Factory,
    ticks: bool,
    tracks_callers: bool,
}

impl DriverEntry {
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
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
        factory: make::<D>,
        ticks: D::TICKS,
        tracks_callers: D::TRACKS_CALLERS,
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
}

struct Slot {
    index: u32,
    /// Which lane runs this device: its bus, or itself when it has none.
    lane_key: String,
    spec: DeviceSpec,
    config: Vec<(String, OwnedValue)>,
    entry: Option<DriverEntry>,
    instance: Mutex<Option<Box<dyn Instance>>>,
    state: Mutex<DeviceState>,
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
}

const NO_TICK: u64 = u64::MAX;

fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Slot {
    fn is_running(&self) -> bool {
        matches!(*lock(&self.state), DeviceState::Running)
    }

    fn ticks(&self) -> bool {
        self.entry.is_some_and(|entry| entry.ticks)
    }

    fn tracks_callers(&self) -> bool {
        self.entry.is_some_and(|entry| entry.tracks_callers)
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

struct Inner {
    slots: RwLock<Vec<Arc<Slot>>>,
    lanes: Mutex<HashMap<String, Arc<Lane>>>,
    /// Goes up whenever any device changes state.
    changes: AtomicU64,
    backend: Mutex<Box<dyn Provider + Send>>,
    clock: Arc<dyn HostClock>,
    options: Options,
    drivers: BTreeMap<&'static str, DriverEntry>,
    /// Called when a device's next tick moves earlier.
    waker: OnceLock<Box<dyn Fn() + Send + Sync>>,
    streams: Streams,
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
}

impl CallWork {
    pub(crate) fn run(self, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        self.inner
            .call_here(&self.slot, self.caller, self.method, args, reply)
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

    /// The device stopped working: let go of it and schedule a retry.
    ///
    /// The wait starts again from the shortest only once a driver has run for as long as the
    /// retry cap. One that fails sooner than that after each bind waits twice as long each time,
    /// up to the cap, so no driver can keep the host binding it.
    fn fault(&self, slot: &Slot, reason: &str) {
        let mut state = lock(&slot.state);
        let attempts = match &*state {
            DeviceState::Off(_) => return,
            DeviceState::Backoff { attempts, .. } => *attempts,
            DeviceState::Running => {
                let ran = self
                    .clock
                    .now_ns()
                    .saturating_sub(slot.bound_ns.load(Ordering::Acquire));
                if u128::from(ran) < self.options.retry_cap.as_nanos() {
                    slot.short_runs.fetch_add(1, Ordering::AcqRel)
                } else {
                    slot.short_runs.store(0, Ordering::Release);
                    0
                }
            }
        };
        *state = self.next_state(attempts, reason);
        self.changes.fetch_add(1, Ordering::Release);
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

    fn next_state(&self, attempts: u32, reason: &str) -> DeviceState {
        if let Some(policy) = self.options.auto_off
            && attempts + 1 >= policy.after_attempts
        {
            return DeviceState::Off(OffReason::Policy);
        }
        let wait = self.options.backoff(attempts).as_nanos() as u64;
        DeviceState::Backoff {
            attempts: attempts + 1,
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

    /// Ticks a device on this thread, inside the panic boundary.
    fn tick_here(&self, slot: &Slot) {
        if slot.hung.load(Ordering::Acquire) {
            return;
        }
        let mut guard = lock(&slot.instance);
        let Some(instance) = guard.as_mut() else {
            return;
        };
        let now = self.clock.now_ns();
        let ticked = catch_unwind(AssertUnwindSafe(|| {
            let next = instance.tick(now);
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
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        if slot.hung.load(Ordering::Acquire) {
            return Err(Status::NO_DEVICE);
        }
        let mut guard = lock(&slot.instance);
        let Some(instance) = guard.as_mut() else {
            return Err(Status::NO_DEVICE);
        };
        let mut writer = Reply::new(reply);
        let ticks = slot.ticks();
        let tracks_callers = slot.tracks_callers();
        let guest = slot.spec.owner == Owner::Host;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if guest && !only_reads(instance.as_ref(), method) {
                return (NOT_OURS, None);
            }
            if tracks_callers {
                instance.caller(caller);
            }
            let status = instance.dispatch(method, args, &mut writer);
            if ticks {
                self.set_next_tick(slot, instance.tick(self.clock.now_ns()));
            }
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

    /// Tells a device, on its lane and inside the panic boundary, that a caller has gone.
    fn caller_gone(&'static self, slot: Arc<Slot>, caller: CallerId) {
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
                instance.caller_gone(caller);
                let next = target.ticks().then(|| instance.tick(self.clock.now_ns()));
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

    fn call(
        &'static self,
        slot: Arc<Slot>,
        caller: CallerId,
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
            let config = Config::new(&entries);
            let mut binding = Binding {
                inner: self,
                slot: &target,
                missing: RefCell::new(Vec::new()),
            };
            let link = target.spec.link.as_deref();
            let built = catch_unwind(AssertUnwindSafe(|| {
                let mut res = Resources::new(&mut binding, link);
                (entry.factory)(&mut res, &config)
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
                self.changes.fetch_add(1, Ordering::Release);
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
        *state = self.next_state(attempts, reason);
        self.changes.fetch_add(1, Ordering::Release);
    }

    fn device_health(&self, slot: &Slot, depth: u32) -> Health {
        let own = {
            let state = lock(&slot.state).clone();
            match state {
                DeviceState::Running => match slot.instance.try_lock() {
                    Ok(mut guard) => match guard.as_mut() {
                        Some(instance) => catch_unwind(AssertUnwindSafe(|| instance.health()))
                            .unwrap_or(Health::Failed("the driver panicked")),
                        None => Health::Failed("not running"),
                    },
                    // In a call right now, so it's alive.
                    Err(_) if slot.hung.load(Ordering::Acquire) => {
                        Health::Failed("a call never returned")
                    }
                    Err(_) => Health::Ok,
                },
                DeviceState::Backoff { .. } => Health::Failed("retrying"),
                DeviceState::Off(_) => Health::Failed("off"),
            }
        };
        if depth == 0 || matches!(own, Health::Failed(_) | Health::Gone(_)) {
            return own;
        }
        let Some(entry) = slot.entry else {
            return own;
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
                Some(child) => self.device_health(child, depth - 1),
                None => Health::Failed("missing"),
            };
            let named = slot.spec.uses.contains_key(need.name);
            match (child_health, need.optional) {
                (Health::Failed(_) | Health::Gone(_), false) => {
                    return Health::Failed("a required device failed");
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
        combined
    }
}

impl Router for Inner {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let slot = self.slot(device).ok_or(Status::NOT_FOUND)?;
        self.call_here(&slot, CallerId::LOCAL, method, args, reply)
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
    fn bus_name(&self, need: &str) -> Result<String> {
        self.slot
            .spec
            .uses
            .get(need)
            .or(self.slot.spec.bus.as_ref())
            .cloned()
            .ok_or(Status::NOT_FOUND)
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
}

macro_rules! buses {
    ($($method:ident -> $handle:ty;)*) => {$(
        fn $method(&mut self, need: &str) -> Result<$handle> {
            let bus = self.bus_name(need)?;
            lock(&self.inner.backend).$method(&bus)
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
        if !target.is_running() {
            return Err(Status::NO_DEVICE);
        }
        let contract = target.contract_index(tag).ok_or(Status::INVALID_ARGUMENT)?;
        Ok(LocalCaller::new(self.inner, target.index, contract))
    }

    buses! {
        i2c -> I2c;
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
        if self.slot.spec.streams.contains_key(need) {
            let endpoint = self.inner.streams.producer(self.slot.index, need)?;
            return Ok(Box::new(endpoint));
        }
        let bus = self.bus_name(need)?;
        lock(&self.inner.backend).stream(&bus)
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
        let order = bind_order(&board.devices)?;
        let table: BTreeMap<_, _> = drivers
            .iter()
            .map(|driver| (driver.manifest.name, *driver))
            .collect();
        let mut slots = Vec::with_capacity(board.devices.len());
        for (index, spec) in board.devices.iter().enumerate() {
            let state = if spec.disabled {
                DeviceState::Off(OffReason::Disabled)
            } else {
                DeviceState::Backoff {
                    attempts: 0,
                    next_retry_ns: 0,
                    reason: "starting".to_string(),
                }
            };
            slots.push(Arc::new(Slot {
                index: index as u32,
                lane_key: match &spec.bus {
                    Some(bus) => format!("bus:{bus}"),
                    None => format!("device:{}", spec.id),
                },
                config: spec.owned_config()?,
                entry: table.get(spec.driver.as_str()).copied(),
                spec: spec.clone(),
                instance: Mutex::new(None),
                state: Mutex::new(state),
                hung: AtomicBool::new(false),
                errors: AtomicU32::new(0),
                missing: Mutex::new(Vec::new()),
                next_tick_ns: AtomicU64::new(NO_TICK),
                tried_ns: AtomicU64::new(0),
                bound_ns: AtomicU64::new(0),
                short_runs: AtomicU32::new(0),
                hinted: AtomicBool::new(false),
            }));
        }

        let streams = Streams::new(allocator, &board.devices);

        // Lives as long as the process: drivers hold plain references to it.
        // `shutdown` lets every device go.
        let inner: &'static Inner = Box::leak(Box::new(Inner {
            slots: RwLock::new(slots),
            lanes: Mutex::new(HashMap::new()),
            changes: AtomicU64::new(0),
            backend: Mutex::new(backend),
            clock,
            options,
            drivers: table,
            waker: OnceLock::new(),
            streams,
        }));
        let host = Self { inner };
        let slots = inner.all();
        for at in order {
            if !board.devices[at].disabled {
                inner.bind(Arc::clone(&slots[at]));
            }
        }
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
        let state = lock(&slot.state).clone();
        DeviceInfo {
            index: slot.index,
            id: slot.spec.id.clone(),
            driver: slot.spec.driver.clone(),
            tags: slot
                .entry
                .map(|e| e.manifest.provides.iter().map(|c| c.tag).collect())
                .unwrap_or_default(),
            state,
            health: self.inner.device_health(slot, MAX_DEPTH),
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
    /// is `NOT_PERMITTED`, and so is one from another driver.
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
        self.inner.call(slot, caller, method, args, reply)
    }

    /// Tells every driver that tracks callers that `caller` has gone. Blocks until each has
    /// answered or its call deadline has passed.
    pub fn caller_gone(&self, caller: CallerId) {
        self.inner.streams.caller_gone(caller);
        let mut index = 0;
        while let Some(slot) = self.inner.slot(index) {
            index += 1;
            if slot.tracks_callers() && slot.is_running() {
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
        let mut index = 0;
        let mut any = false;
        while let Some(slot) = self.inner.slot(index) {
            index += 1;
            any |= match hint {
                Hint::Any => self.inner.hinted(&slot),
                Hint::Changed(name) => slot.names(name) && self.inner.hinted(&slot),
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

    /// When the next retry is due, if one is waiting.
    pub fn next_retry_ns(&self) -> Option<u64> {
        self.inner
            .slots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(|slot| match &*lock(&slot.state) {
                DeviceState::Backoff { next_retry_ns, .. } => Some(*next_retry_ns),
                _ => None,
            })
            .min()
    }

    /// Goes up whenever any device changes state, so a caller can tell that
    /// [`Host::devices`] is worth reading again.
    pub fn changes(&self) -> u64 {
        self.inner.changes.load(Ordering::Acquire)
    }

    /// Switches a device off and lets go of its hardware.
    pub fn off(&self, id: &str) -> core::result::Result<(), HostError> {
        let slot = self.require(id)?;
        *lock(&slot.state) = DeviceState::Off(OffReason::Manual);
        self.inner.changes.fetch_add(1, Ordering::Release);
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
        self.inner.changes.fetch_add(1, Ordering::Release);
        self.inner.bind(slot);
        Ok(())
    }

    /// Lets go of every device, parents first, so each releases its hardware.
    pub fn shutdown(&self) {
        self.inner.streams.close_all();
        let slots = self.inner.all();
        for slot in slots.iter().rev() {
            *lock(&slot.state) = DeviceState::Off(OffReason::Manual);
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
