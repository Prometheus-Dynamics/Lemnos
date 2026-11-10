//! USB in both roles over a port the firmware implements on its USB peripheral.
//!
//! - [`Function`]: this board as a USB device ([`UsbFunction`]) over a [`FunctionPort`]. The
//!   port moves packets and reports bus events; [`Function`] keeps the descriptors, configures
//!   the endpoints they name, answers the host's `GET_DESCRIPTOR` from its copy, and turns the
//!   port's "not yet" into the hal's timeouts.
//! - [`Host`] and [`HostDevice`]: the board's USB host ([`UsbHost`], [`UsbDevice`]) over a
//!   [`HostPort`]. The port runs the transfers; [`HostDevice`] keeps the claims and makes the
//!   standard requests.
//!
//! Neither allocates. Timeouts are measured on the host's clock.
//!
//! # Descriptors
//!
//! [`UsbFunction::enable`] takes the device descriptor (18 bytes), then the whole configuration
//! descriptor (`wTotalLength` bytes, interfaces and endpoints included), then any string
//! descriptors in index order starting at 0 (the language list). One configuration, full speed.

use core::ops::Range;
use core::time::Duration;

use lemnos_core::HostClock;
use lemnos_hal::blocks::{
    ControlSetup, IsoPacket, UsbDevice, UsbDeviceId, UsbDeviceInfo, UsbFunction, UsbFunctionEvent,
    UsbHost,
};
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

/// What the port saw on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortEvent {
    Reset,
    Configured,
    Deconfigured,
    Suspended,
    Resumed,
    /// A control request on endpoint 0, waiting for [`FunctionPort::control_in`],
    /// [`FunctionPort::stall`] or the caller. `length` is the setup's `wLength`.
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
    fn disconnect(&mut self) -> Result<(), Self::Error>;
    fn poll(&mut self) -> Result<Option<PortEvent>, Self::Error>;
    /// Sends the data stage of the pending control-IN request (the port splits it in packets).
    fn control_in(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    /// Refuses the pending control request.
    fn stall(&mut self) -> Result<(), Self::Error>;
    /// A packet the host sent to `endpoint`, or `None` if none has come.
    fn read(&mut self, endpoint: u8, data: &mut [u8]) -> Result<Option<usize>, Self::Error>;
    /// Queues `data` for the host on `endpoint`, or `None` if the endpoint is still busy.
    fn write(&mut self, endpoint: u8, data: &[u8]) -> Result<Option<usize>, Self::Error>;
}

const DESCRIPTOR_DEVICE: u8 = 1;
const DESCRIPTOR_CONFIGURATION: u8 = 2;
const DESCRIPTOR_STRING: u8 = 3;
const DESCRIPTOR_ENDPOINT: u8 = 5;
const DEVICE_LEN: usize = 18;
const GET_DESCRIPTOR: u8 = 6;
const SET_INTERFACE: u8 = 11;
/// Device to host, standard, to the device.
const STANDARD_IN: u8 = 0x80;
/// Host to device, standard, to an interface.
const STANDARD_OUT_INTERFACE: u8 = 0x01;

/// Where each part of the descriptors sits in a [`Function`]'s copy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Layout {
    /// End of the configuration descriptor.
    config_end: usize,
    /// End of all of it.
    end: usize,
}

/// Checks `bytes` against the layout in the module docs, and calls `each` on every endpoint.
fn parse(bytes: &[u8], mut each: impl FnMut(Endpoint)) -> Result<Layout, ErrorKind> {
    let bad = ErrorKind::InvalidInput;
    let device = bytes.get(..DEVICE_LEN).ok_or(bad)?;
    if usize::from(device[0]) != DEVICE_LEN || device[1] != DESCRIPTOR_DEVICE {
        return Err(bad);
    }
    let config = bytes.get(DEVICE_LEN..DEVICE_LEN + 4).ok_or(bad)?;
    if config[1] != DESCRIPTOR_CONFIGURATION {
        return Err(bad);
    }
    let total = usize::from(u16::from_le_bytes([config[2], config[3]]));
    let config_end = DEVICE_LEN + total;
    let config = bytes.get(DEVICE_LEN..config_end).ok_or(bad)?;
    walk(config, |kind, body| {
        if kind == DESCRIPTOR_ENDPOINT {
            let [address, attributes, low, high, interval, ..] = *body else {
                return Err(bad);
            };
            each(Endpoint {
                address,
                kind: TransferKind::from_attributes(attributes),
                max_packet: u16::from_le_bytes([low, high]),
                interval,
            });
        }
        Ok(())
    })?;
    walk(&bytes[config_end..], |kind, _| {
        if kind == DESCRIPTOR_STRING {
            Ok(())
        } else {
            Err(bad)
        }
    })?;
    Ok(Layout {
        config_end,
        end: bytes.len(),
    })
}

/// Calls `each` with the type and the bytes after the header of every descriptor in `bytes`.
fn walk(
    bytes: &[u8],
    mut each: impl FnMut(u8, &[u8]) -> Result<(), ErrorKind>,
) -> Result<(), ErrorKind> {
    let mut at = 0;
    while at < bytes.len() {
        let len = usize::from(bytes[at]);
        if len < 2 || at + len > bytes.len() {
            return Err(ErrorKind::InvalidInput);
        }
        each(bytes[at + 1], &bytes[at + 2..at + len])?;
        at += len;
    }
    Ok(())
}

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

/// This board as a USB device over port `P`, keeping up to `N` bytes of descriptors.
pub struct Function<P, const N: usize> {
    port: P,
    clock: &'static dyn HostClock,
    descriptors: [u8; N],
    layout: Option<Layout>,
}

impl<P: FunctionPort, const N: usize> Function<P, N> {
    pub const fn new(port: P, clock: &'static dyn HostClock) -> Self {
        Self {
            port,
            clock,
            descriptors: [0; N],
            layout: None,
        }
    }

    pub fn port(&mut self) -> &mut P {
        &mut self.port
    }

    /// Answers a `GET_DESCRIPTOR`, or stalls it for a descriptor that isn't there.
    fn answer(&mut self, setup: ControlSetup, length: u16) -> Result<(), ErrorKind> {
        let [index, kind] = setup.value.to_le_bytes();
        let found = self
            .layout
            .and_then(|layout| find(&self.descriptors[..layout.end], layout, kind, index));
        match found {
            Some(range) => {
                let len = range.len().min(usize::from(length));
                let bytes = &self.descriptors[range.start..range.start + len];
                self.port.control_in(bytes).map_err(port_error)
            }
            None => self.port.stall().map_err(port_error),
        }
    }
}

/// Where the descriptor of `kind` and `index` is in `bytes`.
fn find(bytes: &[u8], layout: Layout, kind: u8, index: u8) -> Option<Range<usize>> {
    match (kind, index) {
        (DESCRIPTOR_DEVICE, 0) => Some(0..DEVICE_LEN),
        (DESCRIPTOR_CONFIGURATION, 0) => Some(DEVICE_LEN..layout.config_end),
        (DESCRIPTOR_STRING, _) => {
            let mut at = layout.config_end;
            for _ in 0..index {
                at += usize::from(*bytes.get(at)?);
            }
            let len = usize::from(*bytes.get(at)?);
            (at + len <= bytes.len()).then_some(at..at + len)
        }
        _ => None,
    }
}

impl<P: FunctionPort, const N: usize> UsbFunction for Function<P, N> {
    type Error = ErrorKind;

    fn enable(&mut self, descriptors: &[u8]) -> Result<(), ErrorKind> {
        if descriptors.len() > N {
            return Err(ErrorKind::InvalidInput);
        }
        // Checked whole before any endpoint is configured.
        parse(descriptors, |_| {})?;
        let port = &mut self.port;
        let mut failed = None;
        let layout = parse(descriptors, |endpoint| {
            if failed.is_none()
                && let Err(error) = port.configure(endpoint)
            {
                failed = Some(error.kind());
            }
        })?;
        if let Some(error) = failed {
            return Err(error);
        }
        self.descriptors[..descriptors.len()].copy_from_slice(descriptors);
        self.layout = Some(layout);
        self.port.connect().map_err(port_error)
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        self.layout = None;
        self.port.disconnect().map_err(port_error)
    }

    fn next_event(&mut self) -> Result<Option<UsbFunctionEvent>, ErrorKind> {
        loop {
            let event = match self.port.poll().map_err(port_error)? {
                None => return Ok(None),
                Some(PortEvent::Reset | PortEvent::Deconfigured) => UsbFunctionEvent::Deconfigured,
                Some(PortEvent::Configured) => UsbFunctionEvent::Configured,
                Some(PortEvent::Suspended) => UsbFunctionEvent::Suspended,
                Some(PortEvent::Resumed) => UsbFunctionEvent::Resumed,
                Some(PortEvent::Setup { setup, length }) => {
                    if setup.request_type == STANDARD_IN && setup.request == GET_DESCRIPTOR {
                        self.answer(setup, length)?;
                        continue;
                    }
                    UsbFunctionEvent::Setup(setup)
                }
            };
            return Ok(Some(event));
        }
    }

    fn read(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        if self.layout.is_none() {
            return Err(ErrorKind::Unavailable);
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
        if self.layout.is_none() {
            return Err(ErrorKind::Unavailable);
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
    /// An isochronous transfer, packed as [`UsbDevice::iso_in`] says.
    fn iso_in(
        &mut self,
        device: UsbDeviceId,
        endpoint: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), Self::Error>;
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
            value: u16::from_le_bytes([index, DESCRIPTOR_CONFIGURATION]),
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

    fn iso_in(
        &mut self,
        endpoint: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        timeout: Duration,
    ) -> Result<(), ErrorKind> {
        self.port
            .iso_in(self.id, endpoint, data, packets, timeout)
            .map_err(port_error)
    }
}
