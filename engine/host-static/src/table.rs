use lemnos_core::{
    ClocksHandle, Description, DmaHandle, Health, HostClock, I2c, InterruptHandle, Line,
    LocalCaller, Manifest, MethodId, NeedKind, Provider, Pwm, RawSpiBus, RegistersHandle,
    ResetHandle, Resources, Result, Router, Spi, Status, StreamHandle, Tag, Uart, UsbDeviceHandle,
    UsbFunctionHandle,
};

use crate::Entry;

/// The longest wait between two retries, whatever the options say.
const RETRY_CAP_NS: u64 = 8_000_000_000;

/// Deepest chain of devices health is passed up through.
const MAX_DEPTH: u32 = 16;

/// How the table retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub retry_first_ns: u64,
    /// Capped at 8 s.
    pub retry_cap_ns: u64,
    /// Calls in a row that fail as a device that has gone does (`IO`, `NO_DEVICE`,
    /// `TIMED_OUT`) before its driver is dropped and bound again.
    pub error_limit: u32,
}

impl Options {
    /// 1 s doubling to 8 s.
    pub const DEFAULT: Self = Self {
        retry_first_ns: 1_000_000_000,
        retry_cap_ns: RETRY_CAP_NS,
        error_limit: 5,
    };

    /// The longest wait between retries: the cap asked for, and never more than 8 s.
    pub const fn cap_ns(&self) -> u64 {
        if self.retry_cap_ns < RETRY_CAP_NS {
            self.retry_cap_ns
        } else {
            RETRY_CAP_NS
        }
    }

    /// The wait before retry number `attempt` (0 is the first).
    pub const fn backoff_ns(&self, attempt: u32) -> u64 {
        let cap = self.cap_ns();
        let doubled = match 1u64.checked_shl(attempt) {
            Some(factor) => self.retry_first_ns.saturating_mul(factor),
            None => cap,
        };
        if doubled < cap { doubled } else { cap }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Where a device stands. Every state can be read with [`Table::state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not tried yet.
    Unbound,
    Running,
    /// Its bind failed, or its driver was dropped because what it was bound with is gone; it
    /// is bound again at `next_retry_ns`, forever.
    Backoff {
        attempts: u32,
        next_retry_ns: u64,
        reason: Status,
    },
    /// Switched off with [`Table::off`].
    Off,
}

/// Every device of a firmware, in a fixed order: a device's number is its position.
pub struct Table {
    entries: &'static [&'static dyn Entry],
    clock: &'static dyn HostClock,
    options: Options,
}

impl Table {
    pub const fn new(
        entries: &'static [&'static dyn Entry],
        clock: &'static dyn HostClock,
        options: Options,
    ) -> Self {
        Self {
            entries,
            clock,
            options,
        }
    }

    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn entry(&self, device: u32) -> Result<&'static dyn Entry> {
        self.entries
            .get(device as usize)
            .copied()
            .ok_or(Status::NOT_FOUND)
    }

    /// The number of the device called `id`.
    pub fn index(&self, id: &str) -> Option<u32> {
        self.entries
            .iter()
            .position(|entry| entry.spec().id == id)
            .map(|at| at as u32)
    }

    pub fn id(&self, device: u32) -> Option<&'static str> {
        self.entry(device).ok().map(|entry| entry.spec().id)
    }

    /// The first device providing `tag` (`"name/major"`) at or after `from`.
    pub fn find(&self, tag: &str, from: u32) -> Option<u32> {
        (from..self.entries.len() as u32).find(|&device| {
            self.entries[device as usize]
                .manifest()
                .provides
                .iter()
                .any(|contract| contract.tag.as_str() == tag)
        })
    }

    pub fn manifest(&self, device: u32) -> Option<Manifest> {
        self.entry(device).ok().map(|entry| entry.manifest())
    }

    pub fn state(&self, device: u32) -> Option<State> {
        self.entry(device).ok().map(|entry| entry.state())
    }

    pub fn link(&self, device: u32) -> Option<&'static str> {
        self.entry(device).ok().and_then(|entry| entry.spec().link)
    }

    /// Binds every device, children first. A device that can't bind is left retrying and never
    /// stops the others.
    pub fn start(&'static self, buses: &mut dyn Provider) {
        let now = self.clock.now_ns();
        for _ in 0..self.entries.len() {
            let mut bound = false;
            for device in 0..self.entries.len() {
                if self.entries[device].state() == State::Unbound && self.children_tried(device) {
                    self.bind(device, buses, now);
                    bound = true;
                }
            }
            if !bound {
                break;
            }
        }
        // Devices that use each other in a circle.
        for device in 0..self.entries.len() {
            if self.entries[device].state() == State::Unbound {
                self.bind(device, buses, now);
            }
        }
    }

    /// Whether every device `device` uses has been tried.
    fn children_tried(&self, device: usize) -> bool {
        self.entries[device]
            .spec()
            .uses
            .iter()
            .all(|(_, target)| match self.index(target) {
                Some(child) if child as usize != device => {
                    self.entries[child as usize].state() != State::Unbound
                }
                _ => true,
            })
    }

    fn bind(&'static self, device: usize, buses: &mut dyn Provider, now_ns: u64) {
        let entry = self.entries[device];
        let mut binding = Binding {
            table: self,
            entry,
            buses,
        };
        let mut res = Resources::new(&mut binding, entry.spec().link);
        // A failure is recorded in the device's state.
        let _ = entry.bind(&mut res, now_ns, &self.options);
    }

    /// Retries the binds and runs the ticks that are due, and returns when it next has
    /// something to do, on the clock's scale.
    pub fn poll(&'static self, buses: &mut dyn Provider) -> Option<u64> {
        let now = self.clock.now_ns();
        let mut next: Option<u64> = None;
        for device in 0..self.entries.len() {
            let entry = self.entries[device];
            if let State::Backoff { next_retry_ns, .. } = entry.state()
                && next_retry_ns <= now
            {
                self.bind(device, buses, now);
            }
            entry.tick(now, &self.options);
            if let Some(due) = entry.next_due() {
                next = Some(next.map_or(due, |next| next.min(due)));
            }
        }
        next
    }

    /// Runs a method of `device` and returns the length of its reply.
    pub fn call(
        &self,
        device: u32,
        method: MethodId,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize> {
        self.entry(device)?
            .call(method, args, reply, self.clock, &self.options)
    }

    pub fn describe(&self, device: u32, out: &mut Description) -> Result<()> {
        self.entry(device)?.describe(out)
    }

    /// The device's health, with the devices it uses passed up.
    pub fn health(&self, device: u32) -> Health {
        self.health_at(device, MAX_DEPTH)
    }

    fn health_at(&self, device: u32, depth: u32) -> Health {
        let Ok(entry) = self.entry(device) else {
            return Health::Failed("missing");
        };
        let own = entry.health();
        if depth == 0 || matches!(own, Health::Failed(_) | Health::Gone(_)) {
            return own;
        }
        let mut combined = own;
        for need in entry.manifest().needs {
            if !matches!(need.kind, NeedKind::Device(_)) {
                continue;
            }
            let used = entry.spec().used(need.name);
            let child = match used.and_then(|id| self.index(id)) {
                Some(child) => self.health_at(child, depth - 1),
                None => Health::Failed("missing"),
            };
            match (child, need.optional) {
                (Health::Failed(_) | Health::Gone(_), false) => {
                    return Health::Failed("a required device failed");
                }
                (Health::Failed(_) | Health::Gone(_), true)
                    if used.is_some() && combined == Health::Ok =>
                {
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

    /// Stops calls to a device. Its driver is kept, so [`Table::on`] is immediate.
    pub fn off(&self, device: u32) -> Result<()> {
        self.entry(device)?.set_off(true, self.clock.now_ns())
    }

    /// Lets calls through to a device that was switched off. One that never bound is bound on
    /// the next poll.
    pub fn on(&self, device: u32) -> Result<()> {
        let entry = self.entry(device)?;
        if entry.state() != State::Off {
            return Ok(());
        }
        entry.set_off(false, self.clock.now_ns())
    }
}

impl Router for Table {
    fn call(&self, device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        Table::call(self, device, method, args, reply)
    }

    fn health(&self, device: u32) -> Health {
        Table::health(self, device)
    }
}

/// Gives a binding driver what its spec says it may have.
struct Binding<'a> {
    table: &'static Table,
    entry: &'static dyn Entry,
    buses: &'a mut dyn Provider,
}

impl Binding<'_> {
    fn bus_name(&self, need: &str) -> Result<&'static str> {
        let spec = self.entry.spec();
        spec.used(need).or(spec.bus).ok_or(Status::NOT_FOUND)
    }
}

macro_rules! buses {
    ($($method:ident -> $handle:ty;)*) => {$(
        fn $method(&mut self, need: &str) -> Result<$handle> {
            let bus = self.bus_name(need)?;
            self.buses.$method(bus)
        }
    )*};
}

impl Provider for Binding<'_> {
    fn provided(&self, need: &str) -> bool {
        let spec = self.entry.spec();
        match spec.used(need) {
            // A name that isn't a bus is a device.
            Some(target) if !self.buses.provided(target) => {
                self.table.index(target).is_some_and(|device| {
                    self.table.entries[device as usize].state() == State::Running
                })
            }
            Some(_) => true,
            None => spec.bus.is_some_and(|bus| self.buses.provided(bus)),
        }
    }

    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller> {
        let target = self.entry.spec().used(need).ok_or(Status::NOT_FOUND)?;
        let device = self.table.index(target).ok_or(Status::NOT_FOUND)?;
        let entry = self.table.entries[device as usize];
        if entry.state() != State::Running {
            return Err(Status::NO_DEVICE);
        }
        let contract = entry
            .manifest()
            .provides
            .iter()
            .position(|contract| contract.tag == tag)
            .ok_or(Status::INVALID_ARGUMENT)?;
        Ok(LocalCaller::new(self.table, device, contract as u8))
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
        stream -> StreamHandle;
    }
}
