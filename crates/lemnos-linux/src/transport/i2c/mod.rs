use crate::LinuxPaths;
use crate::transport;
use lemnos_bus::{BusError, BusResult, I2cControllerSession, I2cSession, SessionAccess};
use lemnos_core::{DeviceDescriptor, DeviceKind, I2cOperation, InterfaceKind};

mod kernel;
mod sessions;
mod smbus;

#[cfg(test)]
mod tests;

use kernel::{LinuxKernelI2cControllerTransport, LinuxKernelI2cTransport};
use sessions::{LinuxI2cControllerSession, LinuxI2cSession};

pub(crate) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    device.interface == InterfaceKind::I2c
        && device.kind == DeviceKind::I2cDevice
        && transport::i2c_bus_address(device).is_some()
}

pub(crate) fn open_session(
    paths: &LinuxPaths,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn I2cSession>> {
    LinuxI2cSession::open(paths, device, access)
        .map(|session| Box::new(session) as Box<dyn I2cSession>)
}

pub(crate) fn open_controller(
    paths: &LinuxPaths,
    owner: &DeviceDescriptor,
    bus: u32,
    access: SessionAccess,
) -> BusResult<Box<dyn I2cControllerSession>> {
    LinuxI2cControllerSession::open(paths, owner, bus, access)
        .map(|session| Box::new(session) as Box<dyn I2cControllerSession>)
}

trait I2cTransport: Send + Sync {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<()>;
    fn read(&mut self, length: u32) -> BusResult<Vec<u8>>;
    fn write(&mut self, bytes: &[u8]) -> BusResult<()>;
    fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> BusResult<()>;
    fn write_read(&mut self, write: &[u8], read_length: u32) -> BusResult<Vec<u8>>;
    fn transaction(&mut self, operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>>;
}

trait I2cControllerTransport: Send + Sync {
    fn read_into(&mut self, address: u16, buffer: &mut [u8]) -> BusResult<()>;
    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>>;
    fn write(&mut self, address: u16, bytes: &[u8]) -> BusResult<()>;
    fn write_read_into(&mut self, address: u16, write: &[u8], read: &mut [u8]) -> BusResult<()>;
    fn write_read(&mut self, address: u16, write: &[u8], read_length: u32) -> BusResult<Vec<u8>>;
    fn transaction(&mut self, address: u16, operations: &[I2cOperation])
    -> BusResult<Vec<Vec<u8>>>;
}

fn invalid_i2c_request(
    device_id: &lemnos_core::DeviceId,
    operation: &'static str,
    reason: impl Into<String>,
) -> BusError {
    BusError::InvalidRequest {
        device_id: device_id.clone(),
        operation,
        reason: reason.into(),
    }
}

fn transport_i2c_failure(
    device_id: &lemnos_core::DeviceId,
    operation: &'static str,
    reason: impl Into<String>,
) -> BusError {
    BusError::TransportFailure {
        device_id: device_id.clone(),
        operation,
        reason: reason.into(),
    }
}

fn resolve_devnode(paths: &LinuxPaths, bus: u32) -> String {
    paths.i2c_devnode(bus).display().to_string()
}
