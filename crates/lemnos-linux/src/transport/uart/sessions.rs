use super::kernel::LinuxKernelUartTransport;
use super::{UartTransport, supports_descriptor};
use crate::LinuxPaths;
use crate::backend::{BACKEND_NAME, LinuxTransportConfig};
use crate::metadata::descriptor_devnode;
use crate::transport;
use crate::transport::session;
use lemnos_bus::{
    BusError, BusResult, BusSession, SessionAccess, SessionMetadata, SessionState, UartSession,
};
use lemnos_core::{DeviceDescriptor, InterfaceKind, UartConfiguration};

pub(crate) struct LinuxUartSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    transport: Box<dyn UartTransport>,
}

impl LinuxUartSession {
    pub(crate) fn open(
        paths: &LinuxPaths,
        transport_config: &LinuxTransportConfig,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Self> {
        if !supports_descriptor(device) {
            return Err(BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            });
        }

        let port_name =
            transport::uart_port_name(device).ok_or_else(|| BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            })?;
        let devnode = descriptor_devnode(device)
            .map(str::to_owned)
            .unwrap_or_else(|| paths.tty_devnode(&port_name).display().to_string());
        let transport =
            LinuxKernelUartTransport::new(device.id.clone(), &devnode, transport_config)?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport: Box::new(transport),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_transport(
        device: DeviceDescriptor,
        access: SessionAccess,
        transport: Box<dyn UartTransport>,
    ) -> Self {
        Self {
            device,
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport,
        }
    }

    fn ensure_open(&self, operation: &'static str) -> BusResult<()> {
        session::ensure_open(&self.metadata, &self.device.id, "UART", operation)
    }

    fn ensure_writable(&self, operation: &'static str, reason: &'static str) -> BusResult<()> {
        if self.metadata.access.can_write() {
            Ok(())
        } else {
            Err(session::permission_denied(
                &self.device.id,
                operation,
                reason,
            ))
        }
    }

    fn run_transport_call<T>(
        &mut self,
        operation: &'static str,
        call: impl FnOnce(&mut dyn UartTransport) -> BusResult<T>,
    ) -> BusResult<T> {
        self.ensure_open(operation)?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            call(transport.as_mut())
        })
    }
}

impl BusSession for LinuxUartSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Uart
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.metadata.mark_closed();
        Ok(())
    }
}

impl UartSession for LinuxUartSession {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<usize> {
        self.run_transport_call("uart.read", |transport| transport.read_into(buffer))
    }

    fn read(&mut self, max_bytes: u32) -> BusResult<Vec<u8>> {
        self.run_transport_call("uart.read", |transport| transport.read(max_bytes))
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        self.ensure_writable("uart.write", "session access is read-only")?;
        self.run_transport_call("uart.write", |transport| transport.write(bytes))
    }

    fn flush(&mut self) -> BusResult<()> {
        self.ensure_writable("uart.flush", "session access is read-only")?;
        self.run_transport_call("uart.flush", |transport| transport.flush())
    }

    fn configure(&mut self, configuration: &UartConfiguration) -> BusResult<()> {
        self.ensure_writable(
            "uart.configure",
            "session access does not allow configuration changes",
        )?;
        self.run_transport_call("uart.configure", |transport| {
            transport.configure(configuration)
        })
    }

    fn configuration(&self) -> BusResult<UartConfiguration> {
        self.ensure_open("uart.get_configuration")?;
        self.transport.configuration()
    }
}
