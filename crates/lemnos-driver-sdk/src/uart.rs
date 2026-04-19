use crate::DriverResult;
use crate::transport::transport_error;
use lemnos_bus::UartSession;
use lemnos_core::{DeviceDescriptor, DeviceId, UartConfiguration};

pub const READ_INTERACTION: &str = "uart.read";
pub const WRITE_INTERACTION: &str = "uart.write";
pub const CONFIGURE_INTERACTION: &str = "uart.configure";
pub const FLUSH_INTERACTION: &str = "uart.flush";
pub const GET_CONFIGURATION_INTERACTION: &str = "uart.get_configuration";

pub struct UartDeviceIo<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    session: &'a mut dyn UartSession,
}

impl<'a> UartDeviceIo<'a> {
    pub fn new(
        session: &'a mut dyn UartSession,
        driver_id: &'a str,
        device: &DeviceDescriptor,
    ) -> Self {
        Self::with_device_id(session, driver_id, device.id.clone())
    }

    pub fn with_device_id(
        session: &'a mut dyn UartSession,
        driver_id: &'a str,
        device_id: DeviceId,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            session,
        }
    }

    pub fn read(&mut self, max_bytes: u32) -> DriverResult<Vec<u8>> {
        self.session
            .read(max_bytes)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn read_into(&mut self, buffer: &mut [u8]) -> DriverResult<usize> {
        self.session
            .read_into(buffer)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn write(&mut self, bytes: &[u8]) -> DriverResult<()> {
        self.session
            .write(bytes)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn flush(&mut self) -> DriverResult<()> {
        self.session
            .flush()
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configure(&mut self, configuration: &UartConfiguration) -> DriverResult<()> {
        self.session
            .configure(configuration)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configuration(&self) -> DriverResult<UartConfiguration> {
        self.session
            .configuration()
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }
}
