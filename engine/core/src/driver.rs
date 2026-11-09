use crate::{Config, ContractInfo, Description, Need, Resources, Result, Status, Writer};

/// A driver's reply buffer.
pub type Reply<'a> = Writer<'a>;

/// A method of one of a device's contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MethodId {
    /// Position in the driver's [`Manifest::provides`].
    pub contract: u8,
    pub method: u16,
}

/// What a driver needs and provides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Manifest {
    pub name: &'static str,
    pub needs: &'static [Need],
    pub provides: &'static [ContractInfo],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Health {
    #[default]
    Ok,
    /// Working, with something missing.
    Degraded(&'static str),
    Failed(&'static str),
}

/// A driver as the host sees it. `#[driver]` writes this; handwritten
/// implementations are for tests.
pub trait Driver: Sized {
    const MANIFEST: Manifest;

    fn bind(res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Self>;
    fn describe(&self, out: &mut Description);
    fn health(&self) -> Health {
        Health::Ok
    }
    /// Runs a method. `args` and `reply` mean something only to the contract.
    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status;
    /// Whether the host should call [`Driver::tick`]. See [`Lifecycle::TICKS`].
    const TICKS: bool = false;
    /// See [`Lifecycle::tick`].
    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        None
    }
}

/// The part of a driver its author writes when using `#[driver]`.
pub trait Lifecycle: Sized {
    /// The struct `#[driver]` generates from the `#[needs]` fields.
    type Needs;

    fn start(needs: Self::Needs, cfg: &Config<'_>) -> Result<Self>;
    fn describe(&self, _out: &mut Description) {}
    fn health(&self) -> Health {
        Health::Ok
    }
    /// Set this to `true` to have [`Lifecycle::tick`] called. Other drivers cost the host nothing.
    const TICKS: bool = false;
    /// Does whatever is due at `now_ns`, and returns when to be called next, or `None` to wait
    /// until something changes. `now_ns` is the host's clock, on the same scale as the time
    /// returned.
    ///
    /// The host calls this once after the driver starts, when the time it returned arrives, and
    /// after every method call, so a method can start or stop timed work by changing what the
    /// next `tick` returns. A tick can therefore come early: check what is actually due. It runs
    /// on the driver's lane, one call at a time with its methods, and must not block.
    fn tick(&mut self, _now_ns: u64) -> Option<u64> {
        None
    }
}

/// Time as the host sees it, supplied by the backend so a simulated board can
/// run on simulated time.
pub trait HostClock: Send + Sync {
    /// Nanoseconds since an arbitrary fixed point. Never goes backwards.
    fn now_ns(&self) -> u64;
}
