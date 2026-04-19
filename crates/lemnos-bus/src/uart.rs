use crate::{BusBackend, BusError, BusResult, BusSession, SessionAccess, StreamSession};
use lemnos_core::{DeviceDescriptor, TimestampMs, UartConfiguration};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UartReadChunk {
    pub bytes: Vec<u8>,
    pub sequence: u64,
    pub observed_at: Option<TimestampMs>,
}

pub trait UartSession: BusSession {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<usize> {
        let bytes = self.read(buffer.len() as u32)?;
        let bytes_read = bytes.len().min(buffer.len());
        buffer[..bytes_read].copy_from_slice(&bytes[..bytes_read]);
        Ok(bytes_read)
    }

    fn read(&mut self, max_bytes: u32) -> BusResult<Vec<u8>>;
    fn write(&mut self, bytes: &[u8]) -> BusResult<()>;
    fn flush(&mut self) -> BusResult<()>;
    fn configure(&mut self, configuration: &UartConfiguration) -> BusResult<()>;
    fn configuration(&self) -> BusResult<UartConfiguration>;
}

pub trait UartStreamSession: UartSession + StreamSession<Event = UartReadChunk> {}
impl<T> UartStreamSession for T where T: UartSession + StreamSession<Event = UartReadChunk> {}

pub trait UartBusBackend: BusBackend {
    fn open_uart(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn UartSession>>;

    fn open_uart_stream(
        &self,
        device: &DeviceDescriptor,
        _access: SessionAccess,
    ) -> BusResult<Box<dyn UartStreamSession>> {
        Err(BusError::UnsupportedDevice {
            backend: self.name().to_string(),
            device_id: device.id.clone(),
        })
    }
}
