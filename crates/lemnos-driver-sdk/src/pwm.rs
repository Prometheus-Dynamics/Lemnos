use crate::DriverResult;
use crate::transport::transport_error;
use lemnos_bus::PwmSession;
use lemnos_core::{DeviceDescriptor, DeviceId, PwmConfiguration};

pub const ENABLE_INTERACTION: &str = "pwm.enable";
pub const CONFIGURE_INTERACTION: &str = "pwm.configure";
pub const SET_PERIOD_INTERACTION: &str = "pwm.set_period";
pub const SET_DUTY_CYCLE_INTERACTION: &str = "pwm.set_duty_cycle";
pub const GET_CONFIGURATION_INTERACTION: &str = "pwm.get_configuration";

pub struct PwmDeviceIo<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    session: &'a mut dyn PwmSession,
}

impl<'a> PwmDeviceIo<'a> {
    pub fn new(
        session: &'a mut dyn PwmSession,
        driver_id: &'a str,
        device: &DeviceDescriptor,
    ) -> Self {
        Self::with_device_id(session, driver_id, device.id.clone())
    }

    pub fn with_device_id(
        session: &'a mut dyn PwmSession,
        driver_id: &'a str,
        device_id: DeviceId,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            session,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) -> DriverResult<()> {
        self.session
            .set_enabled(enabled)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configure(&mut self, configuration: &PwmConfiguration) -> DriverResult<()> {
        self.session
            .configure(configuration)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn set_period_ns(&mut self, period_ns: u64) -> DriverResult<()> {
        self.session
            .set_period_ns(period_ns)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn set_duty_cycle_ns(&mut self, duty_cycle_ns: u64) -> DriverResult<()> {
        self.session
            .set_duty_cycle_ns(duty_cycle_ns)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn configuration(&self) -> DriverResult<PwmConfiguration> {
        self.session
            .configuration()
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }
}
