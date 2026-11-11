use core::marker::PhantomData;

use lemnos_core::{
    Config, Description, Driver, Health, HostClock, Manifest, MethodId, Reply, Resources, Result,
    Status,
};

use crate::{Lock, Options, State};

/// A device's line in the table: what the board file would say about it.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub id: &'static str,
    /// The bus a need with no entry in `uses` is given.
    pub bus: Option<&'static str>,
    /// Need name to bus name or device id.
    pub uses: &'static [(&'static str, &'static str)],
    pub link: Option<&'static str>,
    pub config: Config<'static>,
}

impl Spec {
    pub const fn new(id: &'static str) -> Self {
        Self {
            id,
            bus: None,
            uses: &[],
            link: None,
            config: Config::new(&[]),
        }
    }

    pub const fn bus(mut self, bus: &'static str) -> Self {
        self.bus = Some(bus);
        self
    }

    pub const fn uses(mut self, uses: &'static [(&'static str, &'static str)]) -> Self {
        self.uses = uses;
        self
    }

    pub const fn link(mut self, link: &'static str) -> Self {
        self.link = Some(link);
        self
    }

    pub const fn config(mut self, config: Config<'static>) -> Self {
        self.config = config;
        self
    }

    /// What `uses` names for `need`.
    pub fn used(&self, need: &str) -> Option<&'static str> {
        self.uses
            .iter()
            .find(|(name, _)| *name == need)
            .map(|(_, target)| *target)
    }
}

/// Where a device stands, apart from its driver. Nothing here depends on the driver's type, so
/// the code that keeps it is in the firmware once however many drivers there are.
#[derive(Debug)]
struct Standing {
    state: State,
    /// When the driver next wants a tick.
    next_tick_ns: Option<u64>,
    /// Calls in a row that failed as a device that has gone does.
    errors: u32,
    /// When the driver last bound.
    bound_ns: u64,
    /// Runs in a row that ended sooner than the retry cap after their bind: a driver that goes
    /// straight after each bind waits longer each time, as a bind that fails does.
    short_runs: u32,
}

impl Standing {
    const fn new() -> Self {
        Self {
            state: State::Unbound,
            next_tick_ns: None,
            errors: 0,
            bound_ns: 0,
            short_runs: 0,
        }
    }

    /// The driver bound.
    #[inline(never)]
    fn bound(&mut self, ticks: bool, now_ns: u64) {
        self.state = State::Running;
        self.next_tick_ns = ticks.then_some(0);
        self.errors = 0;
        self.bound_ns = now_ns;
    }

    fn wait(&mut self, attempts: u32, now_ns: u64, options: &Options, reason: Status) {
        self.state = State::Backoff {
            attempts: attempts.saturating_add(1),
            next_retry_ns: now_ns.saturating_add(options.backoff_ns(attempts)),
            reason,
        };
    }

    /// A bind failed: the device waits, twice as long as after the last failure.
    #[inline(never)]
    fn bind_failed(&mut self, now_ns: u64, options: &Options, reason: Status) {
        let attempts = match self.state {
            State::Backoff { attempts, .. } => attempts,
            _ => 0,
        };
        self.wait(attempts, now_ns, options, reason);
    }

    /// The driver was dropped: the device waits to be bound again under the failure rule.
    #[inline(never)]
    fn let_go(&mut self, now_ns: u64, options: &Options) {
        self.next_tick_ns = None;
        self.errors = 0;
        let ran = now_ns.saturating_sub(self.bound_ns);
        let attempts = if ran < options.cap_ns() {
            let attempts = self.short_runs;
            self.short_runs = self.short_runs.saturating_add(1);
            attempts
        } else {
            self.short_runs = 0;
            0
        };
        self.wait(attempts, now_ns, options, Status::NO_DEVICE);
    }

    /// Counts a call's answer. Whether the driver has to go: it says it is gone, or its calls
    /// have failed as a gone device's do too many times in a row.
    #[inline(never)]
    fn after_call(&mut self, status: Status, gone: bool, options: &Options) -> bool {
        if status.is_ok() {
            self.errors = 0;
        } else if counts_as_gone(status) {
            self.errors = self.errors.saturating_add(1);
        }
        gone || self.errors >= options.error_limit
    }
}

/// What a [`Slot`]'s lock holds: the driver once bound, and where it stands.
#[derive(Debug)]
pub struct Device<D> {
    driver: Option<D>,
    standing: Standing,
}

impl<D> Device<D> {
    pub const fn new() -> Self {
        Self {
            driver: None,
            standing: Standing::new(),
        }
    }

    /// Drops the driver, which gives back every bus it held, and leaves the device waiting to
    /// be bound again.
    fn let_go(&mut self, now_ns: u64, options: &Options) {
        self.driver = None;
        self.standing.let_go(now_ns, options);
    }
}

/// Whether a failed call is how a device that has gone away fails.
fn counts_as_gone(status: Status) -> bool {
    status == Status::IO || status == Status::NO_DEVICE || status == Status::TIMED_OUT
}

impl<D> Default for Device<D> {
    fn default() -> Self {
        Self::new()
    }
}

/// One device of driver `D`, in a lock `L` the firmware supplies.
pub struct Slot<D, L> {
    spec: Spec,
    cell: L,
    driver: PhantomData<fn() -> D>,
}

impl<D, L> Slot<D, L> {
    pub const fn new(spec: Spec, cell: L) -> Self {
        Self {
            spec,
            cell,
            driver: PhantomData,
        }
    }
}

/// A row of the [`Table`](crate::Table), whatever its driver. [`Slot`] is the only
/// implementation; the table holds rows as `&'static dyn Entry` so it needs no type per driver.
pub trait Entry: Sync {
    fn spec(&self) -> &Spec;
    fn manifest(&self) -> Manifest;
    fn state(&self) -> State;
    /// Builds the driver. On failure the device waits for its next retry.
    fn bind(&self, res: &mut Resources<'_>, now_ns: u64, options: &Options) -> Result<()>;
    /// Runs a method, then the driver's tick if it has one. A driver that then says what it
    /// was bound with is gone ([`Health::Gone`]), or whose calls have failed as a gone device's
    /// do [`Options::error_limit`] times in a row, is dropped and waits to be bound again. The
    /// call's own answer stands.
    fn call(
        &self,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
        clock: &dyn HostClock,
        options: &Options,
    ) -> Result<usize>;
    /// Ticks the driver if its tick is due, and drops it if it then says it is gone.
    fn tick(&self, now_ns: u64, options: &Options);
    /// When this device next wants a tick or a retry.
    fn next_due(&self) -> Option<u64>;
    /// The device's own health, leaving out the devices it uses.
    fn health(&self) -> Health;
    fn describe(&self, out: &mut Description) -> Result<()>;
    /// Stops calls to the device, keeping its driver, or lets them through again.
    fn set_off(&self, off: bool, now_ns: u64) -> Result<()>;
}

impl<D: Driver, L: Lock<Inner = Device<D>>> Entry for Slot<D, L> {
    fn spec(&self) -> &Spec {
        &self.spec
    }

    fn manifest(&self) -> Manifest {
        D::MANIFEST
    }

    fn state(&self) -> State {
        // A device whose lock is held is in a call right now, so it is running.
        self.cell
            .with(|device| device.standing.state)
            .unwrap_or(State::Running)
    }

    fn bind(&self, res: &mut Resources<'_>, now_ns: u64, options: &Options) -> Result<()> {
        let missing = D::MANIFEST
            .needs
            .iter()
            .any(|need| !need.optional && !res.provided(need.name));
        self.cell
            .with(|device| {
                // Built where it stays: the driver is never whole on the stack.
                device.driver = None;
                let built = if missing {
                    Err(Status::NO_DEVICE)
                } else {
                    D::bind_in(&mut device.driver, res, &self.spec.config)
                };
                match built {
                    Ok(()) => {
                        device.standing.bound(D::TICKS, now_ns);
                        Ok(())
                    }
                    Err(status) => {
                        // Whatever a failed bind left goes, and its buses with it.
                        device.driver = None;
                        device.standing.bind_failed(now_ns, options, status);
                        Err(status)
                    }
                }
            })
            .unwrap_or(Err(Status::BUSY))
    }

    fn call(
        &self,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
        clock: &dyn HostClock,
        options: &Options,
    ) -> Result<usize> {
        self.cell
            .with(|device| {
                if device.standing.state != State::Running {
                    return Err(Status::NO_DEVICE);
                }
                let driver = device.driver.as_mut().ok_or(Status::NO_DEVICE)?;
                let mut writer = Reply::new(reply);
                let status = driver.dispatch(method, args, &mut writer);
                let now_ns = clock.now_ns();
                if D::TICKS {
                    device.standing.next_tick_ns = driver.tick(now_ns);
                }
                let gone = matches!(driver.health(), Health::Gone(_));
                if device.standing.after_call(status, gone, options) {
                    device.let_go(now_ns, options);
                }
                if status.is_ok() {
                    Ok(writer.len())
                } else {
                    Err(status)
                }
            })
            .unwrap_or(Err(Status::BUSY))
    }

    fn tick(&self, now_ns: u64, options: &Options) {
        if !D::TICKS {
            return;
        }
        self.cell.with(|device| {
            let standing = &mut device.standing;
            if standing.state != State::Running
                || standing.next_tick_ns.is_none_or(|due| due > now_ns)
            {
                return;
            }
            if let Some(driver) = device.driver.as_mut() {
                standing.next_tick_ns = driver.tick(now_ns);
                if matches!(driver.health(), Health::Gone(_)) {
                    device.let_go(now_ns, options);
                }
            }
        });
    }

    fn next_due(&self) -> Option<u64> {
        self.cell
            .with(|device| match device.standing.state {
                State::Running => device.standing.next_tick_ns,
                State::Backoff { next_retry_ns, .. } => Some(next_retry_ns),
                _ => None,
            })
            .flatten()
    }

    fn health(&self) -> Health {
        self.cell
            .with(|device| match device.standing.state {
                State::Running => device
                    .driver
                    .as_ref()
                    .map_or(Health::Failed("not running"), Driver::health),
                State::Unbound => Health::Failed("not started"),
                State::Backoff { .. } => Health::Failed("retrying"),
                State::Off => Health::Failed("off"),
            })
            // In a call right now, so it's alive.
            .unwrap_or(Health::Ok)
    }

    fn describe(&self, out: &mut Description) -> Result<()> {
        self.cell
            .with(|device| match device.driver.as_ref() {
                Some(driver) => {
                    driver.describe(out);
                    Ok(())
                }
                None => Err(Status::NO_DEVICE),
            })
            .unwrap_or(Err(Status::BUSY))
    }

    fn set_off(&self, off: bool, now_ns: u64) -> Result<()> {
        self.cell
            .with(|device| {
                device.standing.state = match (off, device.driver.is_some()) {
                    (true, _) => State::Off,
                    (false, true) => State::Running,
                    // Never bound: bind on the next poll.
                    (false, false) => State::Backoff {
                        attempts: 0,
                        next_retry_ns: now_ns,
                        reason: Status::NO_DEVICE,
                    },
                };
            })
            .ok_or(Status::BUSY)
    }
}
