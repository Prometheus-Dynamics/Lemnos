use crate::LinuxPaths;
use crate::backend::LinuxTransportConfig;
use crate::transport;
use lemnos_bus::{BusResult, SessionAccess, UartSession};
use lemnos_core::{DeviceDescriptor, DeviceKind, InterfaceKind, UartConfiguration};

mod kernel;
mod sessions;

#[cfg(test)]
mod tests;

use sessions::LinuxUartSession;

pub(crate) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    device.interface == InterfaceKind::Uart
        && device.kind == DeviceKind::UartPort
        && transport::uart_port_name(device).is_some()
}

pub(crate) fn open_session(
    paths: &LinuxPaths,
    transport_config: &LinuxTransportConfig,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn UartSession>> {
    LinuxUartSession::open(paths, transport_config, device, access)
        .map(|session| Box::new(session) as Box<dyn UartSession>)
}

trait UartTransport: Send + Sync {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<usize>;
    fn read(&mut self, max_bytes: u32) -> BusResult<Vec<u8>>;
    fn write(&mut self, bytes: &[u8]) -> BusResult<()>;
    fn flush(&mut self) -> BusResult<()>;
    fn configure(&mut self, configuration: &UartConfiguration) -> BusResult<()>;
    fn configuration(&self) -> BusResult<UartConfiguration>;
}
