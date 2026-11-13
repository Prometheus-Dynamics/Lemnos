//! USB in both roles over a port the firmware implements on its USB peripheral.
//!
//! - [`Function`]: this board as a USB device ([`UsbFunction`]) over a [`FunctionPort`]. The
//!   port moves packets and reports bus events; [`Function`] keeps the descriptors, configures
//!   the endpoints they name, answers the host's `GET_DESCRIPTOR` from its copy, keeps track of
//!   the control request being answered, and turns the port's "not yet" into the hal's
//!   timeouts.
//! - [`Host`] and [`HostDevice`]: the board's USB host ([`UsbHost`], [`UsbDevice`]) over a
//!   [`HostPort`]. The port runs the transfers; [`HostDevice`] keeps the claims and makes the
//!   standard requests.
//!
//! Neither allocates. Timeouts are measured on the host's clock.
//!
//! # Descriptors
//!
//! [`UsbFunction::enable`] takes a descriptor set in the layout of
//! [`lemnos_hal::usb_descriptors`], checked there. The device descriptor, the full-speed
//! configuration and the strings are served to the host as they stand. The port runs at full
//! speed, so a high-speed configuration in the set is kept and not used.
//!
//! An endpoint named in several alternate settings is configured once, as the first names it.
//! `SET_INTERFACE` and `GET_INTERFACE` come to the function as requests like any other, for
//! it to accept, answer or refuse. An isochronous endpoint's `read` and `write` are one
//! frame each, checked against the largest packet any setting gives it; how many frames a
//! port keeps queued with its peripheral is the port's to say.

use core::time::Duration;

use lemnos_core::HostClock;
use lemnos_hal::blocks::{
    BufferHandle, ControlSetup, IsoPacket, UsbDevice, UsbDeviceId, UsbDeviceInfo, UsbFunction,
    UsbFunctionEvent, UsbHost, iso_interval_bytes, iso_shape_ok,
};
use lemnos_hal::usb_descriptors::{self, UsbDescriptors, UsbEndpoint};
use lemnos_hal::{ErrorKind, HalError};

/// How an endpoint moves data, from the low two bits of `bmAttributes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

impl TransferKind {
    const fn from_attributes(attributes: u8) -> Self {
        match attributes & 3 {
            0 => Self::Control,
            1 => Self::Isochronous,
            2 => Self::Bulk,
            _ => Self::Interrupt,
        }
    }
}

/// An endpoint named by the configuration descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    /// Number, with bit 7 set for IN.
    pub address: u8,
    pub kind: TransferKind,
    pub max_packet: u16,
    pub interval: u8,
}

impl From<UsbEndpoint> for Endpoint {
    fn from(endpoint: UsbEndpoint) -> Self {
        Self {
            address: endpoint.address,
            kind: TransferKind::from_attributes(endpoint.attributes),
            max_packet: endpoint.max_packet,
            interval: endpoint.interval,
        }
    }
}

/// What the port saw on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortEvent {
    /// A bus reset, or the host went away. The port has dropped the configuration, any control
    /// request and every halt.
    Reset,
    Configured,
    Deconfigured,
    Suspended,
    Resumed,
    /// A control request on endpoint 0, waiting for [`FunctionPort::control_in`],
    /// [`FunctionPort::control_out`] or [`FunctionPort::stall`]. `length` is the setup's
    /// `wLength`. A request that arrives while another waits replaces it.
    Setup {
        setup: ControlSetup,
        length: u16,
    },
}

/// The firmware's USB device peripheral. No call waits: one that can't finish yet answers
/// `None` and is made again.
pub trait FunctionPort {
    type Error: HalError;

    fn configure(&mut self, endpoint: Endpoint) -> Result<(), Self::Error>;
    /// Pulls up the data line so the host sees the device.
    fn connect(&mut self) -> Result<(), Self::Error>;
    /// Lets go of the data line and forgets the endpoints that were configured.
    fn disconnect(&mut self) -> Result<(), Self::Error>;
    fn poll(&mut self) -> Result<Option<PortEvent>, Self::Error>;
    /// Sends `data` as the data stage of the waiting control request, which is one whose data
    /// goes to the host, and takes the host's status stage. The port splits `data` in packets
    /// and ends the stage short when `data` is shorter than the request's `wLength`; `data` is
    /// never longer. An error of kind `Unavailable` when the request is no longer waiting.
    fn control_in(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    /// The data stage of the waiting control request, which is one whose data comes from the
    /// host, once all of it has come, and the status stage that accepts the request. `None`
    /// while data is still to come. A request with a `wLength` of 0 is accepted at once with
    /// `Some(0)`. `data` holds the request's `wLength` bytes. An error of kind `Unavailable`
    /// when the request is no longer waiting.
    fn control_out(&mut self, data: &mut [u8]) -> Result<Option<usize>, Self::Error>;
    /// Refuses the waiting control request with a protocol stall on endpoint 0, which the next
    /// setup packet clears.
    fn stall(&mut self) -> Result<(), Self::Error>;
    /// Halts `endpoint` so it answers the host with a stall, or clears the halt and resets the
    /// endpoint's data toggle. The port clears a halt by itself when the host asks
    /// (`CLEAR_FEATURE(ENDPOINT_HALT)`) and on a reset.
    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<(), Self::Error>;
    /// A packet the host sent to `endpoint`, or `None` if none has come.
    fn read(&mut self, endpoint: u8, data: &mut [u8]) -> Result<Option<usize>, Self::Error>;
    /// Queues `data` for the host on `endpoint`, or `None` if the endpoint is still busy.
    fn write(&mut self, endpoint: u8, data: &[u8]) -> Result<Option<usize>, Self::Error>;
}

const GET_DESCRIPTOR: u8 = 6;
const SET_INTERFACE: u8 = 11;
/// Device to host, standard, to the device.
const STANDARD_IN: u8 = 0x80;
/// Host to device, standard, to an interface.
const STANDARD_OUT_INTERFACE: u8 = 0x01;

fn port_error<E: HalError>(error: E) -> ErrorKind {
    error.kind()
}

/// Whether `deadline_ns` has passed on `clock`.
fn passed(clock: &dyn HostClock, deadline_ns: u64) -> bool {
    clock.now_ns() >= deadline_ns
}

fn deadline(clock: &dyn HostClock, timeout: Duration) -> u64 {
    let timeout = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
    clock.now_ns().saturating_add(timeout)
}

/// The bit that stands for an endpoint address: 1 to 15 for OUT, 17 to 31 for IN.
const fn endpoint_bit(address: u8) -> u32 {
    1 << ((address & 0x0f) as u32 + (address >> 7) as u32 * 16)
}

/// What an enabled [`Function`] knows of its descriptors without reading them again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Enabled {
    /// Bytes of the descriptor copy in use.
    len: usize,
    /// The endpoints the full-speed configuration names, by [`endpoint_bit`].
    named: u32,
    /// Those of them that are isochronous.
    isochronous: u32,
}

/// This board as a USB device over port `P`, keeping up to `N` bytes of descriptors.
pub struct Function<P, const N: usize> {
    port: P,
    clock: &'static dyn HostClock,
    descriptors: [u8; N],
    enabled: Option<Enabled>,
    configured: bool,
    /// The control request waiting for the function's answer, and its `wLength`.
    pending: Option<(ControlSetup, u16)>,
}

impl<P: FunctionPort, const N: usize> Function<P, N> {
    pub const fn new(port: P, clock: &'static dyn HostClock) -> Self {
        Self {
            port,
            clock,
            descriptors: [0; N],
            enabled: None,
            configured: false,
            pending: None,
        }
    }

    pub fn port(&mut self) -> &mut P {
        &mut self.port
    }

    /// Answers a `GET_DESCRIPTOR`, or stalls it for a descriptor that isn't there.
    fn answer(&mut self, setup: ControlSetup, length: u16) -> Result<(), ErrorKind> {
        let [index, kind] = setup.value.to_le_bytes();
        let found = self
            .enabled
            .and_then(|enabled| UsbDescriptors::parse(&self.descriptors[..enabled.len]).ok())
            .and_then(|set| match (kind, index) {
                (usb_descriptors::DEVICE, 0) => Some(set.device()),
                (usb_descriptors::CONFIGURATION, 0) => Some(set.full_speed()),
                (usb_descriptors::STRING, _) => set.string(index),
                _ => None,
            });
        match found {
            Some(bytes) => {
                let len = bytes.len().min(usize::from(length));
                self.port.control_in(&bytes[..len]).map_err(port_error)
            }
            None => self.port.stall().map_err(port_error),
        }
    }

    /// Puts the endpoints on the port and connects.
    fn bring_up(&mut self, descriptors: &[u8]) -> Result<Enabled, ErrorKind> {
        let set = UsbDescriptors::parse(descriptors)?;
        let mut enabled = Enabled {
            len: descriptors.len(),
            named: 0,
            isochronous: 0,
        };
        for endpoint in usb_descriptors::endpoints(set.full_speed()) {
            let endpoint = Endpoint::from(endpoint);
            // An address that comes again in another alternate setting is the same endpoint.
            if enabled.named & endpoint_bit(endpoint.address) == 0 {
                self.port.configure(endpoint).map_err(port_error)?;
            }
            enabled.named |= endpoint_bit(endpoint.address);
            if endpoint.kind == TransferKind::Isochronous {
                enabled.isochronous |= endpoint_bit(endpoint.address);
            }
        }
        self.port.connect().map_err(port_error)?;
        Ok(enabled)
    }

    /// The `wLength` of the pending request, if its data goes to the host (`to_host`) or
    /// comes from it.
    fn pending(&self, to_host: bool) -> Result<usize, ErrorKind> {
        if self.enabled.is_none() {
            return Err(ErrorKind::Unavailable);
        }
        match self.pending {
            Some((setup, length)) if setup.is_in() == to_host => Ok(usize::from(length)),
            _ => Err(ErrorKind::InvalidInput),
        }
    }

    /// Checks that `endpoint` is one the descriptors name, in the direction asked for if one
    /// is, and that the host has configured the function.
    fn usable(&self, endpoint: u8, to_host: Option<bool>) -> Result<Enabled, ErrorKind> {
        let enabled = self.enabled.ok_or(ErrorKind::Unavailable)?;
        let named = endpoint & 0x70 == 0
            && endpoint & 0x0f != 0
            && enabled.named & endpoint_bit(endpoint) != 0;
        if !named || to_host.is_some_and(|to_host| to_host != (endpoint & 0x80 != 0)) {
            return Err(ErrorKind::InvalidInput);
        }
        if !self.configured {
            return Err(ErrorKind::Unavailable);
        }
        Ok(enabled)
    }

    /// The bytes a frame of the isochronous `endpoint` carries, the most any setting gives
    /// it; `None` for any other endpoint.
    fn iso_bytes(&self, enabled: Enabled, endpoint: u8) -> Option<usize> {
        if enabled.isochronous & endpoint_bit(endpoint) == 0 {
            return None;
        }
        let set = UsbDescriptors::parse(&self.descriptors[..enabled.len]).ok()?;
        usb_descriptors::endpoints(set.full_speed())
            .filter(|e| e.address == endpoint)
            .map(|e| iso_interval_bytes(e.max_packet) as usize)
            .max()
    }
}

impl<P: FunctionPort, const N: usize> UsbFunction for Function<P, N> {
    type Error = ErrorKind;

    /// The port hands bytes over one transfer at a time: there is no descriptor to lend.
    fn endpoint_handles(&self) -> Result<[BufferHandle<'_>; 2], ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn enable(&mut self, descriptors: &[u8]) -> Result<(), ErrorKind> {
        if self.enabled.is_some() {
            return Err(ErrorKind::Busy);
        }
        if descriptors.len() > N {
            return Err(ErrorKind::InvalidInput);
        }
        // Checked whole before any endpoint is configured.
        UsbDescriptors::parse(descriptors)?;
        match self.bring_up(descriptors) {
            Ok(enabled) => {
                self.descriptors[..descriptors.len()].copy_from_slice(descriptors);
                self.enabled = Some(enabled);
                Ok(())
            }
            Err(error) => {
                // Nothing of the attempt stays: the port forgets the endpoints it was given.
                let _ = self.port.disconnect();
                Err(error)
            }
        }
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        self.pending = None;
        self.configured = false;
        if self.enabled.take().is_none() {
            return Ok(());
        }
        self.port.disconnect().map_err(port_error)
    }

    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, ErrorKind> {
        if self.enabled.is_none() {
            return Err(ErrorKind::Unavailable);
        }
        if self.pending.take().is_some() {
            self.port.stall().map_err(port_error)?;
        }
        loop {
            let event = match self.port.poll().map_err(port_error)? {
                None => return Ok(None),
                Some(PortEvent::Reset | PortEvent::Deconfigured) => {
                    if !core::mem::take(&mut self.configured) {
                        continue;
                    }
                    UsbFunctionEvent::Deconfigured
                }
                Some(PortEvent::Configured) => {
                    self.configured = true;
                    UsbFunctionEvent::Configured
                }
                Some(PortEvent::Suspended) => UsbFunctionEvent::Suspended,
                Some(PortEvent::Resumed) => UsbFunctionEvent::Resumed,
                Some(PortEvent::Setup { setup, length }) => {
                    if setup.request_type == STANDARD_IN && setup.request == GET_DESCRIPTOR {
                        self.answer(setup, length)?;
                        continue;
                    }
                    self.pending = Some((setup, length));
                    UsbFunctionEvent::Setup { setup, length }
                }
            };
            return Ok(Some(event));
        }
    }

    /// The port takes the whole data stage at once, so this never waits.
    fn control_in(&mut self, data: &[u8], _timeout: Duration) -> Result<usize, ErrorKind> {
        let length = self.pending(true)?;
        self.pending = None;
        let len = data.len().min(length);
        self.port.control_in(&data[..len]).map_err(port_error)?;
        Ok(len)
    }

    fn control_out(&mut self, data: &mut [u8], timeout: Duration) -> Result<usize, ErrorKind> {
        let length = self.pending(false)?;
        let data = data.get_mut(..length).ok_or(ErrorKind::InvalidInput)?;
        self.pending = None;
        let until = deadline(self.clock, timeout);
        loop {
            if let Some(n) = self.port.control_out(data).map_err(port_error)? {
                return Ok(n.min(length));
            }
            if passed(self.clock, until) {
                // The host never sent its data: the request ends here, refused.
                let _ = self.port.stall();
                return Err(ErrorKind::Timeout);
            }
        }
    }

    fn control_stall(&mut self) -> Result<(), ErrorKind> {
        if self.enabled.is_none() {
            return Err(ErrorKind::Unavailable);
        }
        if self.pending.take().is_none() {
            return Err(ErrorKind::InvalidInput);
        }
        self.port.stall().map_err(port_error)
    }

    fn set_halt(&mut self, endpoint: u8, halted: bool) -> Result<(), ErrorKind> {
        let enabled = self.usable(endpoint, None)?;
        if enabled.isochronous & endpoint_bit(endpoint) != 0 {
            return Err(ErrorKind::Unsupported);
        }
        self.port.set_halt(endpoint, halted).map_err(port_error)
    }

    fn read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        let enabled = self.usable(endpoint, Some(false))?;
        // An isochronous read is one frame, and has room for all of it.
        if self
            .iso_bytes(enabled, endpoint)
            .is_some_and(|bytes| data.len() < bytes)
        {
            return Err(ErrorKind::InvalidInput);
        }
        let until = deadline(self.clock, timeout);
        loop {
            if let Some(n) = self.port.read(endpoint, data).map_err(port_error)? {
                return Ok(n);
            }
            if passed(self.clock, until) {
                return Err(ErrorKind::Timeout);
            }
        }
    }

    fn write(&mut self, endpoint: u8, data: &[u8], timeout: Duration) -> Result<usize, ErrorKind> {
        let enabled = self.usable(endpoint, Some(true))?;
        // An isochronous write is one frame, and is never cut.
        if self
            .iso_bytes(enabled, endpoint)
            .is_some_and(|bytes| data.len() > bytes)
        {
            return Err(ErrorKind::InvalidInput);
        }
        let until = deadline(self.clock, timeout);
        loop {
            if let Some(n) = self.port.write(endpoint, data).map_err(port_error)? {
                return Ok(n);
            }
            if passed(self.clock, until) {
                return Err(ErrorKind::Timeout);
            }
        }
    }
}

/// The firmware's USB host peripheral. Each transfer runs to completion or to its timeout. A
/// port is a cheap handle (`Clone`) to the peripheral, so each opened device keeps one.
pub trait HostPort: Clone {
    type Error: HalError;

    /// Fills `out` and returns how many devices there are, which may be more than fit.
    fn enumerate(&mut self, out: &mut [UsbDeviceInfo]) -> Result<usize, Self::Error>;
    /// Makes ready to talk to `device`, failing if it isn't plugged in.
    fn open(&mut self, device: UsbDeviceId) -> Result<(), Self::Error>;
    /// Resets `device` at its port, with the result [`UsbDevice::reset`] defines: `Ok` when it
    /// is still the same device, an error of kind `Unavailable` when it left or came back as a
    /// different one (and for every call on `device` after that), any other error when the
    /// reset was not carried out.
    fn reset(&mut self, device: UsbDeviceId) -> Result<(), Self::Error>;
    /// A control transfer; `wLength` is `data.len()`.
    fn control_in(
        &mut self,
        device: UsbDeviceId,
        setup: ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    fn control_out(
        &mut self,
        device: UsbDeviceId,
        setup: ControlSetup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    /// A bulk or interrupt transfer from the device.
    fn transfer_in(
        &mut self,
        device: UsbDeviceId,
        endpoint: u8,
        kind: TransferKind,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    /// A bulk or interrupt transfer to the device.
    fn transfer_out(
        &mut self,
        device: UsbDeviceId,
        endpoint: u8,
        kind: TransferKind,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, Self::Error>;
    /// Starts an isochronous stream from `device`, as [`UsbDevice::iso_in_start`] says. The
    /// shape is one [`iso_shape_ok`] takes.
    fn iso_in_start(
        &mut self,
        device: UsbDeviceId,
        endpoint: u8,
        transfers: u8,
        packets: u16,
        packet_bytes: u32,
    ) -> Result<(), Self::Error>;
    /// The stream's next transfer, as [`UsbDevice::iso_in_next`] says.
    fn iso_in_next(
        &mut self,
        device: UsbDeviceId,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), Self::Error>;
    /// Ends the stream, as [`UsbDevice::iso_in_stop`] says.
    fn iso_in_stop(&mut self, device: UsbDeviceId) -> Result<(), Self::Error>;
}

/// The board's USB host over port `P`.
#[derive(Debug, Clone)]
pub struct Host<P> {
    port: P,
}

impl<P: HostPort> Host<P> {
    pub const fn new(port: P) -> Self {
        Self { port }
    }
}

impl<P: HostPort> UsbHost for Host<P> {
    type Error = ErrorKind;
    type Device = HostDevice<P>;

    fn enumerate(&mut self, out: &mut [UsbDeviceInfo]) -> Result<usize, ErrorKind> {
        self.port.enumerate(out).map_err(port_error)
    }

    fn open(&mut self, id: UsbDeviceId) -> Result<HostDevice<P>, ErrorKind> {
        self.port.open(id).map_err(port_error)?;
        Ok(HostDevice {
            port: self.port.clone(),
            id,
            claimed: 0,
        })
    }
}

/// A device opened through a [`Host`]. Claims are kept here: interfaces 0 to 31.
#[derive(Debug)]
pub struct HostDevice<P> {
    port: P,
    id: UsbDeviceId,
    claimed: u32,
}

impl<P> HostDevice<P> {
    pub const fn id(&self) -> UsbDeviceId {
        self.id
    }

    fn bit(interface: u8) -> Result<u32, ErrorKind> {
        1u32.checked_shl(u32::from(interface))
            .ok_or(ErrorKind::InvalidInput)
    }

    /// Whether `interface` is claimed.
    pub fn holds(&self, interface: u8) -> bool {
        Self::bit(interface).is_ok_and(|bit| self.claimed & bit != 0)
    }
}

/// How long a standard request made for the caller may take.
const STANDARD_TIMEOUT: Duration = Duration::from_secs(1);

impl<P: HostPort> UsbDevice for HostDevice<P> {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, index: u8, out: &mut [u8]) -> Result<usize, ErrorKind> {
        let setup = ControlSetup {
            request_type: STANDARD_IN,
            request: GET_DESCRIPTOR,
            value: u16::from_le_bytes([index, usb_descriptors::CONFIGURATION]),
            index: 0,
        };
        self.port
            .control_in(self.id, setup, out, STANDARD_TIMEOUT)
            .map_err(port_error)
    }

    /// The port's reset. Claims are kept here, so they stand when the device does.
    fn reset(&mut self) -> Result<(), ErrorKind> {
        self.port.reset(self.id).map_err(port_error)
    }

    /// Firmware has no drivers of its own bound to anything, so there is never one to detach.
    fn detach_kernel_driver(&mut self, _interface: u8) -> Result<(), ErrorKind> {
        Ok(())
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        self.claimed |= Self::bit(interface)?;
        Ok(())
    }

    fn release_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        if !self.holds(interface) {
            return Err(ErrorKind::InvalidInput);
        }
        self.claimed &= !Self::bit(interface)?;
        Ok(())
    }

    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Result<(), ErrorKind> {
        if !self.holds(interface) {
            return Err(ErrorKind::InvalidInput);
        }
        let setup = ControlSetup {
            request_type: STANDARD_OUT_INTERFACE,
            request: SET_INTERFACE,
            value: u16::from(alternate),
            index: u16::from(interface),
        };
        self.port
            .control_out(self.id, setup, &[], STANDARD_TIMEOUT)
            .map(drop)
            .map_err(port_error)
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.port
            .control_in(self.id, setup, data, timeout)
            .map_err(port_error)
    }

    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.port
            .control_out(self.id, setup, data, timeout)
            .map_err(port_error)
    }

    fn bulk_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.port
            .transfer_in(self.id, endpoint, TransferKind::Bulk, data, timeout)
            .map_err(port_error)
    }

    fn bulk_out(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.port
            .transfer_out(self.id, endpoint, TransferKind::Bulk, data, timeout)
            .map_err(port_error)
    }

    fn interrupt_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        self.port
            .transfer_in(self.id, endpoint, TransferKind::Interrupt, data, timeout)
            .map_err(port_error)
    }

    /// A shape out of range never reaches the port.
    fn iso_in_start(
        &mut self,
        endpoint: u8,
        transfers: u8,
        packets: u16,
        packet_bytes: u32,
    ) -> Result<(), ErrorKind> {
        if !iso_shape_ok(transfers, packets, packet_bytes) {
            return Err(ErrorKind::InvalidInput);
        }
        self.port
            .iso_in_start(self.id, endpoint, transfers, packets, packet_bytes)
            .map_err(port_error)
    }

    fn iso_in_next(
        &mut self,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), ErrorKind> {
        self.port
            .iso_in_next(self.id, data, packets, timeout)
            .map_err(port_error)
    }

    fn iso_in_stop(&mut self) -> Result<(), ErrorKind> {
        self.port.iso_in_stop(self.id).map_err(port_error)
    }
}
