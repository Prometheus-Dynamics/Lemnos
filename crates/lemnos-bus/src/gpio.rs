use crate::{BusBackend, BusError, BusResult, BusSession, SessionAccess, StreamSession};
use lemnos_core::{DeviceDescriptor, GpioEdge, GpioLevel, GpioLineConfiguration, TimestampMs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioEdgeEvent {
    pub edge: GpioEdge,
    pub level: Option<GpioLevel>,
    pub sequence: u64,
    pub observed_at: Option<TimestampMs>,
}

pub trait GpioSession: BusSession {
    fn read_level(&mut self) -> BusResult<GpioLevel>;
    fn write_level(&mut self, level: GpioLevel) -> BusResult<()>;
    fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> BusResult<()>;
    fn configuration(&self) -> BusResult<GpioLineConfiguration>;
}

pub trait GpioEdgeStreamSession: GpioSession + StreamSession<Event = GpioEdgeEvent> {}
impl<T> GpioEdgeStreamSession for T where T: GpioSession + StreamSession<Event = GpioEdgeEvent> {}

pub trait GpioBusBackend: BusBackend {
    fn open_gpio(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn GpioSession>>;

    fn open_gpio_edge_stream(
        &self,
        device: &DeviceDescriptor,
        _access: SessionAccess,
    ) -> BusResult<Box<dyn GpioEdgeStreamSession>> {
        Err(BusError::UnsupportedDevice {
            backend: self.name().to_string(),
            device_id: device.id.clone(),
        })
    }
}
