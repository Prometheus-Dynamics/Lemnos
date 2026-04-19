use crate::{BusBackend, BusResult, BusSession, SessionAccess};
use lemnos_core::{DeviceDescriptor, SpiConfiguration};

pub trait SpiSession: BusSession {
    fn transfer(&mut self, write: &[u8]) -> BusResult<Vec<u8>>;
    fn write(&mut self, bytes: &[u8]) -> BusResult<()>;
    fn configure(&mut self, configuration: &SpiConfiguration) -> BusResult<()>;
    fn configuration(&self) -> BusResult<SpiConfiguration>;
}

pub trait SpiBusBackend: BusBackend {
    fn open_spi(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn SpiSession>>;
}
