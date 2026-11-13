use crate::{
    Config, ContractInfo, Decode, Description, Encode, Need, Reader, Resources, Result, Shape,
    Status, Writer,
};
use lemnos_hal::blocks::BufferHandle;
use lemnos_hal::register::AddressWidth;

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

/// What a driver does on a device the board file gives to the host (`owner = "host"`): one the
/// operating system's own driver owns, which Lemnos may read and must not change.
///
/// The host can stop a write only where it stands between the driver and the hardware, which is
/// at the driver's needs. So it has to know whether that is everywhere the driver reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostOwned {
    /// The driver says nothing, so it is never bound to a host-owned device: the device stays
    /// unbound, and says why.
    #[default]
    Refused,
    /// Everything the driver does to hardware goes through its needs. The host hands those to
    /// a host-owned device as read-only, so no path in the driver can write, from `start`,
    /// `tick` or a method.
    ThroughNeeds,
    /// The driver reaches hardware by itself (a file, a device node), and on a host-owned
    /// device ([`Config::host_owned`]) opens it for reading only and writes nothing, so the
    /// operating system refuses a write it would attempt. Its needs are read-only as well.
    ReadsOnly,
}

/// Who is calling a driver. Numbers mean nothing except that one caller keeps its own for as long
/// as it is connected, and they aren't reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallerId(pub u64);

impl CallerId {
    /// A caller in the same process: another driver, or the host itself. It never goes away.
    pub const LOCAL: Self = Self(0);

    /// The caller a device is when it calls another device it uses (see [`crate::LocalCaller::by`]):
    /// its slot's index, in a range the daemon never numbers its connections from.
    pub const fn device(index: u32) -> Self {
        Self((1 << 62) | index as u64)
    }

    /// Whether this is a device's own caller, from [`CallerId::device`].
    pub const fn is_device(self) -> bool {
        self.0 & (1 << 62) != 0
    }
}

/// Where a board device comes from: the board file's own list, or hardware that comes and goes.
/// A device from a driver package is a board-file device like any other; the board decides
/// whether it is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Named by the board file's `[[devices]]`, on a bus that stays put.
    BuiltIn,
    /// Made by an `[[each]]` entry for a resource it found, or named on a bus that comes and goes
    /// (see [`Provider::hotplug`](crate::Provider::hotplug)). Absent is a normal state for it.
    Hotplug,
}

/// One device of the board, as a driver that watches the board sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardDevice<'a> {
    pub id: &'a str,
    pub origin: Origin,
    /// Bound now: its driver is running. A device waiting to retry, or switched off, is not.
    pub bound: bool,
    /// Its health as the host reports it now, with a dead device's [`Health::Gone`] apart from
    /// [`Health::Failed`].
    pub health: Health,
}

/// The board as the host has it now: every device the board names, in the host's order. The
/// host makes it fresh before each tick of a driver that watches the board, and reading it
/// allocates nothing.
pub trait BoardView {
    /// Calls `f` once for each device.
    fn each(&self, f: &mut dyn FnMut(BoardDevice<'_>));
}

/// A driver as the host sees it. `#[driver]` writes this; handwritten
/// implementations are for tests.
pub trait Driver: Sized {
    const MANIFEST: Manifest;

    fn bind(res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Self>;
    /// Binds into `place`, which is where the driver will live: a host with no allocator keeps
    /// each driver in a `static`, and a large one built on the stack and moved there costs the
    /// stack its whole size. `place` is `None` on the way in and `Some` only on success. The
    /// default builds with [`Driver::bind`] and moves the result.
    fn bind_in(place: &mut Option<Self>, res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<()> {
        *place = Some(Self::bind(res, cfg)?);
        Ok(())
    }
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
    /// See [`Lifecycle::wait_fd`].
    fn wait_fd(&self) -> Option<BufferHandle<'_>> {
        None
    }
    /// What the driver does on a host-owned device. See [`Lifecycle::HOST_OWNED`].
    const HOST_OWNED: HostOwned = HostOwned::Refused;
    /// See [`Lifecycle::WATCHES_BOARD`].
    const WATCHES_BOARD: bool = false;
    /// See [`Lifecycle::board`].
    fn board(&mut self, _view: &dyn BoardView) {}
    /// How its I2C chips name a register. See [`Lifecycle::I2C_REGISTER_ADDRESS`].
    const I2C_REGISTER_ADDRESS: Option<AddressWidth> = None;
    /// Whether the host should say who is calling. See [`Lifecycle::TRACKS_CALLERS`].
    const TRACKS_CALLERS: bool = false;
    /// See [`Lifecycle::caller`].
    fn caller(&mut self, _caller: CallerId) {}
    /// See [`Lifecycle::caller_gone`].
    fn caller_gone(&mut self, _caller: CallerId) {}
    /// See [`Lifecycle::endpoints`].
    fn endpoints(&mut self, _caller: CallerId) -> Result<[BufferHandle<'_>; 2]> {
        Err(Status::NOT_SUPPORTED)
    }
    /// See [`Lifecycle::release_endpoints`].
    fn release_endpoints(&mut self, _caller: CallerId) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    /// See [`Lifecycle::endpoints_ended`].
    fn endpoints_ended(&mut self) -> bool {
        false
    }
    /// Whether the driver runs long jobs. See [`Lifecycle::JOBS`].
    const JOBS: bool = false;
    /// See [`Lifecycle::job`].
    fn job(&mut self, _job: JobId, _result: &mut Reply<'_>) -> JobState {
        JobState::Finished(Status::NOT_FOUND)
    }
    /// See [`Lifecycle::job_cancel`].
    fn job_cancel(&mut self, _job: JobId) {}
}

/// The part of a driver its author writes when using `#[driver]`.
pub trait Lifecycle: Sized {
    /// The struct `#[driver]` generates from the `#[needs]` fields.
    type Needs;

    fn start(needs: Self::Needs, cfg: &Config<'_>) -> Result<Self>;
    /// [`Lifecycle::start`], into the place the driver will live. A driver whose state is large
    /// can write this to fill `place` field by field, so that it is never whole on the stack of
    /// a microcontroller; `place` must be `Some` exactly when this succeeds. The default starts
    /// by value and moves the result.
    fn start_in(place: &mut Option<Self>, needs: Self::Needs, cfg: &Config<'_>) -> Result<()> {
        *place = Some(Self::start(needs, cfg)?);
        Ok(())
    }
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
    /// A descriptor the host waits on in place of a tick deadline. When it becomes readable the host
    /// calls [`Lifecycle::tick`] at once, as if the driver's next tick were due, so a driver whose
    /// hardware raises a doorbell for each piece of work needs no poll. The host keeps a duplicate
    /// and reads it to clear each wake, before the tick runs; the driver keeps the descriptor for as
    /// long as it runs. A tick may find nothing: check what is due. `None` by default.
    fn wait_fd(&self) -> Option<BufferHandle<'_>> {
        None
    }
    /// Set this to `true` for a driver that needs to know the rest of the board: which devices
    /// there are, which are bound and how healthy each is. The host calls [`Lifecycle::board`]
    /// with a fresh view before each tick, so the driver reads it there and keeps what it
    /// needs. Other drivers cost the host nothing, and a driver package gets an empty view.
    const WATCHES_BOARD: bool = false;
    /// Given the board as it is now, before each tick when [`Lifecycle::WATCHES_BOARD`] is set.
    /// Must not block, and must not keep the view past the call.
    fn board(&mut self, _view: &dyn BoardView) {}
    /// Whether this driver can be bound to a device the operating system owns, and how it
    /// keeps from changing it. Left as it is, the driver is never bound to one.
    const HOST_OWNED: HostOwned = HostOwned::Refused;
    /// How the I2C chips this driver reads name a register: the width of the register address
    /// written before a register is read, or `None` for chips read without one.
    ///
    /// It matters only on a host-owned device, where a write on the bus could change what the
    /// operating system's driver set up. There the host passes an I2C transaction only when it
    /// reads: every write in it is exactly one register address of this width and is followed
    /// at once by a read (a repeated start, then the read), and the transaction ends with a
    /// read. A write of a register's value, a write with no read after it, and anything longer
    /// than an address are refused before the bus. With `None` only plain reads pass.
    ///
    /// The host can't tell a register address from a command byte, so this is the driver's
    /// word for its chips: set it only when a write of an address of this width does nothing on
    /// them but move the register pointer. A chip that takes one-byte commands (a reset, a
    /// conversion started by a write) is read with `None`, or not on a host-owned device.
    ///
    /// Moving the pointer is still a change the operating system's driver can see: one that
    /// sets the pointer in one transfer and reads it with a plain read in a later transfer can
    /// read the register Lemnos named in between. Lemnos's own write and read can't be split:
    /// they go out as one transfer, and the kernel holds the adapter for a whole transfer, so the
    /// operating system's driver can't land a transfer between them.
    const I2C_REGISTER_ADDRESS: Option<AddressWidth> = None;
    /// Set this to `true` for a driver that holds something on a caller's behalf and has to give
    /// it up when that caller goes. Other drivers cost the host nothing.
    const TRACKS_CALLERS: bool = false;
    /// Says who the method about to run is for. Called just before every method, so a driver
    /// keeps the value in a field and reads it from the method.
    fn caller(&mut self, _caller: CallerId) {}
    /// `caller` has gone, by closing its connection or by dying. Give up what it held. Runs on
    /// the driver's lane, one call at a time with its methods, and must not block.
    fn caller_gone(&mut self, _caller: CallerId) {}
    /// Lends `caller` the two bulk endpoints of the driver's function, as descriptors it can
    /// read and write itself, `[OUT, IN]`. Only one caller holds them at a time: a driver
    /// answers `BUSY` for another while one holds them, and `Unavailable` while the function
    /// is not enabled. The borrow is good for the caller to dup, and is taken on the driver's
    /// lane, so it must not block. Set [`TRACKS_CALLERS`](Self::TRACKS_CALLERS) for a driver
    /// that lends, so [`caller_gone`](Self::caller_gone) ends the hold. `NOT_SUPPORTED` by
    /// default.
    fn endpoints(&mut self, _caller: CallerId) -> Result<[BufferHandle<'_>; 2]> {
        Err(Status::NOT_SUPPORTED)
    }
    /// `caller` gives up the endpoints [`endpoints`](Self::endpoints) lent it. `NOT_FOUND`
    /// when it holds none.
    fn release_endpoints(&mut self, _caller: CallerId) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    /// Whether the driver has ended the endpoints' hold for good: the function was taken down
    /// by the driver itself and will not be back, so every copy of the pair a holder keeps is to
    /// be let go. The host asks after each tick and closes the holder's END descriptor when this
    /// is `true`. A host going away, a bus reset or a suspend is not this: those leave the hold as
    /// it is, and the same descriptors work again. `false` by default.
    fn endpoints_ended(&mut self) -> bool {
        false
    }
    /// Set this to `true` for a driver with long jobs: methods its contracts mark
    /// `#[method(N, job)]`, which start work and return a [`JobId`] at once. The work itself is
    /// done a step at a time from [`Lifecycle::tick`], so such a driver ticks too.
    const JOBS: bool = false;
    /// How job `job` stands. The host asks after every method, tick and cancel on this driver,
    /// for each job still open, and passes what changed to the job's starter. Answer
    /// [`JobState::Finished`] once, with the job's result written to `result`; the host forgets
    /// the job then. A number the driver doesn't know is finished with `NOT_FOUND`. Runs on the
    /// driver's lane and must not block.
    fn job(&mut self, _job: JobId, _result: &mut Reply<'_>) -> JobState {
        JobState::Finished(Status::NOT_FOUND)
    }
    /// Stop job `job` as soon as it can, undoing what a half-done job would leave, and answer
    /// [`JobState::Finished`] with [`Status::CANCELLED`] from then on. The host calls this when
    /// the starter cancels and when the starter goes. Must not block.
    fn job_cancel(&mut self, _job: JobId) {}
}

/// A long job, by the number its driver gave it when it started.
///
/// A contract method marked `#[method(N, job)]` returns one. The host keeps the job on the books
/// for whoever started it, asks the driver how it stands ([`Driver::job`]), and carries its
/// progress and its result to that caller. A driver's numbers are its own: it may reuse one once
/// it has answered [`JobState::Finished`] for it. A client sees the host's number for the job
/// instead, which is never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct JobId(pub u64);

impl Encode for JobId {
    const SHAPE: Shape = Shape::U64;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        self.0.encode(out)
    }
}

impl Decode<'_> for JobId {
    fn decode(from: &mut Reader<'_>) -> Result<Self> {
        u64::decode(from).map(Self)
    }
}

/// How a long job stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Still going: `done` of `total`, in whatever unit the contract says (bytes, steps).
    /// `total` is 0 when it isn't known yet.
    Running { done: u64, total: u64 },
    /// Over. On success the result the contract defines is in the reply. A job that was
    /// cancelled ends with [`Status::CANCELLED`]. The host never asks about it again.
    Finished(Status),
}

/// Time as the host sees it, supplied by the backend so a simulated board can
/// run on simulated time.
pub trait HostClock: Send + Sync {
    /// Nanoseconds on the platform's monotonic clock: `CLOCK_MONOTONIC` on Linux, which is
    /// the clock every stream frame, value, interrupt and edge time is on there. Never goes
    /// backwards.
    fn now_ns(&self) -> u64;
}
