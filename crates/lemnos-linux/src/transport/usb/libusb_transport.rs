use super::{UsbTarget, UsbTransport};
use crate::backend::LinuxTransportConfig;
use lemnos_bus::{BusError, BusResult};
use lemnos_core::{
    UsbControlTransfer, UsbDirection, UsbInterruptTransfer, UsbRecipient, UsbRequestType,
};
use rusb::{
    Context, DeviceHandle, Direction, Error as RusbError, Recipient, RequestType, UsbContext,
};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub(super) struct LinuxLibusbTransport {
    device_id: lemnos_core::DeviceId,
    _context: Context,
    handle: Mutex<DeviceHandle<Context>>,
    claimed_interfaces: Mutex<BTreeMap<u8, Option<u8>>>,
    default_timeout: Duration,
}

impl LinuxLibusbTransport {
    pub(super) fn new(
        device_id: lemnos_core::DeviceId,
        target: UsbTarget,
        transport_config: &LinuxTransportConfig,
    ) -> BusResult<Self> {
        let context = Context::new().map_err(|error| BusError::TransportFailure {
            device_id: device_id.clone(),
            operation: "open",
            reason: format!("failed to create libusb context: {error}"),
        })?;
        let handle = open_usb_handle(&context, &device_id, target.bus, &target.ports)?;

        Ok(Self {
            device_id,
            _context: context,
            handle: Mutex::new(handle),
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

    /// Recover poisoned libusb locks so a panic in another caller does not
    /// permanently orphan the owned device handle for this session.
    fn lock_handle(&self) -> MutexGuard<'_, DeviceHandle<Context>> {
        self.handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Claimed-interface bookkeeping is session-local state; recovering poison
    /// keeps later release/reclaim operations possible after panic recovery.
    fn lock_claimed_interfaces(&self) -> MutexGuard<'_, BTreeMap<u8, Option<u8>>> {
        self.claimed_interfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn remember_claimed_interface(&self, interface_number: u8, alternate_setting: Option<u8>) {
        self.lock_claimed_interfaces()
            .insert(interface_number, alternate_setting);
    }

    fn forget_claimed_interface(&self, interface_number: u8) {
        self.lock_claimed_interfaces().remove(&interface_number);
    }

    fn claim_interface_on_handle(
        &self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()> {
        let handle = self.lock_handle();
        handle.claim_interface(interface_number).map_err(|error| {
            self.transport_failure(
                "usb.claim_interface",
                format!("USB interface claim failed: {error}"),
            )
        })?;

        if let Some(alternate_setting) = alternate_setting
            && let Err(error) = handle.set_alternate_setting(interface_number, alternate_setting)
        {
            let rollback_error = handle.release_interface(interface_number).err();
            let reason = match rollback_error {
                Some(rollback_error) => format!(
                    "USB alternate setting change failed: {error}; rollback release failed: {rollback_error}"
                ),
                None => format!("USB alternate setting change failed: {error}"),
            };
            return Err(self.transport_failure("usb.claim_interface", reason));
        }

        Ok(())
    }

    fn release_interface_on_handle(&self, interface_number: u8) -> BusResult<()> {
        self.lock_handle()
            .release_interface(interface_number)
            .map_err(|error| {
                self.transport_failure(
                    "usb.release_interface",
                    format!("USB interface release failed: {error}"),
                )
            })
    }
}

impl UsbTransport for LinuxLibusbTransport {
    fn close(&mut self) -> BusResult<()> {
        let claimed_interfaces = {
            let mut claimed = self.lock_claimed_interfaces();
            std::mem::take(&mut *claimed)
        };

        for interface_number in claimed_interfaces.into_keys() {
            self.release_interface_on_handle(interface_number)?;
        }

        Ok(())
    }

    fn control_transfer(&mut self, transfer: &UsbControlTransfer) -> BusResult<Vec<u8>> {
        let request_type = rusb::request_type(
            to_rusb_direction(transfer.setup.direction),
            to_rusb_request_type(transfer.setup.request_type),
            to_rusb_recipient(transfer.setup.recipient),
        );
        let timeout = timeout_duration(transfer.timeout_ms, self.default_timeout);
        let handle = self.lock_handle();

        match transfer.setup.direction {
            UsbDirection::In => {
                let mut buffer = vec![0; transfer.data.len()];
                let bytes_read = handle
                    .read_control(
                        request_type,
                        transfer.setup.request,
                        transfer.setup.value,
                        transfer.setup.index,
                        &mut buffer,
                        timeout,
                    )
                    .map_err(|error| {
                        self.transport_failure(
                            "usb.control_transfer",
                            format!("USB control read failed: {error}"),
                        )
                    })?;
                buffer.truncate(bytes_read);
                Ok(buffer)
            }
            UsbDirection::Out => {
                handle
                    .write_control(
                        request_type,
                        transfer.setup.request,
                        transfer.setup.value,
                        transfer.setup.index,
                        &transfer.data,
                        timeout,
                    )
                    .map_err(|error| {
                        self.transport_failure(
                            "usb.control_transfer",
                            format!("USB control write failed: {error}"),
                        )
                    })?;
                Ok(Vec::new())
            }
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
        self.lock_handle()
            .read_bulk(
                endpoint,
                buffer,
                timeout_duration(timeout_ms, self.default_timeout),
            )
            .map_err(|error| {
                self.transport_failure("usb.bulk_read", format!("USB bulk read failed: {error}"))
            })
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
        self.lock_handle()
            .write_bulk(
                endpoint,
                bytes,
                timeout_duration(timeout_ms, self.default_timeout),
            )
            .map_err(|error| {
                self.transport_failure("usb.bulk_write", format!("USB bulk write failed: {error}"))
            })?;
        Ok(())
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
        self.lock_handle()
            .read_interrupt(
                endpoint,
                buffer,
                timeout_duration(timeout_ms, self.default_timeout),
            )
            .map_err(|error| {
                self.transport_failure(
                    "usb.interrupt_read",
                    format!("USB interrupt read failed: {error}"),
                )
            })
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
        self.lock_handle()
            .write_interrupt(
                transfer.endpoint,
                &transfer.bytes,
                timeout_duration(transfer.timeout_ms, self.default_timeout),
            )
            .map_err(|error| {
                self.transport_failure(
                    "usb.interrupt_write",
                    format!("USB interrupt write failed: {error}"),
                )
            })?;
        Ok(())
    }

    fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()> {
        self.claim_interface_on_handle(interface_number, alternate_setting)?;
        self.remember_claimed_interface(interface_number, alternate_setting);
        Ok(())
    }

    fn release_interface(&mut self, interface_number: u8) -> BusResult<()> {
        self.release_interface_on_handle(interface_number)?;
        self.forget_claimed_interface(interface_number);
        Ok(())
    }
}

pub(super) fn classify_open_error(
    device_id: &lemnos_core::DeviceId,
    bus: u16,
    ports: &[u8],
    error: RusbError,
) -> BusError {
    let target = format_usb_target(bus, ports);

    match error {
        RusbError::Access => BusError::PermissionDenied {
            device_id: device_id.clone(),
            operation: "open",
            reason: format!("failed to open Linux USB device on {target}: {error}"),
        },
        RusbError::Busy => BusError::AccessConflict {
            device_id: device_id.clone(),
            reason: format!("Linux USB device on {target} is already in use"),
        },
        RusbError::NoDevice | RusbError::NotFound => BusError::SessionUnavailable {
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

fn open_usb_handle(
    context: &Context,
    device_id: &lemnos_core::DeviceId,
    bus: u16,
    ports: &[u8],
) -> BusResult<DeviceHandle<Context>> {
    let devices = context
        .devices()
        .map_err(|error| BusError::TransportFailure {
            device_id: device_id.clone(),
            operation: "open",
            reason: format!("failed to enumerate USB devices: {error}"),
        })?;

    for device in devices.iter() {
        let device_bus = u16::from(device.bus_number());
        let device_ports = device
            .port_numbers()
            .map_err(|error| BusError::TransportFailure {
                device_id: device_id.clone(),
                operation: "open",
                reason: format!("failed to read USB port topology: {error}"),
            })?;
        if device_bus == bus && device_ports == ports {
            return device
                .open()
                .map_err(|error| classify_open_error(device_id, bus, ports, error));
        }
    }

    Err(BusError::SessionUnavailable {
        device_id: device_id.clone(),
        reason: format!(
            "no Linux USB device matched {}",
            format_usb_target(bus, ports)
        ),
    })
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

fn to_rusb_direction(direction: UsbDirection) -> Direction {
    match direction {
        UsbDirection::In => Direction::In,
        UsbDirection::Out => Direction::Out,
    }
}

fn to_rusb_request_type(request_type: UsbRequestType) -> RequestType {
    match request_type {
        UsbRequestType::Standard => RequestType::Standard,
        UsbRequestType::Class => RequestType::Class,
        UsbRequestType::Vendor => RequestType::Vendor,
        UsbRequestType::Reserved => RequestType::Reserved,
    }
}

fn to_rusb_recipient(recipient: UsbRecipient) -> Recipient {
    match recipient {
        UsbRecipient::Device => Recipient::Device,
        UsbRecipient::Interface => Recipient::Interface,
        UsbRecipient::Endpoint => Recipient::Endpoint,
        UsbRecipient::Other => Recipient::Other,
    }
}
