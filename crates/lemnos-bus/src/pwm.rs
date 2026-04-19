use crate::{BusBackend, BusResult, BusSession, SessionAccess};
use lemnos_core::{DeviceDescriptor, PwmConfiguration};

pub trait PwmSession: BusSession {
    fn set_enabled(&mut self, enabled: bool) -> BusResult<()>;
    fn set_period_ns(&mut self, period_ns: u64) -> BusResult<()>;
    fn set_duty_cycle_ns(&mut self, duty_cycle_ns: u64) -> BusResult<()>;
    fn configure(&mut self, configuration: &PwmConfiguration) -> BusResult<()>;
    fn configuration(&self) -> BusResult<PwmConfiguration>;
}

pub trait PwmBusBackend: BusBackend {
    fn open_pwm(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn PwmSession>>;
}
