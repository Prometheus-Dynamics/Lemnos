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

/// What a [`Slot`]'s lock holds: the driver once bound, and where it stands.
#[derive(Debug)]
pub struct Device<D> {
    driver: Option<D>,
    state: State,
    /// When the driver next wants a tick.
    next_tick_ns: Option<u64>,
}

impl<D> Device<D> {
    pub const fn new() -> Self {
        Self {
            driver: None,
            state: State::Unbound,
            next_tick_ns: None,
        }
    }
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
    /// Runs a method, then the driver's tick if it has one.
    fn call(
        &self,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
        clock: &dyn HostClock,
    ) -> Result<usize>;
    /// Ticks the driver if its tick is due.
    fn tick(&self, now_ns: u64);
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
            .with(|device| device.state)
            .unwrap_or(State::Running)
    }

    fn bind(&self, res: &mut Resources<'_>, now_ns: u64, options: &Options) -> Result<()> {
        let missing = D::MANIFEST
            .needs
            .iter()
            .any(|need| !need.optional && !res.provided(need.name));
        let built = if missing {
            Err(Status::NO_DEVICE)
        } else {
            D::bind(res, &self.spec.config)
        };
        self.cell
            .with(|device| match built {
                Ok(driver) => {
                    device.driver = Some(driver);
                    device.state = State::Running;
                    device.next_tick_ns = D::TICKS.then_some(0);
                    Ok(())
                }
                Err(status) => {
                    let attempts = match device.state {
                        State::Backoff { attempts, .. } => attempts,
                        _ => 0,
                    };
                    device.state = State::Backoff {
                        attempts: attempts.saturating_add(1),
                        next_retry_ns: now_ns.saturating_add(options.backoff_ns(attempts)),
                        reason: status,
                    };
                    Err(status)
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
    ) -> Result<usize> {
        self.cell
            .with(|device| {
                if device.state != State::Running {
                    return Err(Status::NO_DEVICE);
                }
                let driver = device.driver.as_mut().ok_or(Status::NO_DEVICE)?;
                let mut writer = Reply::new(reply);
                let status = driver.dispatch(method, args, &mut writer);
                if D::TICKS {
                    device.next_tick_ns = driver.tick(clock.now_ns());
                }
                if status.is_ok() {
                    Ok(writer.len())
                } else {
                    Err(status)
                }
            })
            .unwrap_or(Err(Status::BUSY))
    }

    fn tick(&self, now_ns: u64) {
        if !D::TICKS {
            return;
        }
        self.cell.with(|device| {
            if device.state != State::Running || device.next_tick_ns.is_none_or(|due| due > now_ns)
            {
                return;
            }
            if let Some(driver) = device.driver.as_mut() {
                device.next_tick_ns = driver.tick(now_ns);
            }
        });
    }

    fn next_due(&self) -> Option<u64> {
        self.cell
            .with(|device| match device.state {
                State::Running => device.next_tick_ns,
                State::Backoff { next_retry_ns, .. } => Some(next_retry_ns),
                _ => None,
            })
            .flatten()
    }

    fn health(&self) -> Health {
        self.cell
            .with(|device| match device.state {
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
                device.state = match (off, device.driver.is_some()) {
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
