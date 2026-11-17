//! The stage's USB device path: the device logic for the boot ROM's download IDs and the stage's
//! bulk pair. It runs over a [`UsbHw`] trait, which the `-sys` crate implements over the DesignWare
//! OTG controller (`dwc2`) and the simulator/tests implement with a scripted host. The logic here is
//! safe code and has no register knowledge.
//!
//! The device answers as the ROM's download mode does, so the host's observation rule finds the same
//! IDs (`29f1:0230`): the device descriptor carries them. The stage's identity is the vendor request
//! GET_CPU_INFO (`bmRequestType 0xc0`, request 0, value 0), and the loader version is request 0 value
//! 1, as the host reads them (`lemnos-k230-usbboot::bootrom::stage`).
//!
//! Watchdog rule: this layer never feeds the watchdogs. Bytes move between the hardware and the
//! [`BulkEndpoints`] (the stage), and the stage feeds only after a verified chunk.
//!
//! Register and protocol facts, with their sources, are in `docs/flashing.md` and at the constants below.

use crate::format::{LOADER_VERSION, PACKET, STAGE_IDENT};
use crate::stage::NoReply;

/// The download IDs the device presents (`kburn.h:205`, `kburn.cpp:403-405`).
pub const VID: u16 = 0x29f1;
pub const PID: u16 = 0x0230;
/// The bulk OUT endpoint's address (`kburn_k230.h:13`) and the bulk IN endpoint's (`kburn_k230.h:14`).
pub const BULK_OUT: u8 = 0x01;
pub const BULK_IN: u8 = 0x81;
/// Endpoint 0, the control endpoint (IN and OUT share the number).
pub const EP0: u8 = 0x00;
/// The high-speed bulk packet size and the endpoint 0 packet size.
pub const BULK_MPS: u16 = 512;
pub const EP0_MPS: u8 = 64;
/// The largest bulk message the device accepts: a command, or one page of a write, is far smaller;
/// the buffer holds one stage chunk and its packet.
pub const RX_MAX: usize = 4096 + PACKET;

/// A USB device-side failure of the hardware (the device's own errors, not the protocol's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwError {
    /// The endpoint is stalled and the transfer was refused.
    Stalled,
    /// The device left the bus (a reset mid-transfer is reported as this, then as a [`Event::BusReset`]).
    Disconnected,
    /// The hardware did not finish in its bounded wait.
    Timeout,
    /// The transfer was bigger than the buffer.
    Overrun,
}

/// What the hardware reports next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Nothing to do.
    Idle,
    /// The host reset the bus: the device goes back to address 0, unconfigured.
    BusReset,
    /// A SETUP packet on endpoint 0.
    Setup([u8; 8]),
    /// A complete OUT transfer of `len` bytes on endpoint `ep` (its data is taken with
    /// [`UsbHw::read_out`]). A transfer ends at a short packet, as the protocol says.
    Out { ep: u8, len: usize },
}

/// The device-side hardware. Each method is one operation; a bounded wait is the implementation's.
pub trait UsbHw {
    /// Resets the core and brings it up in device mode, ready for a host's enumeration.
    fn core_reset(&mut self) -> Result<(), HwError>;
    /// The next event, or [`Event::Idle`].
    fn poll(&mut self) -> Event;
    /// Copies the data of the last [`Event::Out`] on `ep` into `buf`, returning its length.
    fn read_out(&mut self, ep: u8, buf: &mut [u8]) -> Result<usize, HwError>;
    /// Sends `data` on IN endpoint `ep` (an empty `data` is the zero-length status packet).
    fn write_in(&mut self, ep: u8, data: &[u8]) -> Result<(), HwError>;
    /// True when the IN endpoint can take a new transfer (the last one has gone to the host).
    fn in_ready(&self, ep: u8) -> bool;
    /// Stalls endpoint `ep` (its next transfer is refused).
    fn stall(&mut self, ep: u8);
    /// Sets the device address after the SET_ADDRESS status stage.
    fn set_address(&mut self, address: u8);
}

/// The stage's bulk side, as the device sees it: commands and data in, replies out, and a reset.
pub trait BulkEndpoints {
    /// One bulk OUT message from the host.
    fn on_out(&mut self, message: &[u8]) -> Result<(), crate::stage::Reboot>;
    /// The next reply for the host, if one is waiting (`out` takes it).
    fn on_in(&mut self, out: &mut [u8]) -> Result<usize, NoReply>;
    /// The bus was reset: whatever the stage was in the middle of is abandoned.
    fn on_reset(&mut self);
}

/// What one [`Device::step`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Idle,
    Reset,
    /// A control request was answered (or stalled, see [`Step::Stalled`]).
    Control,
    /// A bulk message went to the stage; a reply may have gone back with it (see [`Step::Replied`]).
    Bulk,
    /// A reply went to the host.
    Replied,
    /// Nothing was waiting for the host yet.
    Quiet,
    /// An endpoint was stalled: an unknown request, or a bulk endpoint the stage does not have.
    Stalled,
    /// The host asked for a reboot: the caller makes the fast reset.
    Reboot,
}

/// The device: the enumeration state and the endpoints.
pub struct Device<H: UsbHw> {
    hw: H,
    address: u8,
    configured: bool,
    rx: [u8; RX_MAX],
    reply: [u8; PACKET + 4096],
}

impl<H: UsbHw> Device<H> {
    /// Brings the core up in device mode. Fails when the hardware does.
    pub fn start(mut hw: H) -> Result<Self, HwError> {
        hw.core_reset()?;
        Ok(Self {
            hw,
            address: 0,
            configured: false,
            rx: [0; RX_MAX],
            reply: [0; PACKET + 4096],
        })
    }

    /// The device's address (0 until SET_ADDRESS).
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Whether the host has configured the device.
    pub fn configured(&self) -> bool {
        self.configured
    }

    /// The hardware, for a test or the simulator to look at.
    pub fn hw(&self) -> &H {
        &self.hw
    }

    /// The hardware, mutably.
    pub fn hw_mut(&mut self) -> &mut H {
        &mut self.hw
    }

    /// Handles one hardware event, then offers the stage's next reply if the IN endpoint is free.
    pub fn step<B: BulkEndpoints>(&mut self, bulk: &mut B) -> Result<Step, HwError> {
        match self.hw.poll() {
            Event::Idle => Ok(self.pump(bulk)?.unwrap_or(Step::Idle)),
            Event::BusReset => {
                self.address = 0;
                self.configured = false;
                self.hw.set_address(0);
                bulk.on_reset();
                Ok(Step::Reset)
            }
            Event::Setup(setup) => match self.control(setup) {
                Ok(()) => Ok(Step::Control),
                Err(HwError::Stalled) => {
                    self.hw.stall(EP0);
                    Ok(Step::Stalled)
                }
                Err(error) => Err(error),
            },
            Event::Out { ep: EP0, .. } => Ok(Step::Control),
            Event::Out { ep: BULK_OUT, len } => {
                if len > self.rx.len() {
                    return Err(HwError::Overrun);
                }
                let got = self.hw.read_out(BULK_OUT, &mut self.rx[..len])?;
                match bulk.on_out(&self.rx[..got]) {
                    Err(crate::stage::Reboot) => Ok(Step::Reboot),
                    Ok(()) => {
                        self.pump(bulk)?;
                        Ok(Step::Bulk)
                    }
                }
            }
            Event::Out { ep, .. } => {
                self.hw.stall(ep);
                Ok(Step::Stalled)
            }
        }
    }

    /// Sends the stage's next reply on the bulk IN endpoint, if the endpoint is free and one waits.
    fn pump<B: BulkEndpoints>(&mut self, bulk: &mut B) -> Result<Option<Step>, HwError> {
        if !self.hw.in_ready(BULK_IN) {
            return Ok(None);
        }
        match bulk.on_in(&mut self.reply) {
            Ok(n) => {
                let reply = &self.reply[..n];
                self.hw.write_in(BULK_IN, reply)?;
                Ok(Some(Step::Replied))
            }
            Err(NoReply) => Ok(Some(Step::Quiet)),
        }
    }

    /// One control request on endpoint 0. Standard requests the host needs to enumerate, and the
    /// stage's two vendor requests. Anything else is [`HwError::Stalled`].
    fn control(&mut self, setup: [u8; 8]) -> Result<(), HwError> {
        let request_type = setup[0];
        let request = setup[1];
        let value = u16::from_le_bytes([setup[2], setup[3]]);
        let length = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
        match (request_type, request) {
            // GET_DESCRIPTOR (standard, device to host): device, configuration, strings.
            (0x80, 6) => {
                let kind = (value >> 8) as u8;
                let index = (value & 0xff) as u8;
                let data = descriptor(kind, index).ok_or(HwError::Stalled)?;
                self.hw.write_in(EP0, &data[..data.len().min(length)])
            }
            // SET_ADDRESS: the address takes effect after the status stage.
            (0x00, 5) => {
                self.hw.write_in(EP0, &[])?;
                self.address = (value & 0x7f) as u8;
                self.hw.set_address(self.address);
                Ok(())
            }
            // SET_CONFIGURATION.
            (0x00, 9) => {
                self.configured = value != 0;
                self.hw.write_in(EP0, &[])
            }
            // The stage's identity: GET_CPU_INFO, 32 bytes, NUL padded (`bootrom::stage` reads it).
            (0xc0, 0) if value == 0 => {
                let mut info = [0u8; 32];
                info[..STAGE_IDENT.len()].copy_from_slice(STAGE_IDENT.as_bytes());
                self.hw.write_in(EP0, &info[..info.len().min(length)])
            }
            // The loader version (request 0, value 1), as four bytes.
            (0xc0, 0) if value == 1 => {
                let version = LOADER_VERSION.to_le_bytes();
                self.hw.write_in(EP0, &version[..version.len().min(length)])
            }
            _ => Err(HwError::Stalled),
        }
    }
}

/// The device descriptor: the download IDs, one configuration, endpoint 0 at 64 bytes.
pub const DEVICE: [u8; 18] = [
    18,
    1, // bLength, bDescriptorType (device)
    0x00,
    0x02, // bcdUSB 2.00
    0,
    0,
    0,       // class, subclass, protocol (per interface)
    EP0_MPS, // bMaxPacketSize0
    VID as u8,
    (VID >> 8) as u8, // idVendor 29f1
    PID as u8,
    (PID >> 8) as u8, // idProduct 0230
    0x00,
    0x01, // bcdDevice 1.00
    0,
    0,
    0, // no string descriptors
    1, // one configuration
];

/// The configuration: one interface (vendor class) with a bulk IN and a bulk OUT, 512 bytes each.
pub const CONFIGURATION: [u8; 32] = [
    9,
    2,
    32,
    0,
    1,
    1,
    0,
    0x80,
    50, // configuration, total length 32, one interface, 100 mA
    9,
    4,
    0,
    0,
    2,
    0xff,
    0,
    0,
    0, // interface 0, two endpoints, vendor class
    7,
    5,
    BULK_IN,
    0x02,
    BULK_MPS as u8,
    (BULK_MPS >> 8) as u8,
    0, // bulk IN
    7,
    5,
    BULK_OUT,
    0x02,
    BULK_MPS as u8,
    (BULK_MPS >> 8) as u8,
    0, // bulk OUT
];

/// The language ID descriptor (string index 0): English (US), 0x0409. The device has no other strings.
pub const LANGUAGE: [u8; 4] = [4, 3, 0x09, 0x04];

/// The descriptor of `kind` (1 device, 2 configuration, 3 string 0 only), or `None` for one the
/// device does not have.
pub fn descriptor(kind: u8, index: u8) -> Option<&'static [u8]> {
    match (kind, index) {
        (1, 0) => Some(&DEVICE),
        (2, 0) => Some(&CONFIGURATION),
        (3, 0) => Some(&LANGUAGE),
        _ => None,
    }
}

impl<M: crate::medium::Medium, F: crate::wdt::Feed> BulkEndpoints for crate::stage::Stage<M, F> {
    fn on_out(&mut self, message: &[u8]) -> Result<(), crate::stage::Reboot> {
        crate::stage::Stage::on_out(self, message)
    }

    fn on_in(&mut self, out: &mut [u8]) -> Result<usize, NoReply> {
        crate::stage::Stage::on_in(self, out)
    }

    fn on_reset(&mut self) {
        self.abandon();
    }
}
