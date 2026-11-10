//! Brings this board up as a USB device and keeps it up.
//!
//! Untested on board.
//!
//! The driver takes a [`UsbFunction`](lemnos_hal::UsbFunction) (the board file's `function`
//! need: on Linux a controller, `udc:NAME`) and a description of the gadget, builds the
//! descriptor set once at bind, enables the function with it, and enables it again whenever
//! that fails or the controller lets go.
//!
//! It owns bring-up and health and nothing else. No merged contract carries endpoint bytes or
//! control requests for a caller (`raw-bus` is I2C, SPI, a line and a PWM channel), and this
//! driver invents none: until such a contract exists, a host sees the device with the identity
//! and interfaces described, every control request sent to the function is refused with a
//! stall, and the endpoints carry nothing. What the interfaces are for is not this driver's
//! business, and it never looks.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `vendor`, `product` (integers, required): the USB vendor and product numbers, 0 to 0xffff.
//! - `interfaces` (string, required): the interfaces and their endpoints, as [`description`]
//!   sets out. With the `std` feature, a path to a file holding that text, read once at bind.
//! - `release` (integer, default 0x0100): the device's release number (`bcdDevice`).
//! - `class`, `subclass`, `protocol` (integers, default 0): the device's class triple. 0 leaves
//!   the class to each interface.
//! - `manufacturer`, `product_name`, `serial` (strings, optional): what the host shows.
//! - `max_power_ma` (integer, default 100): the most the device draws from the bus, up to 500.
//! - `self_powered` (boolean, default false).
//!
//! A setting that is missing, out of range or malformed fails the bind with
//! `INVALID_ARGUMENT`. Binding does not touch the function.
//!
//! # Failure
//!
//! The work is done in [`Lifecycle::tick`], which never waits. While the gadget is up the
//! driver looks for events every [`POLL_NS`]. When enabling fails, or the function reports
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

#[cfg(feature = "std")]
extern crate std;

pub mod description;
#[cfg(test)]
mod tests;

use description::{Fault, Identity, Set};
use lemnos_core::{Config, Health, Lifecycle, Result, Status, UsbFunctionHandle, driver};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::UsbFunctionEvent;

const NS_PER_MS: u64 = 1_000_000;
/// How often an enabled function is asked for events.
pub const POLL_NS: u64 = 20 * NS_PER_MS;
/// The first wait after a failure. Each further failure doubles it.
pub const BACKOFF_FIRST_NS: u64 = 1_000 * NS_PER_MS;
/// The longest wait after a failure, and how long the gadget must stay up to clear the
/// backoff.
pub const BACKOFF_CAP_NS: u64 = 8_000 * NS_PER_MS;
/// The most events dealt with in one tick, so a host that floods requests can't hold the
/// lane.
const EVENTS_PER_TICK: usize = 16;

pub const STARTING: &str = "not enabled yet";
pub const NOT_THERE: &str = "the USB device controller, or what the board sets up for it, is \
                             not there: retrying";
pub const REFUSED: &str = "the controller can't carry this gadget's descriptors: retrying";
pub const DENIED: &str = "not allowed to set the gadget up: retrying";
pub const IN_USE: &str = "something else holds the controller: retrying";
pub const ENABLE_FAILED: &str = "the gadget could not be enabled: retrying";
pub const LET_GO: &str = "the controller let go of the gadget: enabling it again";

fn enable_fault(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::NotFound | ErrorKind::Unavailable => NOT_THERE,
        ErrorKind::InvalidInput | ErrorKind::Unsupported => REFUSED,
        ErrorKind::PermissionDenied => DENIED,
        ErrorKind::Busy => IN_USE,
        _ => ENABLE_FAILED,
    }
}

#[driver("usb-gadget", provides = [])]
pub struct UsbGadget {
    #[needs]
    function: UsbFunctionHandle,
    set: Set,
    /// Whether the function is enabled.
    up: bool,
    /// Whether the host has the device configured. Only for a reader of the state.
    configured: bool,
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

    fn bring_up(&mut self, now_ns: u64) -> u64 {
        match self.function.enable(self.set.bytes()) {
            Ok(()) => {
                self.up = true;
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

    fn keep_up(&mut self, now_ns: u64) -> u64 {
        if now_ns.saturating_sub(self.up_since_ns) >= BACKOFF_CAP_NS {
            self.attempts = 0;
        }
        for _ in 0..EVENTS_PER_TICK {
            match self.function.next_event() {
                Ok(None) => break,
                Ok(Some(UsbFunctionEvent::Configured)) => self.configured = true,
                Ok(Some(UsbFunctionEvent::Deconfigured)) => self.configured = false,
                Ok(Some(UsbFunctionEvent::Suspended | UsbFunctionEvent::Resumed)) => {}
                Ok(Some(UsbFunctionEvent::Setup { .. })) => {
                    // Nobody here answers requests. A host that already let it go is no fault.
                    let _ = self.function.control_stall();
                }
                Err(_) => {
                    self.up = false;
                    self.configured = false;
                    let _ = self.function.disable();
                    return self.backoff(LET_GO, now_ns);
                }
            }
        }
        self.next_ns = now_ns.saturating_add(POLL_NS);
        self.next_ns
    }
}

impl Lifecycle for UsbGadget {
    type Needs = UsbGadgetNeeds;

    const TICKS: bool = true;

    fn start(needs: UsbGadgetNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            function: needs.function,
            set: set(cfg)?,
            up: false,
            configured: false,
            next_ns: 0,
            up_since_ns: 0,
            attempts: 0,
            fault: STARTING,
        })
    }

    fn health(&self) -> Health {
        match (self.up, self.fault) {
            (true, _) => Health::Ok,
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

/// The descriptor set the settings describe.
fn set(cfg: &Config<'_>) -> Result<Set> {
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
        serial: string(cfg, "serial")?,
        max_power_ma: integer(cfg, "max_power_ma", 500, Some(100))? as u16,
        self_powered,
    };
    let interfaces = string(cfg, "interfaces")?.ok_or(Status::INVALID_ARGUMENT)?;
    #[cfg(feature = "std")]
    let text = std::fs::read_to_string(interfaces).map_err(|_| Status::INVALID_ARGUMENT)?;
    #[cfg(feature = "std")]
    let interfaces = text.as_str();
    description::build(&identity, interfaces).map_err(|_: Fault| Status::INVALID_ARGUMENT)
}
