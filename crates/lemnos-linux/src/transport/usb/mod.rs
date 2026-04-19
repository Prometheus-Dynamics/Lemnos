use crate::LinuxPaths;
use crate::backend::BACKEND_NAME;
use crate::backend::LinuxTransportConfig;
use crate::transport;
use crate::transport::session;
use lemnos_bus::{
    BusError, BusResult, BusSession, SessionAccess, SessionMetadata, SessionState, UsbSession,
};
use lemnos_core::{
    DeviceDescriptor, DeviceKind, InterfaceKind, UsbControlTransfer, UsbDirection,
    UsbInterruptTransfer,
};

mod libusb_transport;

#[cfg(test)]
mod tests;

pub(crate) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    device.interface == InterfaceKind::Usb
        && matches!(
            device.kind,
            DeviceKind::UsbDevice | DeviceKind::UsbInterface
        )
        && resolve_target(device).is_some()
}

pub(crate) fn open_session(
    _paths: &LinuxPaths,
    transport_config: &LinuxTransportConfig,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn UsbSession>> {
    LinuxUsbSession::open(device, access, transport_config)
        .map(|session| Box::new(session) as Box<dyn UsbSession>)
}

trait UsbTransport: Send + Sync {
    fn close(&mut self) -> BusResult<()>;
    fn control_transfer(&mut self, transfer: &UsbControlTransfer) -> BusResult<Vec<u8>>;
    fn bulk_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize>;
    fn bulk_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>>;
    fn bulk_write(&mut self, endpoint: u8, bytes: &[u8], timeout_ms: Option<u32>) -> BusResult<()>;
    fn interrupt_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize>;
    fn interrupt_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>>;
    fn interrupt_write(&mut self, transfer: &UsbInterruptTransfer) -> BusResult<()>;
    fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()>;
    fn release_interface(&mut self, interface_number: u8) -> BusResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UsbTarget {
    pub(super) bus: u16,
    pub(super) ports: Vec<u8>,
}

pub(crate) struct LinuxUsbSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    transport: Box<dyn UsbTransport>,
}

impl LinuxUsbSession {
    fn open(
        device: &DeviceDescriptor,
        access: SessionAccess,
        transport_config: &LinuxTransportConfig,
    ) -> BusResult<Self> {
        if !supports_descriptor(device) {
            return Err(BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            });
        }

        let target = resolve_target(device).ok_or_else(|| BusError::UnsupportedDevice {
            backend: BACKEND_NAME.to_string(),
            device_id: device.id.clone(),
        })?;
        let transport = libusb_transport::LinuxLibusbTransport::new(
            device.id.clone(),
            target,
            transport_config,
        )?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport: Box::new(transport),
        })
    }

    #[cfg(test)]
    fn with_transport(
        device: DeviceDescriptor,
        access: SessionAccess,
        transport: Box<dyn UsbTransport>,
    ) -> Self {
        Self {
            device,
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport,
        }
    }
}

impl BusSession for LinuxUsbSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Usb
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.transport.close()?;
        self.metadata.mark_closed();
        Ok(())
    }
}

impl UsbSession for LinuxUsbSession {
    fn control_transfer(&mut self, transfer: &UsbControlTransfer) -> BusResult<Vec<u8>> {
        session::ensure_open(
            &self.metadata,
            &self.device.id,
            "USB",
            "usb.control_transfer",
        )?;
        if transfer.setup.direction == UsbDirection::Out && !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "usb.control_transfer",
                "session access is read-only for outbound control transfers",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.control_transfer(transfer)
        })
    }

    fn bulk_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        session::ensure_open(&self.metadata, &self.device.id, "USB", "usb.bulk_read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.bulk_read_into(endpoint, buffer, timeout_ms)
        })
    }

    fn bulk_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        session::ensure_open(&self.metadata, &self.device.id, "USB", "usb.bulk_read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.bulk_read(endpoint, length, timeout_ms)
        })
    }

    fn bulk_write(&mut self, endpoint: u8, bytes: &[u8], timeout_ms: Option<u32>) -> BusResult<()> {
        session::ensure_open(&self.metadata, &self.device.id, "USB", "usb.bulk_write")?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "usb.bulk_write",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.bulk_write(endpoint, bytes, timeout_ms)
        })
    }

    fn interrupt_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        session::ensure_open(&self.metadata, &self.device.id, "USB", "usb.interrupt_read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.interrupt_read_into(endpoint, buffer, timeout_ms)
        })
    }

    fn interrupt_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        session::ensure_open(&self.metadata, &self.device.id, "USB", "usb.interrupt_read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.interrupt_read(endpoint, length, timeout_ms)
        })
    }

    fn interrupt_write(&mut self, transfer: &UsbInterruptTransfer) -> BusResult<()> {
        session::ensure_open(
            &self.metadata,
            &self.device.id,
            "USB",
            "usb.interrupt_write",
        )?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "usb.interrupt_write",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.interrupt_write(transfer)
        })
    }

    fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()> {
        session::ensure_open(
            &self.metadata,
            &self.device.id,
            "USB",
            "usb.claim_interface",
        )?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "usb.claim_interface",
                "session access does not allow interface claims",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.claim_interface(interface_number, alternate_setting)
        })
    }

    fn release_interface(&mut self, interface_number: u8) -> BusResult<()> {
        session::ensure_open(
            &self.metadata,
            &self.device.id,
            "USB",
            "usb.release_interface",
        )?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "usb.release_interface",
                "session access does not allow interface release",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.release_interface(interface_number)
        })
    }
}

fn resolve_target(device: &DeviceDescriptor) -> Option<UsbTarget> {
    transport::usb_bus_ports(device).map(|(bus, ports)| UsbTarget { bus, ports })
}
