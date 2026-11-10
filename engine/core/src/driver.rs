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
    /// What the driver was bound with is dead for good, and only a new bind can bring it back: a
    /// handle to a device that was unplugged, say. The host drops the driver and binds it again
    /// under the failure rule, as it does a bind that failed. A driver that can recover by itself
    /// reports [`Health::Failed`] and keeps trying; this is for when it can't.
    ///
    /// The host looks after every method call and every tick, so a driver that only ticks can
    /// ask too. Asking again straight after each bind is answered more and more slowly, up to the
    /// retry cap.
    Gone(&'static str),
}

/// Who is calling a driver. Numbers mean nothing except that one caller keeps its own for as long
/// as it is connected, and they aren't reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallerId(pub u64);

impl CallerId {
    /// A caller in the same process: another driver, or the host itself. It never goes away.
    pub const LOCAL: Self = Self(0);
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
    /// Whether the host should say who is calling. See [`Lifecycle::TRACKS_CALLERS`].
    const TRACKS_CALLERS: bool = false;
    /// See [`Lifecycle::caller`].
    fn caller(&mut self, _caller: CallerId) {}
    /// See [`Lifecycle::caller_gone`].
    fn caller_gone(&mut self, _caller: CallerId) {}
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
    /// Set this to `true` for a driver that holds something on a caller's behalf and has to give
    /// it up when that caller goes. Other drivers cost the host nothing.
    const TRACKS_CALLERS: bool = false;
    /// Says who the method about to run is for. Called just before every method, so a driver
    /// keeps the value in a field and reads it from the method.
    fn caller(&mut self, _caller: CallerId) {}
    /// `caller` has gone, by closing its connection or by dying. Give up what it held. Runs on
    /// the driver's lane, one call at a time with its methods, and must not block.
    fn caller_gone(&mut self, _caller: CallerId) {}
}

/// Time as the host sees it, supplied by the backend so a simulated board can
/// run on simulated time.
pub trait HostClock: Send + Sync {
    /// Nanoseconds since an arbitrary fixed point. Never goes backwards.
    fn now_ns(&self) -> u64;
}
