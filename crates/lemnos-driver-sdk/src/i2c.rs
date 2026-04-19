use crate::transport::transport_error;
use crate::{DriverError, DriverResult};
use lemnos_bus::{I2cControllerSession, I2cSession};
use lemnos_core::{DeviceDescriptor, DeviceId, I2cOperation};

pub const READ_INTERACTION: &str = "i2c.read";
pub const WRITE_INTERACTION: &str = "i2c.write";
pub const WRITE_READ_INTERACTION: &str = "i2c.write_read";
pub const TRANSACTION_INTERACTION: &str = "i2c.transaction";

pub struct I2cDeviceIo<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    session: &'a mut dyn I2cSession,
}

impl<'a> I2cDeviceIo<'a> {
    pub fn new(
        session: &'a mut dyn I2cSession,
        driver_id: &'a str,
        device: &DeviceDescriptor,
    ) -> Self {
        Self::with_device_id(session, driver_id, device.id.clone())
    }

    pub fn with_device_id(
        session: &'a mut dyn I2cSession,
        driver_id: &'a str,
        device_id: DeviceId,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            session,
        }
    }

    pub fn read_u8(&mut self, register: u8) -> DriverResult<u8> {
        let bytes = self.read_block(register, 1)?;
        Ok(bytes[0])
    }

    pub fn read(&mut self, length: u32) -> DriverResult<Vec<u8>> {
        self.session
            .read(length)
            .map_err(|source| self.transport_error(source))
    }

    pub fn read_into(&mut self, buffer: &mut [u8]) -> DriverResult<()> {
        self.session
            .read_into(buffer)
            .map_err(|source| self.transport_error(source))
    }

    pub fn read_block(&mut self, register: u8, length: u32) -> DriverResult<Vec<u8>> {
        let bytes = self
            .session
            .write_read(&[register], length)
            .map_err(|source| self.transport_error(source))?;
        self.expect_length(bytes, register, length as usize)
    }

    pub fn read_block_into(&mut self, register: u8, buffer: &mut [u8]) -> DriverResult<()> {
        self.session
            .write_read_into(&[register], buffer)
            .map_err(|source| self.transport_error(source))
    }

    pub fn read_exact_block<const N: usize>(&mut self, register: u8) -> DriverResult<[u8; N]> {
        let mut bytes = [0; N];
        self.read_block_into(register, &mut bytes)?;
        Ok(bytes)
    }

    pub fn read_u16_be(&mut self, register: u8) -> DriverResult<u16> {
        let bytes = self.read_exact_block::<2>(register)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_i16_be(&mut self, register: u8) -> DriverResult<i16> {
        let bytes = self.read_exact_block::<2>(register)?;
        Ok(i16::from_be_bytes(bytes))
    }

    pub fn write_u8(&mut self, register: u8, value: u8) -> DriverResult<()> {
        self.session
            .write(&[register, value])
            .map_err(|source| self.transport_error(source))
    }

    pub fn write(&mut self, bytes: &[u8]) -> DriverResult<()> {
        self.session
            .write(bytes)
            .map_err(|source| self.transport_error(source))
    }

    pub fn write_u16_be(&mut self, register: u8, value: u16) -> DriverResult<()> {
        let [msb, lsb] = value.to_be_bytes();
        self.session
            .write(&[register, msb, lsb])
            .map_err(|source| self.transport_error(source))
    }

    pub fn write_read(&mut self, write: &[u8], read_length: u32) -> DriverResult<Vec<u8>> {
        self.session
            .write_read(write, read_length)
            .map_err(|source| self.transport_error(source))
    }

    pub fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> DriverResult<()> {
        self.session
            .write_read_into(write, read)
            .map_err(|source| self.transport_error(source))
    }

    pub fn transaction(&mut self, operations: &[I2cOperation]) -> DriverResult<Vec<Vec<u8>>> {
        self.session
            .transaction(operations)
            .map_err(|source| self.transport_error(source))
    }

    fn transport_error(&self, source: lemnos_bus::BusError) -> DriverError {
        transport_error(self.driver_id, &self.device_id, source)
    }

    fn expect_length(
        &self,
        bytes: Vec<u8>,
        register: u8,
        expected: usize,
    ) -> DriverResult<Vec<u8>> {
        if bytes.len() == expected {
            return Ok(bytes);
        }

        Err(DriverError::InvariantViolation {
            driver_id: self.driver_id.to_string(),
            device_id: self.device_id.clone(),
            reason: format!(
                "expected {expected} bytes from register 0x{register:02x}, received {}",
                bytes.len()
            ),
        })
    }
}

pub struct I2cControllerIo<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    controller: &'a mut dyn I2cControllerSession,
}

impl<'a> I2cControllerIo<'a> {
    pub fn new(
        controller: &'a mut dyn I2cControllerSession,
        driver_id: &'a str,
        device: &DeviceDescriptor,
    ) -> Self {
        Self::with_device_id(controller, driver_id, device.id.clone())
    }

    pub fn with_device_id(
        controller: &'a mut dyn I2cControllerSession,
        driver_id: &'a str,
        device_id: DeviceId,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            controller,
        }
    }

    pub fn target<'s>(&'s mut self, address: u16) -> I2cControllerTarget<'s> {
        I2cControllerTarget {
            driver_id: self.driver_id,
            device_id: self.device_id.clone(),
            controller: self.controller,
            address,
        }
    }
}

pub struct I2cControllerTarget<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    controller: &'a mut dyn I2cControllerSession,
    address: u16,
}

impl<'a> I2cControllerTarget<'a> {
    pub fn read_u8(&mut self, register: u8) -> DriverResult<u8> {
        let bytes = self.read_block(register, 1)?;
        Ok(bytes[0])
    }

    pub fn read_block(&mut self, register: u8, length: u32) -> DriverResult<Vec<u8>> {
        let bytes = self
            .controller
            .write_read(self.address, &[register], length)
            .map_err(|source| self.transport_error(source))?;
        self.expect_length(bytes, register, length as usize)
    }

    pub fn read_block_into(&mut self, register: u8, buffer: &mut [u8]) -> DriverResult<()> {
        self.controller
            .write_read_into(self.address, &[register], buffer)
            .map_err(|source| self.transport_error(source))
    }

    pub fn read_exact_block<const N: usize>(&mut self, register: u8) -> DriverResult<[u8; N]> {
        let mut bytes = [0; N];
        self.read_block_into(register, &mut bytes)?;
        Ok(bytes)
    }

    pub fn read_u16_be(&mut self, register: u8) -> DriverResult<u16> {
        let bytes = self.read_exact_block::<2>(register)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_i16_be(&mut self, register: u8) -> DriverResult<i16> {
        let bytes = self.read_exact_block::<2>(register)?;
        Ok(i16::from_be_bytes(bytes))
    }

    pub fn write_u8(&mut self, register: u8, value: u8) -> DriverResult<()> {
        self.controller
            .write(self.address, &[register, value])
            .map_err(|source| self.transport_error(source))
    }

    pub fn write_u16_be(&mut self, register: u8, value: u16) -> DriverResult<()> {
        let [msb, lsb] = value.to_be_bytes();
        self.controller
            .write(self.address, &[register, msb, lsb])
            .map_err(|source| self.transport_error(source))
    }

    pub fn write_read(&mut self, write: &[u8], read_length: u32) -> DriverResult<Vec<u8>> {
        self.controller
            .write_read(self.address, write, read_length)
            .map_err(|source| self.transport_error(source))
    }

    pub fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> DriverResult<()> {
        self.controller
            .write_read_into(self.address, write, read)
            .map_err(|source| self.transport_error(source))
    }

    pub fn address(&self) -> u16 {
        self.address
    }

    fn transport_error(&self, source: lemnos_bus::BusError) -> DriverError {
        transport_error(self.driver_id, &self.device_id, source)
    }

    fn expect_length(
        &self,
        bytes: Vec<u8>,
        register: u8,
        expected: usize,
    ) -> DriverResult<Vec<u8>> {
        if bytes.len() == expected {
            return Ok(bytes);
        }

        Err(DriverError::InvariantViolation {
            driver_id: self.driver_id.to_string(),
            device_id: self.device_id.clone(),
            reason: format!(
                "expected {expected} bytes from register 0x{register:02x} at address 0x{:02x}, received {}",
                self.address,
                bytes.len()
            ),
        })
    }
}
