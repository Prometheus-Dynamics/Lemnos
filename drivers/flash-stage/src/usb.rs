//! The USB device side, controller-independent: the control endpoint's state machine (the standard requests
//! a host uses to enumerate the device), the descriptors the device presents, the enumeration loop, and the
//! bulk pair as a [`Link`].
//!
//! A chip implements [`Controller`] over its own device controller (the DWC3 on the Rockchip stage; the dwc2
//! path of the K230 stage would implement it the same way). The controller is the only thing that touches
//! registers. Everything here is safe code, and the model in `sim::model` drives it in tests.
//!
//! Each control request is answered in full before the next is looked at: its data stage (chunked at
//! [`EP0_MAX_PACKET`]), then its status stage. A request this module does not answer is stalled and the
//! session goes on.

use crate::link::{IoError, Link};
use crate::wait::Clock;
use lemnos_flash_wire::timing::IDLE_LIMIT_US;
use lemnos_flash_wire::usb::{
    BULK_IN, BULK_MAX_PACKET, BULK_OUT, CONFIGURATION_VALUE, EP0_MAX_PACKET, INTERFACE_CLASS,
    INTERFACE_PROTOCOL, INTERFACE_SUBCLASS, LANGUAGE, PRODUCT, PRODUCT_ID, VENDOR_ID,
};

/// What the controller reports, one event at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Nothing new.
    None,
    /// The host reset the bus: the device's address and configuration are gone.
    BusReset,
    /// A control SETUP packet, eight bytes.
    Setup([u8; 8]),
    /// The cable was removed.
    Disconnect,
}

/// The speed the host negotiated with the device. The stage serves only at high speed: its bulk packets are 512 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speed {
    Low,
    Full,
    /// High speed: the only speed the stage serves at.
    #[default]
    High,
    Super,
    /// The controller reports a speed the stage does not know.
    Unknown,
}

/// Why a controller operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbError {
    /// A bounded wait ran out.
    Timeout,
    /// The controller reports a failure.
    Failed,
    /// The bus was reset while the operation was in flight.
    Reset,
}

/// The device controller, as the device side uses it. Every method that waits takes the deadline of the phase that
/// calls it, and returns [`UsbError::Timeout`] if it cannot finish by that deadline. None of them takes a deadline of
/// its own, and none waits on a fixed budget (ruling R4-B1: nothing a phase calls may wait past the phase's deadline).
pub trait Controller {
    /// The next event, without waiting for one. `deadline_us` bounds any command the controller issues to produce the
    /// event (the setup arm): a command that cannot finish by it is not issued to completion, and the event is `None`.
    fn poll(&mut self, deadline_us: u64) -> Event;
    /// Sets the device's address. Called before the status stage of SET_ADDRESS, as the host expects.
    fn set_address(&mut self, address: u8, deadline_us: u64) -> Result<(), UsbError>;
    /// Enables the bulk pair when `on` (the configuration is set), or disables it. Configuring an endpoint is a
    /// controller command: it finishes by `deadline_us` or it is a timeout.
    fn set_configured(&mut self, on: bool, deadline_us: u64) -> Result<(), UsbError>;
    /// One control IN data packet, at most [`EP0_MAX_PACKET`] bytes. An empty packet is a zero-length packet.
    fn ep0_in(&mut self, data: &[u8], deadline_us: u64) -> Result<(), UsbError>;
    /// The status stage of a request with no data stage: a zero-length IN packet.
    fn ep0_in_status(&mut self, deadline_us: u64) -> Result<(), UsbError>;
    /// The status stage of a control IN request: a zero-length OUT packet from the host.
    fn ep0_out_status(&mut self, deadline_us: u64) -> Result<(), UsbError>;
    /// Stalls the control endpoint for the request in flight.
    fn ep0_stall(&mut self, deadline_us: u64) -> Result<(), UsbError>;
    /// The speed negotiated on the bus, read after the host has enumerated the device. The stage requires high speed: a
    /// full-speed or slower link fails enumeration (the device does not configure). Bench check: the encoding is the
    /// controller's.
    fn speed(&mut self) -> Speed;
    /// Whether this controller takes the new address before the status stage of SET_ADDRESS (`true`), or only
    /// after it (`false`). The shared control code calls `set_address` on the side this says, so a controller
    /// that applies the address late is not assumed to apply it early. Each chip decides this from its own
    /// controller's behaviour, and records it as a bench check.
    fn address_before_status(&self) -> bool;
    /// Arms the bulk OUT transfer for exactly `len` bytes. The arm command is bounded by `deadline_us`: an arm that cannot
    /// finish by it is a timeout, so the phase that asked for it does not run past its deadline.
    fn bulk_arm(&mut self, len: usize, deadline_us: u64) -> Result<(), UsbError>;
    /// Takes the armed transfer if it has completed, without waiting: `Some(n)` with its length.
    fn bulk_poll(&mut self, out: &mut [u8]) -> Result<Option<usize>, UsbError>;
    /// Sends one bulk IN transfer, bounded by `budget_us`.
    fn bulk_send(&mut self, data: &[u8], budget_us: u64) -> Result<(), UsbError>;
}

/// The control endpoint's state: the address and whether a configuration is set.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ep0 {
    address: u8,
    configured: bool,
}

/// The descriptors. Built from the wire's USB identity, so the host and the device read the same numbers.
pub mod descriptors {
    use super::*;

    /// The device descriptor, 18 bytes. Its product string is string index 2.
    pub const DEVICE: [u8; 18] = [
        18,
        1,
        0x00,
        0x02, // bcdUSB 2.00
        0x00,
        0x00,
        0x00, // class, subclass, protocol: per interface
        EP0_MAX_PACKET,
        VENDOR_ID as u8,
        (VENDOR_ID >> 8) as u8,
        PRODUCT_ID as u8,
        (PRODUCT_ID >> 8) as u8,
        0x00,
        0x00, // bcdDevice 0.00
        0x00, // iManufacturer
        0x02, // iProduct
        0x00, // iSerialNumber
        1,    // one configuration
    ];

    /// The configuration descriptor with its one interface and its two bulk endpoints, 32 bytes.
    pub const CONFIGURATION: [u8; 32] = [
        9,
        2,
        32,
        0, // wTotalLength 32
        1, // one interface
        CONFIGURATION_VALUE,
        0,    // iConfiguration
        0x80, // bus powered
        50,   // 100 mA
        // interface 0
        9,
        4,
        0,
        0,
        2, // two endpoints
        INTERFACE_CLASS,
        INTERFACE_SUBCLASS,
        INTERFACE_PROTOCOL,
        3, // iInterface: the wire's name and short hash (string index 3)
        // endpoint OUT
        7,
        5,
        BULK_OUT,
        0x02, // bulk
        BULK_MAX_PACKET as u8,
        (BULK_MAX_PACKET >> 8) as u8,
        0,
        // endpoint IN
        7,
        5,
        BULK_IN,
        0x02,
        BULK_MAX_PACKET as u8,
        (BULK_MAX_PACKET >> 8) as u8,
        0,
    ];

    /// String descriptor 0: the language list.
    pub const LANGUAGES: [u8; 4] = [4, 3, LANGUAGE as u8, (LANGUAGE >> 8) as u8];

    /// The product string, as a string descriptor (UTF-16LE).
    pub const fn product() -> [u8; 2 + 2 * PRODUCT.len()] {
        let mut out = [0u8; 2 + 2 * PRODUCT.len()];
        out[0] = (2 + 2 * PRODUCT.len()) as u8;
        out[1] = 3;
        let bytes = PRODUCT.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            out[2 + 2 * i] = bytes[i];
            i += 1;
        }
        out
    }

    /// The product string descriptor: its bytes (see [`product`]).
    pub const PRODUCT_STRING: [u8; 2 + 2 * PRODUCT.len()] = product();

    /// The interface string descriptor (index 3): the wire's name and short hash, as UTF-16LE. Built from the wire
    /// crate's [`lemnos_flash_wire::usb::INTERFACE_NAME`], so a host reads the same text the wire names.
    pub const fn interface_string() -> [u8; 2 + 2 * INTERFACE_NAME_LEN] {
        let name = lemnos_flash_wire::usb::INTERFACE_NAME;
        let mut out = [0u8; 2 + 2 * INTERFACE_NAME_LEN];
        out[0] = (2 + 2 * INTERFACE_NAME_LEN) as u8;
        out[1] = 3;
        let mut i = 0;
        while i < INTERFACE_NAME_LEN {
            out[2 + 2 * i] = name[i];
            i += 1;
        }
        out
    }

    /// The length of the interface string in characters.
    pub const INTERFACE_NAME_LEN: usize = lemnos_flash_wire::usb::INTERFACE_NAME.len();

    /// The interface string descriptor, as bytes.
    pub const INTERFACE_STRING: [u8; 2 + 2 * INTERFACE_NAME_LEN] = interface_string();
}

/// The request's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    /// Parses the eight bytes of a SETUP packet.
    pub fn parse(bytes: [u8; 8]) -> Self {
        Self {
            request_type: bytes[0],
            request: bytes[1],
            value: u16::from_le_bytes([bytes[2], bytes[3]]),
            index: u16::from_le_bytes([bytes[4], bytes[5]]),
            length: u16::from_le_bytes([bytes[6], bytes[7]]),
        }
    }
}

impl Ep0 {
    /// A device after power-up or a bus reset: address 0, unconfigured.
    pub const fn new() -> Self {
        Self {
            address: 0,
            configured: false,
        }
    }

    /// The device's address.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Whether a non-zero configuration is set.
    pub fn configured(&self) -> bool {
        self.configured
    }

    /// A bus reset: the address and the configuration are gone.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Answers one control request, or stalls it. Returns the controller's failure if it has one.
    /// Answers one control request. Every controller command it issues takes `deadline_us`, the deadline of the phase
    /// that received the SETUP, so the request cannot run past that phase (R4-B1).
    pub fn handle<C: Controller>(
        &mut self,
        ctrl: &mut C,
        bytes: [u8; 8],
        deadline_us: u64,
    ) -> Result<(), UsbError> {
        let setup = Setup::parse(bytes);
        let standard = setup.request_type & 0x60 == 0;
        let to_host = setup.request_type & 0x80 != 0;
        let device = setup.request_type & 0x1F == 0;
        match (standard, device, to_host, setup.request) {
            // GET_STATUS (device): two zero bytes (self powered: no, remote wake: no).
            (true, true, true, 0) => self.data_in(ctrl, &[0, 0], setup.length, deadline_us),
            // GET_DESCRIPTOR (device).
            (true, true, true, 6) => match descriptor(setup.value, setup.index) {
                Some(data) => self.data_in(ctrl, data, setup.length, deadline_us),
                None => ctrl.ep0_stall(deadline_us),
            },
            // GET_CONFIGURATION (device).
            (true, true, true, 8) => {
                let value = [if self.configured {
                    CONFIGURATION_VALUE
                } else {
                    0
                }];
                self.data_in(ctrl, &value, setup.length, deadline_us)
            }
            // SET_ADDRESS (device): a seven-bit address. When the address takes effect is the controller's
            // (`address_before_status`); the status stage is the same either way.
            (true, true, false, 5) if setup.value < 128 => {
                self.address = setup.value as u8;
                if ctrl.address_before_status() {
                    ctrl.set_address(self.address, deadline_us)?;
                    ctrl.ep0_in_status(deadline_us)
                } else {
                    ctrl.ep0_in_status(deadline_us)?;
                    ctrl.set_address(self.address, deadline_us)
                }
            }
            // GET_STATUS to an interface (index 0) or an endpoint the device has (0x00, 0x01, 0x81): both zero.
            (true, _, true, 0)
                if (setup.request_type & 0x1F == 1 && setup.index == 0)
                    || (setup.request_type & 0x1F == 2
                        && matches!(setup.index as u8, 0x00 | 0x01 | 0x81)) =>
            {
                self.data_in(ctrl, &[0, 0], setup.length, deadline_us)
            }
            // SET_CONFIGURATION (device): 0 unconfigures, CONFIGURATION_VALUE configures; anything else stalls.
            (true, true, false, 9)
                if setup.value == 0 || setup.value == u16::from(CONFIGURATION_VALUE) =>
            {
                self.configured = setup.value != 0;
                ctrl.set_configured(self.configured, deadline_us)?;
                ctrl.ep0_in_status(deadline_us)
            }
            _ => ctrl.ep0_stall(deadline_us),
        }
    }

    /// A control IN request's data stage and status stage: the data truncated to the host's `wLength`, sent in
    /// packets of at most [`EP0_MAX_PACKET`]. A short total that is a whole number of packets ends with a
    /// zero-length packet, as the host expects.
    fn data_in<C: Controller>(
        &mut self,
        ctrl: &mut C,
        data: &[u8],
        wanted: u16,
        deadline_us: u64,
    ) -> Result<(), UsbError> {
        let data = &data[..data.len().min(usize::from(wanted))];
        let mps = usize::from(EP0_MAX_PACKET);
        for packet in data.chunks(mps) {
            ctrl.ep0_in(packet, deadline_us)?;
        }
        if data.len() < usize::from(wanted) && data.len().is_multiple_of(mps) {
            ctrl.ep0_in(&[], deadline_us)?;
        }
        ctrl.ep0_out_status(deadline_us)
    }
}

/// The descriptor a GET_DESCRIPTOR names, if the device has it.
pub fn descriptor(value: u16, index: u16) -> Option<&'static [u8]> {
    let kind = (value >> 8) as u8;
    let number = (value & 0xFF) as u8;
    match (kind, number, index) {
        (1, 0, _) => Some(&descriptors::DEVICE),
        (2, 0, _) => Some(&descriptors::CONFIGURATION),
        (3, 0, _) => Some(&descriptors::LANGUAGES),
        (3, 2, index) if index == LANGUAGE => Some(&descriptors::PRODUCT_STRING),
        (3, 3, index) if index == LANGUAGE => Some(&descriptors::INTERFACE_STRING),
        _ => None,
    }
}

/// Why enumeration ended without a configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumError {
    /// No configuration was set before the session's deadline. The host never finished; the stage ends.
    NotConfigured,
    /// The cable was removed.
    Disconnected,
    /// The host configured the device on a link slower than high speed. The configuration was undone; the device does not
    /// serve on that link.
    NotHighSpeed,
    /// The controller failed.
    Controller(UsbError),
}

/// Answers the host's control requests until a configuration is set, or the deadline passes. The deadline is an
/// absolute time on `clock`, and it is the session's idle deadline (counted from the last watchdog feed): a bus reset
/// does not move it, so re-enumeration does not grant a fresh limit beyond what the watchdog allows.
pub fn enumerate<C: Controller, K: Clock>(
    ctrl: &mut C,
    clock: &mut K,
    ep0: &mut Ep0,
    deadline_us: u64,
) -> Result<(), EnumError> {
    loop {
        if ep0.configured() {
            // The host configured the device. A link slower than high speed is a failed enumeration: the configuration is
            // undone, so the bulk pair is not left enabled on a link the stage cannot serve.
            if ctrl.speed() != Speed::High {
                ctrl.set_configured(false, deadline_us)
                    .map_err(EnumError::Controller)?;
                ep0.reset();
                return Err(EnumError::NotHighSpeed);
            }
            return Ok(());
        }
        if clock.now_us() >= deadline_us {
            return Err(EnumError::NotConfigured);
        }
        match ctrl.poll(deadline_us) {
            Event::None => {}
            Event::BusReset => ep0.reset(),
            // A control command that cannot finish by the deadline is the enumeration's deadline, not a controller fault.
            Event::Setup(bytes) => ep0.handle(ctrl, bytes, deadline_us).map_err(|e| match e {
                UsbError::Timeout => EnumError::NotConfigured,
                other => EnumError::Controller(other),
            })?,
            Event::Disconnect => return Err(EnumError::Disconnected),
        }
    }
}

/// Enumerates with the idle limit counted from now: the start of a session, where the watchdog is armed.
pub fn enumerate_from_now<C: Controller, K: Clock>(
    ctrl: &mut C,
    clock: &mut K,
    ep0: &mut Ep0,
) -> Result<(), EnumError> {
    let deadline = clock.now_us().saturating_add(IDLE_LIMIT_US);
    enumerate(ctrl, clock, ep0, deadline)
}

/// The bulk pair as a [`Link`]. While a frame waits for its bytes, control requests are answered, so a host
/// that reads its descriptors in the middle of a session is served; a bus reset or an unconfiguration ends the
/// wait with [`IoError::Reset`].
pub struct BulkLink<'a, C: Controller, K: Clock> {
    pub ctrl: &'a mut C,
    pub clock: &'a mut K,
    pub ep0: &'a mut Ep0,
}

impl<C: Controller, K: Clock> Link for BulkLink<'_, C, K> {
    fn recv(&mut self, buf: &mut [u8], budget_us: u64) -> Result<usize, IoError> {
        let deadline = self.clock.now_us().saturating_add(budget_us);
        // The arm is part of the phase: it is bounded by the same deadline as the wait.
        self.ctrl.bulk_arm(buf.len(), deadline).map_err(map_error)?;
        // Each pass, in this order: a completed frame is taken; then the deadline is checked; then one control event is
        // serviced, every command of which takes this phase's deadline (R4-B1).
        //
        // A frame is accepted only if it completed at or before the deadline. The completion is observed at the
        // `bulk_poll` that takes it, and that observation instant is what is compared. This is exact because every
        // controller operation returns by the deadline: a control request that runs on past it is a timeout before its
        // frame is taken, so no observation can come after the deadline for a frame that completed before it, and none
        // can come before it for one that completed after.
        loop {
            if let Some(n) = self.ctrl.bulk_poll(buf).map_err(map_error)? {
                let completed_at = self.clock.now_us();
                if completed_at > deadline {
                    return Err(IoError::Timeout);
                }
                return Ok(n);
            }
            if self.clock.now_us() >= deadline {
                return Err(IoError::Timeout);
            }
            match self.ctrl.poll(deadline) {
                Event::None => {}
                Event::BusReset => {
                    self.ep0.reset();
                    return Err(IoError::Reset);
                }
                Event::Disconnect => return Err(IoError::Reset),
                Event::Setup(bytes) => {
                    self.ep0
                        .handle(self.ctrl, bytes, deadline)
                        .map_err(map_error)?;
                    if !self.ep0.configured() {
                        return Err(IoError::Reset);
                    }
                }
            }
        }
    }

    fn send(&mut self, data: &[u8], budget_us: u64) -> Result<(), IoError> {
        self.ctrl.bulk_send(data, budget_us).map_err(map_error)
    }

    fn now_us(&mut self) -> u64 {
        self.clock.now_us()
    }
}

fn map_error(error: UsbError) -> IoError {
    match error {
        UsbError::Timeout => IoError::Timeout,
        UsbError::Failed => IoError::Failed,
        UsbError::Reset => IoError::Reset,
    }
}
