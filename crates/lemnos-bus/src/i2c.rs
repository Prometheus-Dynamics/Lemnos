use crate::{BusBackend, BusError, BusResult, BusSession, SessionAccess};
use lemnos_core::{DeviceDescriptor, I2cOperation};

pub trait I2cSession: BusSession {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<()> {
        let bytes = self.read(buffer.len() as u32)?;
        if bytes.len() != buffer.len() {
            return Err(BusError::TransportFailure {
                device_id: self.device().id.clone(),
                operation: "i2c.read",
                reason: format!(
                    "expected {} bytes from I2C read, received {}",
                    buffer.len(),
                    bytes.len()
                ),
            });
        }
        buffer.copy_from_slice(&bytes);
        Ok(())
    }

    fn read(&mut self, length: u32) -> BusResult<Vec<u8>>;
    fn write(&mut self, bytes: &[u8]) -> BusResult<()>;

    fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        let bytes = self.write_read(write, read.len() as u32)?;
        if bytes.len() != read.len() {
            return Err(BusError::TransportFailure {
                device_id: self.device().id.clone(),
                operation: "i2c.write_read",
                reason: format!(
                    "expected {} bytes from I2C write_read, received {}",
                    read.len(),
                    bytes.len()
                ),
            });
        }
        read.copy_from_slice(&bytes);
        Ok(())
    }

    fn write_read(&mut self, write: &[u8], read_length: u32) -> BusResult<Vec<u8>>;
    fn transaction(&mut self, operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>>;
}

pub trait I2cControllerSession: BusSession {
    fn bus(&self) -> u32;

    fn read_into(&mut self, address: u16, buffer: &mut [u8]) -> BusResult<()> {
        let bytes = self.read(address, buffer.len() as u32)?;
        if bytes.len() != buffer.len() {
            return Err(BusError::TransportFailure {
                device_id: self.device().id.clone(),
                operation: "i2c.read",
                reason: format!(
                    "expected {} bytes from I2C controller read, received {}",
                    buffer.len(),
                    bytes.len()
                ),
            });
        }
        buffer.copy_from_slice(&bytes);
        Ok(())
    }

    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>>;
    fn write(&mut self, address: u16, bytes: &[u8]) -> BusResult<()>;

    fn write_read_into(&mut self, address: u16, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        let bytes = self.write_read(address, write, read.len() as u32)?;
        if bytes.len() != read.len() {
            return Err(BusError::TransportFailure {
                device_id: self.device().id.clone(),
                operation: "i2c.write_read",
                reason: format!(
                    "expected {} bytes from I2C controller write_read, received {}",
                    read.len(),
                    bytes.len()
                ),
            });
        }
        read.copy_from_slice(&bytes);
        Ok(())
    }

    fn write_read(&mut self, address: u16, write: &[u8], read_length: u32) -> BusResult<Vec<u8>>;
    fn transaction(&mut self, address: u16, operations: &[I2cOperation])
    -> BusResult<Vec<Vec<u8>>>;
}

pub trait I2cBusBackend: BusBackend {
    fn open_i2c(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn I2cSession>>;

    fn open_i2c_controller(
        &self,
        owner: &DeviceDescriptor,
        _bus: u32,
        _access: SessionAccess,
    ) -> BusResult<Box<dyn I2cControllerSession>> {
        Err(BusError::UnsupportedDevice {
            backend: self.name().to_string(),
            device_id: owner.id.clone(),
        })
    }
}
