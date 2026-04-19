use crate::DriverResult;
use crate::transport::transport_error;
use lemnos_bus::GpioSession;
use lemnos_core::{DeviceDescriptor, DeviceId, GpioLevel, GpioLineConfiguration};

pub const READ_INTERACTION: &str = "gpio.read";
pub const WRITE_INTERACTION: &str = "gpio.write";
pub const CONFIGURE_INTERACTION: &str = "gpio.configure";
pub const GET_CONFIGURATION_INTERACTION: &str = "gpio.get_configuration";

pub struct GpioDeviceIo<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    session: &'a mut dyn GpioSession,
}

impl<'a> GpioDeviceIo<'a> {
    pub fn new(
        session: &'a mut dyn GpioSession,
        driver_id: &'a str,
        device: &DeviceDescriptor,
    ) -> Self {
        Self::with_device_id(session, driver_id, device.id.clone())
    }

    pub fn with_device_id(
        session: &'a mut dyn GpioSession,
        driver_id: &'a str,
        device_id: DeviceId,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            session,
        }
    }

    pub fn read_level(&mut self) -> DriverResult<GpioLevel> {
        self.session
            .read_level()
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn write_level(&mut self, level: GpioLevel) -> DriverResult<()> {
        self.session
            .write_level(level)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> DriverResult<()> {
        self.session
            .configure_line(configuration)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configuration(&self) -> DriverResult<GpioLineConfiguration> {
        self.session
            .configuration()
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }
}
