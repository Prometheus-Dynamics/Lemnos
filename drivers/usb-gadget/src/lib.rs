//! Brings this board up as a USB device and keeps it up.
//!
//! Untested on board: the checks are `testing/board-runs/E36.md`.
//!
//! The driver takes a [`UsbFunction`](lemnos_hal::UsbFunction) (the board file's `function`
//! need: on Linux a controller, `udc:NAME`) and a description of the gadget, builds the
//! descriptor set once at bind, enables the function with it, and enables it again whenever
//! that fails or the controller lets go.
//!
//! It owns bring-up and health, and it carries the device's traffic for whoever serves it,
//! through `usb.function/1` (`lemnos-contract-usb-function`): the bytes of every endpoint as
//! jobs on a data stream, control requests delivered as events and answered or stalled by
//! calls, endpoint halts, and the bus events. What the interfaces are for, and what any byte
//! means, is the caller's business, and the driver never looks.
//!
//! The data stream is the optional need `data` in the board file (`[devices.streams.data]`,
//! `job_bytes` at least the contract's `JOB_BYTES`, buffers of the contract's `DATA_OFFSET`
//! plus the longest transfer wanted). Name the controller under `uses`
//! (`uses = { function = "udc:NAME" }`), not as the device's `bus`: a `bus` stands in for
//! every need a board file doesn't name, the data stream included. Without the stream, and whenever no consumer has
//! the stream open, the device is on the bus with the identity and interfaces described,
//! every control request sent to the function is refused with a stall, and the endpoints
//! carry nothing: a device nobody serves refuses what it is asked. How jobs are checked and
//! buffers held is in [`serve`].
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `vendor`, `product` (integers, required): the USB vendor and product numbers, 0 to 0xffff.
//! - `interfaces` (string, required): the interfaces and their endpoints, as [`description`]
//!   sets out: the text itself, in every build.
//! - `interfaces_file` (string, optional, `std` builds only): a path to a file holding that text,
//!   read once at bind. A build without a filesystem refuses it by name ([`refusal`]). Giving both
//!   `interfaces` and `interfaces_file` is refused by name too.
//! - `release` (integer, default 0x0100): the device's release number (`bcdDevice`).
//! - `class`, `subclass`, `protocol` (integers, default 0): the device's class triple. 0 leaves
//!   the class to each interface.
//! - `manufacturer`, `product_name`, `serial` (strings, optional): what the host shows. `serial`
//!   defaults to the board's serial fact (`board_serial`, below); with neither, the device has no
//!   serial string and health says so.
//! - `board_serial` (string, optional, set by the daemon, never by the board file): the board's
//!   normalised serial, read from the identity device the gadget names as `identity` (see
//!   `docs/identity.md`). The daemon refuses a board file that sets it. With `function ncm derive`
//!   it is what the NCM addresses come from ([`derive`]); the family is the board's `[board] id`.
//! - `max_power_ma` (integer, default 100): the most the device draws from the bus, up to 500.
//! - `self_powered` (boolean, default false).
//! - `control_timeout_ms` (integer, default 1000, 1 to 60000): how long a control request
//!   waits for the caller's answer before it is stalled.
//!
//! A setting that is missing, out of range or malformed fails the bind with
//! `INVALID_ARGUMENT`. Binding does not touch the function.
//!
//! # Failure
//!
//! The work is done in [`Lifecycle::tick`], which never waits: endpoint transfers are asked
//! for with no timeout, so one that can't move yet is tried again by the next tick, and the
//! function loses nothing meanwhile. While the gadget is up the driver looks for events every
//! [`POLL_NS`], and every [`SERVE_NS`] while a consumer has the data stream open (at once
//! when bytes moved and more wait). When enabling fails, or the function reports
//! that its controller let go, the driver disables the function and enables it again after a
//! backoff that starts at 1 s, doubles, and stops at 8 s. The backoff clears once the gadget
//! has stayed up for 8 s. It never gives up and never switches itself off.
//!
//! A host that is not there is not a failure: the gadget is up, waiting for it, and health is
//! `Ok`. A reset or a pulled cable needs nothing from the driver: the function stays enabled
//! and the host finds it again. While the gadget is down, health is `Failed` with the reason.
//! Health is never `Gone`: a function handle is never dead for good (see
//! [`UsbFunction`](lemnos_hal::UsbFunction)), so there is nothing a new bind could mend that a
//! new enable can't.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod derive;
pub mod description;
pub mod serve;
#[cfg(test)]
mod serve_tests;
#[cfg(test)]
mod tests;

use core::time::Duration;

use description::{Fault, Identity, Set};
use lemnos_contract_usb_function::{
    Counters, EndpointInfo, Event, EventKind, FunctionInfo, JobStatus, MAX_IFNAME_TEXT, Request,
    State, UsbFunction,
};
use lemnos_core::{
    BufferHandle, CallerId, Config, Description, Health, Item, Lifecycle, MethodId, Result, Status,
    StreamHandle, UsbFunctionHandle, driver, status_of,
};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::UsbFunctionEvent;
use lemnos_hal::usb_descriptors::{self, UsbDescriptors};
use serve::Serve;

const NS_PER_MS: u64 = 1_000_000;
/// How often an enabled function is asked for events.
pub const POLL_NS: u64 = 20 * NS_PER_MS;
/// The first wait after a failure. Each further failure doubles it.
pub const BACKOFF_FIRST_NS: u64 = 1_000 * NS_PER_MS;
/// The longest wait after a failure, and how long the gadget must stay up to clear the
/// backoff.
pub const BACKOFF_CAP_NS: u64 = 8_000 * NS_PER_MS;
/// How often the data stream is served while a consumer has it open.
pub const SERVE_NS: u64 = NS_PER_MS;
/// The most events dealt with in one tick, so a host that floods requests can't hold the
/// lane.
const EVENTS_PER_TICK: usize = 16;
/// How long the host is given to take or send the data stage of a request the caller answers.
/// The host is in the middle of the request, so the stage moves at once or not at all.
const CONTROL_STAGE: Duration = Duration::from_millis(100);
/// The name the data stream is opened by: its need's.
const DATA_STREAM: &str = "data";

pub const STARTING: &str = "not enabled yet";
pub const NOT_THERE: &str = "the USB device controller, or what the board sets up for it, is \
                             not there: retrying";
pub const REFUSED: &str = "the controller can't carry this gadget's descriptors: retrying";
pub const DENIED: &str = "not allowed to set the gadget up: retrying";
pub const IN_USE: &str = "something else holds the controller: retrying";
pub const ENABLE_FAILED: &str = "the gadget could not be enabled: retrying";
pub const LET_GO: &str = "the controller let go of the gadget: enabling it again";
/// The description item for the NCM interface's name, read by the `ifname` method.
const IFNAME: MethodId = MethodId {
    contract: 0,
    method: 11,
};
/// Health while up, when the gadget has no serial string: the board names no serial source.
pub const NO_SERIAL: &str = "no USB serial string: the board names no readable serial";
/// Health while up, when the gadget has no serial string although the board names one that
/// could not be read.
pub const NO_SERIAL_READ: &str = "no USB serial string: the board serial could not be read";

/// The name the kernel gave the NCM interface, as the bound gadget reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InterfaceName {
    bytes: [u8; MAX_IFNAME_TEXT],
    len: usize,
}

fn enable_fault(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::NotFound | ErrorKind::Unavailable => NOT_THERE,
        ErrorKind::InvalidInput | ErrorKind::Unsupported => REFUSED,
        ErrorKind::PermissionDenied => DENIED,
        ErrorKind::Busy => IN_USE,
        _ => ENABLE_FAILED,
    }
}

#[driver("usb-gadget", provides = [UsbFunction])]
pub struct UsbGadget {
    #[needs]
    function: UsbFunctionHandle,
    /// `usb.function/1` jobs and their completions.
    #[needs(optional, job)]
    data: Option<StreamHandle>,
    set: Set,
    /// Whether the function is enabled.
    up: bool,
    /// The NCM interface's name, read back once the gadget is bound; `None` while it is down or
    /// the gadget has no NCM function.
    ifname: Option<InterfaceName>,
    /// Why the gadget is up but missing something: its health while up.
    note: Option<&'static str>,
    /// Whether the host has the device configured.
    configured: bool,
    /// Whether the bus is idle.
    suspended: bool,
    /// How long a control request waits for the caller's answer.
    control_timeout_ns: u64,
    /// How many consumers had gone when the request that waits was reported: one more since
    /// means whoever was told of it is no longer there.
    request_gone: u32,
    /// The length of the pending request when its data comes from the host, else `None`: the
    /// bytes `control_out` must be able to hold.
    request_out: Option<u16>,
    /// The caller holding the bulk endpoints, if one does: see [`Lifecycle::endpoints`].
    holder: Option<CallerId>,
    serve: Serve,
    /// When the next step is due. A tick can come early.
    next_ns: u64,
    /// When the gadget came up.
    up_since_ns: u64,
    /// Failures since the gadget last stayed up.
    attempts: u32,
    fault: &'static str,
}

impl UsbGadget {
    /// Whether the host has chosen the device's configuration.
    pub fn configured(&self) -> bool {
        self.configured
    }

    /// The wait after one more failure: 1, 2, 4, 8, 8... s.
    fn backoff(&mut self, fault: &'static str, now_ns: u64) -> u64 {
        self.fault = fault;
        let wait = BACKOFF_FIRST_NS
            .saturating_mul(1 << self.attempts.min(3))
            .min(BACKOFF_CAP_NS);
        self.attempts = self.attempts.saturating_add(1);
        self.next_ns = now_ns.saturating_add(wait);
        self.next_ns
    }

    /// The name the kernel gave the NCM interface, read after the bind. `None` when the function
    /// has no NCM function or the backend cannot say.
    fn read_interface_name(&mut self) -> Option<InterfaceName> {
        let mut bytes = [0u8; MAX_IFNAME_TEXT];
        match self.function.interface_name(&mut bytes) {
            Ok(Some(len)) if len <= MAX_IFNAME_TEXT => Some(InterfaceName { bytes, len }),
            _ => None,
        }
    }

    fn bring_up(&mut self, now_ns: u64) -> u64 {
        match self.function.enable(self.set.bytes()) {
            Ok(()) => {
                self.up = true;
                self.ifname = self.read_interface_name();
                self.up_since_ns = now_ns;
                self.next_ns = now_ns.saturating_add(POLL_NS);
                self.next_ns
            }
            Err(kind) => {
                // After a failed enable nothing is left, but a function that says it is
                // already enabled is: take it down so the next attempt starts clean.
                let _ = self.function.disable();
                self.backoff(enable_fault(kind), now_ns)
            }
        }
    }

    /// The controller let go, or the function failed: the gadget comes down, and what waited
    /// on it ends.
    fn let_go(&mut self, now_ns: u64) -> u64 {
        self.up = false;
        self.ifname = None;
        self.suspended = false;
        self.serve.request_deadline_ns = None;
        // The endpoints a holder has went with the session: its files are dead for good.
        self.holder = None;
        if core::mem::take(&mut self.configured) {
            self.serve.note(serve::plain(EventKind::Deconfigured));
        }
        if let Some(stream) = self.data.as_deref_mut() {
            // A stream that fails here leaves the jobs for the function to fail one by one.
            let _ = self
                .serve
                .end(stream, JobStatus::NotConfigured, now_ns, |job| {
                    job.kind != lemnos_contract_usb_function::JobKind::Event
                });
        }
        let _ = self.function.disable();
        self.backoff(LET_GO, now_ns)
    }

    /// Refuses the control request that waits, if one does, and counts it.
    fn refuse_request(&mut self) {
        if self.serve.request_deadline_ns.take().is_some() {
            // A host that already let it go is no fault.
            let _ = self.function.control_stall();
            self.serve.counters.requests_unanswered += 1;
        }
    }

    fn keep_up(&mut self, now_ns: u64) -> u64 {
        if now_ns.saturating_sub(self.up_since_ns) >= BACKOFF_CAP_NS {
            self.attempts = 0;
        }
        // Whether a consumer has the data stream open. A stream that fails counts as nobody
        // there for this tick. Nobody is served while a caller holds the bulk endpoints: the
        // bytes on them are that caller's, and this driver's reads would race it.
        let serving = match self.data.as_deref_mut() {
            Some(stream) => self.serve.refresh(stream).unwrap_or(false),
            None => false,
        } && self.holder.is_none();
        // A request waits for its answer only while the consumer that was told of it is there
        // to give it, and only so long.
        if !serving
            || self.serve.gone != self.request_gone
            || self
                .serve
                .request_deadline_ns
                .is_some_and(|deadline| now_ns >= deadline)
        {
            self.refuse_request();
        }
        // Reading on would stall a request that waits, so nothing is read until it is answered.
        let mut events = EVENTS_PER_TICK;
        while events > 0 && self.serve.request_deadline_ns.is_none() {
            events -= 1;
            match self.function.next_event() {
                Ok(None) => break,
                Ok(Some(UsbFunctionEvent::Configured)) => {
                    self.configured = true;
                    self.suspended = false;
                    self.serve.note(serve::plain(EventKind::Configured));
                }
                Ok(Some(UsbFunctionEvent::Deconfigured)) => {
                    self.configured = false;
                    self.suspended = false;
                    self.serve.note(serve::plain(EventKind::Deconfigured));
                    if let Some(stream) = self.data.as_deref_mut() {
                        let _ = self
                            .serve
                            .end(stream, JobStatus::NotConfigured, now_ns, |job| {
                                job.kind != lemnos_contract_usb_function::JobKind::Event
                            });
                    }
                }
                Ok(Some(UsbFunctionEvent::Suspended)) => {
                    self.suspended = true;
                    self.serve.note(serve::plain(EventKind::Suspended));
                }
                Ok(Some(UsbFunctionEvent::Resumed)) => {
                    self.suspended = false;
                    self.serve.note(serve::plain(EventKind::Resumed));
                }
                Ok(Some(UsbFunctionEvent::Setup { setup, length })) => {
                    if serving {
                        self.serve.note(Event {
                            kind: EventKind::Setup,
                            request: Request {
                                request_type: setup.request_type,
                                request: setup.request,
                                value: setup.value,
                                index: setup.index,
                                length,
                            },
                        });
                        self.serve.request_deadline_ns =
                            Some(now_ns.saturating_add(self.control_timeout_ns));
                        self.request_gone = self.serve.gone;
                        self.request_out = (setup.request_type & 0x80 == 0).then_some(length);
                    } else {
                        // Nobody here answers requests. A host that already let it go is no
                        // fault.
                        let _ = self.function.control_stall();
                        self.serve.counters.requests_unanswered += 1;
                    }
                }
                Err(_) => return self.let_go(now_ns),
            }
        }
        let mut again = false;
        if serving && let Some(stream) = self.data.as_deref_mut() {
            // A stream that fails leaves its jobs where they are, for the next tick.
            again = self
                .serve
                .run(stream, &mut *self.function, now_ns)
                .unwrap_or(false);
        }
        self.next_ns = match (serving, again) {
            (true, true) => now_ns,
            (true, false) => now_ns.saturating_add(SERVE_NS),
            (false, _) => now_ns.saturating_add(POLL_NS),
        };
        self.next_ns
    }

    /// What an answer to the pending request came to. Nothing is pending afterwards, unless
    /// the function said the answer did not fit the request.
    fn answered<T>(&mut self, answer: core::result::Result<T, ErrorKind>) -> Result<T> {
        if !matches!(answer, Err(ErrorKind::InvalidInput)) {
            self.serve.request_deadline_ns = None;
        }
        answer.map_err(status_of)
    }
}

impl UsbFunction for UsbGadget {
    /// The NCM interface's name, as the kernel gave it. `NOT_FOUND` while there is none.
    fn ifname(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = self.ifname.ok_or(Status::NOT_FOUND)?;
        into.get_mut(..name.len)
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(&name.bytes[..name.len]);
        Ok(name.len)
    }

    fn info(&mut self) -> Result<FunctionInfo> {
        Ok(FunctionInfo {
            endpoints: self.serve.endpoints().len() as u8,
            max_transfer: self.serve.max_transfer() as u32,
            max_pending: serve::MAX_PENDING as u16,
            max_events: serve::MAX_EVENTS as u16,
            control_timeout_ms: (self.control_timeout_ns / NS_PER_MS) as u32,
        })
    }

    fn endpoint(&mut self, index: u8) -> Result<EndpointInfo> {
        self.serve
            .endpoints()
            .get(usize::from(index))
            .copied()
            .ok_or(Status::NOT_FOUND)
    }

    fn data_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        if self.data.is_none() {
            return Err(Status::NOT_SUPPORTED);
        }
        into.get_mut(..DATA_STREAM.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(DATA_STREAM.as_bytes());
        Ok(DATA_STREAM.len())
    }

    fn state(&mut self) -> Result<State> {
        Ok(State {
            up: self.up,
            configured: self.configured,
            suspended: self.suspended,
            request_pending: self.serve.request_deadline_ns.is_some(),
        })
    }

    fn control_in(&mut self, data: &[u8]) -> Result<u32> {
        let sent = self.function.control_in(data, CONTROL_STAGE);
        self.answered(sent).map(|sent| sent as u32)
    }

    fn control_out(&mut self, into: &mut [u8]) -> Result<usize> {
        // The data stage is taken whole or not at all: a buffer that cannot hold the pending
        // request's data is refused before the function is asked, so nothing is transferred
        // and the request stays pending.
        if self.serve.request_deadline_ns.is_some()
            && self
                .request_out
                .is_some_and(|length| into.len() < usize::from(length))
        {
            return Err(Status::BUFFER_TOO_SMALL);
        }
        let came = self.function.control_out(into, CONTROL_STAGE);
        self.answered(came)
    }

    fn control_stall(&mut self) -> Result<()> {
        let stalled = self.function.control_stall();
        self.answered(stalled)
    }

    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<()> {
        self.function.set_halt(endpoint, halted).map_err(status_of)
    }

    fn cancel(&mut self, endpoint: u8) -> Result<u32> {
        let Some(stream) = self.data.as_deref_mut() else {
            return Ok(0);
        };
        // The stream's clock is the tick's: a cancelled job is stamped with the last one.
        let now_ns = self.next_ns;
        self.serve
            .end(stream, JobStatus::Cancelled, now_ns, |job| {
                use lemnos_contract_usb_function::JobKind;
                match job.kind {
                    JobKind::Event => endpoint == 0,
                    JobKind::Send | JobKind::Receive => endpoint != 0 && job.endpoint == endpoint,
                }
            })
            .map_err(status_of)
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.serve.counters)
    }
}

impl Lifecycle for UsbGadget {
    type Needs = UsbGadgetNeeds;

    const TICKS: bool = true;
    /// The holder of the bulk endpoints is told when it goes, so its hold ends with it.
    const TRACKS_CALLERS: bool = true;

    /// The NCM interface's name, once the gadget is bound and the kernel has named it. Absent
    /// otherwise, and while the gadget is down.
    fn describe(&self, out: &mut Description) {
        if self.ifname.is_some() {
            out.add(Item::value("ifname", IFNAME));
        }
    }

    /// Lends the bulk pair to `caller`, which holds it until it releases it or goes. One holder
    /// at a time: another caller gets `BUSY`, and the same caller asking again gets fresh
    /// descriptors for the same hold. Everything comes from the function's endpoint files, which
    /// the function keeps open while enabled; nothing is opened here.
    fn endpoints(&mut self, caller: CallerId) -> Result<[BufferHandle<'_>; 2]> {
        if self.holder.is_some_and(|holder| holder != caller) {
            return Err(Status::BUSY);
        }
        let handles = self.function.endpoint_handles().map_err(status_of)?;
        self.holder = Some(caller);
        Ok(handles)
    }

    /// Gives up the hold `caller` has. `NOT_FOUND` when it holds none.
    fn release_endpoints(&mut self, caller: CallerId) -> Result<()> {
        if self.holder != Some(caller) {
            return Err(Status::NOT_FOUND);
        }
        self.holder = None;
        Ok(())
    }

    fn caller_gone(&mut self, caller: CallerId) {
        if self.holder == Some(caller) {
            self.holder = None;
        }
    }

    fn start(needs: UsbGadgetNeeds, cfg: &Config<'_>) -> Result<Self> {
        let (set, _) = settings(cfg)?;
        let note = if cfg.str("serial").is_some() || cfg.str("board_serial").is_some() {
            None
        } else if cfg.str("board_serial_error").is_some() {
            Some(NO_SERIAL_READ)
        } else {
            Some(NO_SERIAL)
        };
        let control_timeout_ms = integer(cfg, "control_timeout_ms", 60_000, Some(1_000))?;
        if control_timeout_ms == 0 {
            return Err(Status::INVALID_ARGUMENT);
        }
        // The set was built here, so it parses; one that didn't would name no endpoints.
        let serve = {
            let parsed = UsbDescriptors::parse(set.bytes()).ok();
            let named = parsed
                .iter()
                .flat_map(|set| usb_descriptors::endpoints(set.full_speed()))
                .map(|e| EndpointInfo {
                    address: e.address,
                    attributes: e.attributes,
                    max_packet: e.max_packet,
                });
            Serve::new(named, needs.data.is_some())
        };
        Ok(Self {
            function: needs.function,
            data: needs.data,
            set,
            up: false,
            ifname: None,
            note,
            configured: false,
            suspended: false,
            control_timeout_ns: control_timeout_ms as u64 * NS_PER_MS,
            request_gone: 0,
            request_out: None,
            serve,
            holder: None,
            next_ns: 0,
            up_since_ns: 0,
            attempts: 0,
            fault: STARTING,
        })
    }

    fn health(&self) -> Health {
        match (self.up, self.fault) {
            (true, _) => self.note.map_or(Health::Ok, Health::Degraded),
            (false, STARTING) => Health::Degraded(STARTING),
            (false, fault) => Health::Failed(fault),
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if now_ns < self.next_ns {
            return Some(self.next_ns);
        }
        Some(if self.up {
            self.keep_up(now_ns)
        } else {
            self.bring_up(now_ns)
        })
    }
}

impl Drop for UsbGadget {
    /// A driver that goes takes its gadget off the bus.
    fn drop(&mut self) {
        let _ = self.function.disable();
    }
}

/// An integer setting within `0..=max`, or `default` when it is not given.
fn integer(cfg: &Config<'_>, key: &str, max: i64, default: Option<i64>) -> Result<i64> {
    match (cfg.get(key), default) {
        (None, Some(default)) => Ok(default),
        (None, None) => Err(Status::INVALID_ARGUMENT),
        (Some(_), _) => cfg
            .int(key)
            .filter(|value| (0..=max).contains(value))
            .ok_or(Status::INVALID_ARGUMENT),
    }
}

/// A string setting, if given. A setting of another type is refused.
fn string<'a>(cfg: &Config<'a>, key: &str) -> Result<Option<&'a str>> {
    match (cfg.get(key), cfg.str(key)) {
        (None, _) => Ok(None),
        (Some(_), Some(text)) => Ok(Some(text)),
        (Some(_), None) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The reason the interface settings are refused by name, in this build: both `interfaces` and
/// `interfaces_file` given, or `interfaces_file` in a build without a filesystem. `None` when they
/// are not refused. The daemon checks a board file's settings with this before it binds, so the
/// reason is named there; the driver's own bind refuses with `INVALID_ARGUMENT`.
pub fn refusal(cfg: &Config<'_>) -> Option<&'static str> {
    match (cfg.get("interfaces"), cfg.get("interfaces_file")) {
        (Some(_), Some(_)) => Some("give interfaces or interfaces_file, not both"),
        (None, Some(_)) if !cfg!(feature = "std") => {
            Some("interfaces_file needs a build with a filesystem")
        }
        _ => None,
    }
}

/// The descriptor set and the functions the settings describe. The setup that makes the
/// gadget's configfs tree reads the same settings through this, so the two agree.
pub fn settings(cfg: &Config<'_>) -> Result<(Set, description::Functions)> {
    let self_powered = match (cfg.get("self_powered"), cfg.bool("self_powered")) {
        (None, _) => false,
        (Some(_), Some(value)) => value,
        (Some(_), None) => return Err(Status::INVALID_ARGUMENT),
    };
    let identity = Identity {
        vendor: integer(cfg, "vendor", 0xffff, None)? as u16,
        product: integer(cfg, "product", 0xffff, None)? as u16,
        release: integer(cfg, "release", 0xffff, Some(0x0100))? as u16,
        class: integer(cfg, "class", 0xff, Some(0))? as u8,
        subclass: integer(cfg, "subclass", 0xff, Some(0))? as u8,
        protocol: integer(cfg, "protocol", 0xff, Some(0))? as u8,
        manufacturer: string(cfg, "manufacturer")?,
        product_name: string(cfg, "product_name")?,
        serial: string(cfg, "serial")?.or(cfg.str("board_serial")),
        max_power_ma: integer(cfg, "max_power_ma", 500, Some(100))? as u16,
        self_powered,
    };
    if refusal(cfg).is_some() {
        return Err(Status::INVALID_ARGUMENT);
    }
    #[cfg(feature = "std")]
    let file = match cfg.str("interfaces_file") {
        Some(path) => Some(std::fs::read_to_string(path).map_err(|_| Status::INVALID_ARGUMENT)?),
        None => None,
    };
    #[cfg(feature = "std")]
    let interfaces = match &file {
        Some(text) => text.as_str(),
        None => string(cfg, "interfaces")?.ok_or(Status::INVALID_ARGUMENT)?,
    };
    #[cfg(not(feature = "std"))]
    let interfaces = string(cfg, "interfaces")?.ok_or(Status::INVALID_ARGUMENT)?;
    let set =
        description::build(&identity, interfaces).map_err(|_: Fault| Status::INVALID_ARGUMENT)?;
    // The addresses come from the board's serial fact, and only when there is one in the format.
    let macs = cfg
        .str("board_serial")
        .and_then(|serial| derive::macs(cfg.board(), serial));
    let functions =
        description::functions(interfaces, macs).map_err(|_: Fault| Status::INVALID_ARGUMENT)?;
    Ok((set, functions))
}
