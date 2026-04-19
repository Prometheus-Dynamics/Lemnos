use crate::{BusBackend, BusError, BusResult, BusSession, SessionAccess, StreamSession};
use lemnos_core::{DeviceDescriptor, TimestampMs, UsbControlTransfer, UsbInterruptTransfer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterruptPacket {
    pub endpoint: u8,
    pub bytes: Vec<u8>,
    pub sequence: u64,
    pub observed_at: Option<TimestampMs>,
}

pub trait UsbSession: BusSession {
    fn control_transfer(&mut self, transfer: &UsbControlTransfer) -> BusResult<Vec<u8>>;

    fn bulk_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        let bytes = self.bulk_read(endpoint, buffer.len() as u32, timeout_ms)?;
        let bytes_read = bytes.len().min(buffer.len());
        buffer[..bytes_read].copy_from_slice(&bytes[..bytes_read]);
        Ok(bytes_read)
    }

    fn bulk_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>>;
    fn bulk_write(&mut self, endpoint: u8, bytes: &[u8], timeout_ms: Option<u32>) -> BusResult<()>;

    fn interrupt_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        let bytes = self.interrupt_read(endpoint, buffer.len() as u32, timeout_ms)?;
        let bytes_read = bytes.len().min(buffer.len());
        buffer[..bytes_read].copy_from_slice(&bytes[..bytes_read]);
        Ok(bytes_read)
    }

    fn interrupt_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>>;
    fn interrupt_write(&mut self, transfer: &UsbInterruptTransfer) -> BusResult<()>;
    fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()>;
    fn release_interface(&mut self, interface_number: u8) -> BusResult<()>;
}

pub trait UsbInterruptStreamSession:
    UsbSession + StreamSession<Event = UsbInterruptPacket>
{
    fn endpoint(&self) -> u8;
}

pub trait UsbBusBackend: BusBackend {
    fn open_usb(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn UsbSession>>;

    fn open_usb_interrupt_stream(
        &self,
        device: &DeviceDescriptor,
        _endpoint: u8,
        _access: SessionAccess,
    ) -> BusResult<Box<dyn UsbInterruptStreamSession>> {
        Err(BusError::UnsupportedDevice {
            backend: self.name().to_string(),
            device_id: device.id.clone(),
        })
    }
}
