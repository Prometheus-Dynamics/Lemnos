use crate::DriverResult;
use crate::transport::transport_error;
use lemnos_bus::SpiSession;
use lemnos_core::{DeviceDescriptor, DeviceId, SpiConfiguration};

pub const TRANSFER_INTERACTION: &str = "spi.transfer";
pub const WRITE_INTERACTION: &str = "spi.write";
pub const CONFIGURE_INTERACTION: &str = "spi.configure";
pub const GET_CONFIGURATION_INTERACTION: &str = "spi.get_configuration";

pub struct SpiDeviceIo<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    session: &'a mut dyn SpiSession,
}

impl<'a> SpiDeviceIo<'a> {
    pub fn new(
        session: &'a mut dyn SpiSession,
        driver_id: &'a str,
        device: &DeviceDescriptor,
    ) -> Self {
        Self::with_device_id(session, driver_id, device.id.clone())
    }

    pub fn with_device_id(
        session: &'a mut dyn SpiSession,
        driver_id: &'a str,
        device_id: DeviceId,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            session,
        }
    }

    pub fn transfer(&mut self, write: &[u8]) -> DriverResult<Vec<u8>> {
        self.session
            .transfer(write)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn write(&mut self, bytes: &[u8]) -> DriverResult<()> {
        self.session
            .write(bytes)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configure(&mut self, configuration: &SpiConfiguration) -> DriverResult<()> {
        self.session
            .configure(configuration)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configuration(&self) -> DriverResult<SpiConfiguration> {
        self.session
            .configuration()
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }
}
