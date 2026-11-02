//! The Linux USB transport: `lemnos-usb` (usbdevfs) behind the bus session.

use super::{UsbTarget, UsbTransport};
use crate::backend::LinuxTransportConfig;
use lemnos_bus::{BusError, BusResult};
use lemnos_core::{
    UsbControlTransfer, UsbDirection, UsbInterruptTransfer, UsbRecipient, UsbRequestType,
};
use lemnos_usb::{
    ControlKind, ControlRecipient, ControlSetup, Direction, Error as UsbError, ErrorKind,
    Interface, UsbDevice, UsbLocation,
};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub(super) struct LinuxUsbTransport {
    device_id: lemnos_core::DeviceId,
    device: UsbDevice,
    claimed_interfaces: Mutex<BTreeMap<u8, Interface>>,
    default_timeout: Duration,
}

impl LinuxUsbTransport {
    pub(super) fn new(
        device_id: lemnos_core::DeviceId,
        target: UsbTarget,
        transport_config: &LinuxTransportConfig,
    ) -> BusResult<Self> {
        let location = UsbLocation::new(target.bus.to_string(), target.ports.clone());
        let device = UsbDevice::open_location(&location).map_err(|error| match error.kind() {
            ErrorKind::NotFound => BusError::SessionUnavailable {
                device_id: device_id.clone(),
                reason: format!(
                    "no Linux USB device matched {}",
                    format_usb_target(target.bus, &target.ports)
                ),
            },
            _ => classify_open_error(&device_id, target.bus, &target.ports, &error),
        })?;

        Ok(Self {
            device_id,
            device,
            claimed_interfaces: Mutex::new(BTreeMap::new()),
            default_timeout: Duration::from_millis(transport_config.usb_timeout_ms),
        })
    }

    fn invalid_request(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::InvalidRequest {
            device_id: self.device_id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn transport_failure(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::TransportFailure {
            device_id: self.device_id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    /// Recover poisoned locks so a panic in another caller does not
    /// permanently orphan the owned device handle for this session.
    fn lock_claimed_interfaces(&self) -> MutexGuard<'_, BTreeMap<u8, Interface>> {
        self.claimed_interfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Maps a transfer error into the bus error for `operation`.
    fn transfer_error(&self, operation: &'static str, error: UsbError) -> BusError {
        match error.kind() {
            ErrorKind::InvalidInput => self.invalid_request(operation, error.message()),
            _ => self.transport_failure(operation, error.to_string()),
        }
    }
}

impl UsbTransport for LinuxUsbTransport {
    fn close(&mut self) -> BusResult<()> {
        // Dropping the claims releases them (and reattaches any driver a claim detached).
        self.lock_claimed_interfaces().clear();
        Ok(())
    }

    fn control_transfer(&mut self, transfer: &UsbControlTransfer) -> BusResult<Vec<u8>> {
        let setup = ControlSetup {
            direction: match transfer.setup.direction {
                UsbDirection::In => Direction::In,
                UsbDirection::Out => Direction::Out,
            },
            kind: match transfer.setup.request_type {
                UsbRequestType::Standard => ControlKind::Standard,
                UsbRequestType::Class => ControlKind::Class,
                UsbRequestType::Vendor => ControlKind::Vendor,
                UsbRequestType::Reserved => {
                    return Err(self.invalid_request(
                        "usb.control_transfer",
                        "reserved control request types are not supported",
                    ));
                }
            },
            recipient: match transfer.setup.recipient {
                UsbRecipient::Device => ControlRecipient::Device,
                UsbRecipient::Interface => ControlRecipient::Interface,
                UsbRecipient::Endpoint => ControlRecipient::Endpoint,
                UsbRecipient::Other => ControlRecipient::Other,
            },
            request: transfer.setup.request,
            value: transfer.setup.value,
            index: transfer.setup.index,
        };
        let timeout = timeout_duration(transfer.timeout_ms, self.default_timeout);
        match transfer.setup.direction {
            UsbDirection::In => {
                let length = u16::try_from(transfer.data.len()).map_err(|_| {
                    self.invalid_request(
                        "usb.control_transfer",
                        "control read longer than 65535 bytes",
                    )
                })?;
                self.device
                    .control_in(&setup, length, timeout)
                    .map_err(|error| self.transfer_error("usb.control_transfer", error))
            }
            UsbDirection::Out => self
                .device
                .control_out(&setup, &transfer.data, timeout)
                .map(|_| Vec::new())
                .map_err(|error| self.transfer_error("usb.control_transfer", error)),
        }
    }

    fn bulk_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        if endpoint == 0 {
            return Err(self.invalid_request(
                "usb.bulk_read",
                "bulk read endpoint must be greater than zero",
            ));
        }
        self.device
            .bulk_read(
                endpoint,
                buffer,
                timeout_duration(timeout_ms, self.default_timeout),
            )
            .map_err(|error| self.transfer_error("usb.bulk_read", error))
    }

    fn bulk_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        let mut buffer = vec![0; length as usize];
        let bytes_read = self.bulk_read_into(endpoint, &mut buffer, timeout_ms)?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }

    fn bulk_write(&mut self, endpoint: u8, bytes: &[u8], timeout_ms: Option<u32>) -> BusResult<()> {
        if endpoint == 0 {
            return Err(self.invalid_request(
                "usb.bulk_write",
                "bulk write endpoint must be greater than zero",
            ));
        }
        if bytes.is_empty() {
            return Err(
                self.invalid_request("usb.bulk_write", "bulk write payload must not be empty")
            );
        }
        self.device
            .bulk_write(
                endpoint,
                bytes,
                timeout_duration(timeout_ms, self.default_timeout),
            )
            .map(|_| ())
            .map_err(|error| self.transfer_error("usb.bulk_write", error))
    }

    fn interrupt_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        if endpoint == 0 {
            return Err(self.invalid_request(
                "usb.interrupt_read",
                "interrupt read endpoint must be greater than zero",
            ));
        }
        self.device
            .interrupt_read(
                endpoint,
                buffer,
                timeout_duration(timeout_ms, self.default_timeout),
            )
            .map_err(|error| self.transfer_error("usb.interrupt_read", error))
    }

    fn interrupt_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        let mut buffer = vec![0; length as usize];
        let bytes_read = self.interrupt_read_into(endpoint, &mut buffer, timeout_ms)?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }

    fn interrupt_write(&mut self, transfer: &UsbInterruptTransfer) -> BusResult<()> {
        if transfer.endpoint == 0 {
            return Err(self.invalid_request(
                "usb.interrupt_write",
                "interrupt write endpoint must be greater than zero",
            ));
        }
        if transfer.bytes.is_empty() {
            return Err(self.invalid_request(
                "usb.interrupt_write",
                "interrupt write payload must not be empty",
            ));
        }
        self.device
            .interrupt_write(
                transfer.endpoint,
                &transfer.bytes,
                timeout_duration(transfer.timeout_ms, self.default_timeout),
            )
            .map(|_| ())
            .map_err(|error| self.transfer_error("usb.interrupt_write", error))
    }

    fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()> {
        let interface = self
            .device
            .claim_interface(interface_number)
            .map_err(|error| {
                self.transport_failure(
                    "usb.claim_interface",
                    format!("USB interface claim failed: {error}"),
                )
            })?;
        if let Some(alternate_setting) = alternate_setting {
            // A failed alternate setting drops `interface`, which releases the claim.
            interface
                .set_alternate(alternate_setting)
                .map_err(|error| {
                    self.transport_failure(
                        "usb.claim_interface",
                        format!("USB alternate setting change failed: {error}"),
                    )
                })?;
        }
        self.lock_claimed_interfaces()
            .insert(interface_number, interface);
        Ok(())
    }

    fn release_interface(&mut self, interface_number: u8) -> BusResult<()> {
        self.lock_claimed_interfaces()
            .remove(&interface_number)
            .map(drop)
            .ok_or_else(|| {
                self.transport_failure(
                    "usb.release_interface",
                    format!(
                        "USB interface release failed: interface {interface_number} is not claimed"
                    ),
                )
            })
    }
}

pub(super) fn classify_open_error(
    device_id: &lemnos_core::DeviceId,
    bus: u16,
    ports: &[u8],
    error: &UsbError,
) -> BusError {
    let target = format_usb_target(bus, ports);

    match error.kind() {
        ErrorKind::PermissionDenied => BusError::PermissionDenied {
            device_id: device_id.clone(),
            operation: "open",
            reason: format!("failed to open Linux USB device on {target}: {error}"),
        },
        ErrorKind::Busy => BusError::AccessConflict {
            device_id: device_id.clone(),
            reason: format!("Linux USB device on {target} is already in use"),
        },
        ErrorKind::Disconnected | ErrorKind::NotFound => BusError::SessionUnavailable {
            device_id: device_id.clone(),
            reason: format!("Linux USB device on {target} is not currently available: {error}"),
        },
        _ => BusError::TransportFailure {
            device_id: device_id.clone(),
            operation: "open",
            reason: format!("failed to open Linux USB device on {target}: {error}"),
        },
    }
}

fn format_usb_target(bus: u16, ports: &[u8]) -> String {
    format!(
        "bus {bus} ports {}",
        ports
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(".")
    )
}

fn timeout_duration(timeout_ms: Option<u32>, default_timeout: Duration) -> Duration {
    timeout_ms
        .map(|timeout| Duration::from_millis(u64::from(timeout)))
        .unwrap_or(default_timeout)
}
