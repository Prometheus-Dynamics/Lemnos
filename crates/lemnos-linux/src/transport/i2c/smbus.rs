//! SMBus fallbacks for adapters that cannot do plain I2C transfers.

use crate::hal::I2cBus;
use std::io;

/// The SMBus calls the fallbacks need, on one target.
pub(super) trait SmbusDevice {
    fn smbus_write_byte(&mut self, value: u8) -> io::Result<()>;
    fn smbus_write_byte_data(&mut self, register: u8, value: u8) -> io::Result<()>;
    fn smbus_write_i2c_block_data(&mut self, register: u8, values: &[u8]) -> io::Result<()>;
    fn smbus_read_byte_data(&mut self, register: u8) -> io::Result<u8>;
    fn smbus_read_i2c_block_data(&mut self, register: u8, len: u8) -> io::Result<Vec<u8>>;
}

/// A target address on an i2c-dev bus.
pub(super) struct SmbusTarget<'a> {
    pub(super) bus: &'a mut I2cBus,
    pub(super) address: u16,
}

impl SmbusDevice for SmbusTarget<'_> {
    fn smbus_write_byte(&mut self, value: u8) -> io::Result<()> {
        self.bus.smbus_write_byte(self.address, value)
    }

    fn smbus_write_byte_data(&mut self, register: u8, value: u8) -> io::Result<()> {
        self.bus
            .smbus_write_byte_data(self.address, register, value)
    }

    fn smbus_write_i2c_block_data(&mut self, register: u8, values: &[u8]) -> io::Result<()> {
        self.bus
            .smbus_write_i2c_block_data(self.address, register, values)
    }

    fn smbus_read_byte_data(&mut self, register: u8) -> io::Result<u8> {
        self.bus.smbus_read_byte_data(self.address, register)
    }

    fn smbus_read_i2c_block_data(&mut self, register: u8, len: u8) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; usize::from(len)];
        let n = self
            .bus
            .smbus_read_i2c_block_data(self.address, register, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}

pub(super) fn smbus_write_fallback<D: SmbusDevice>(device: &mut D, bytes: &[u8]) -> io::Result<()> {
    match bytes {
        [] => Ok(()),
        [value] => device.smbus_write_byte(*value),
        [register, value] => device.smbus_write_byte_data(*register, *value),
        [register, values @ ..] if values.len() <= 32 => device
            .smbus_write_i2c_block_data(*register, values)
            .or_else(|error| {
                if is_smbus_unsupported_io_error(&error) {
                    smbus_write_byte_data_sequence(device, *register, values)
                } else {
                    Err(error)
                }
            }),
        [register, values @ ..] => smbus_write_byte_data_sequence(device, *register, values),
    }
}

pub(super) fn smbus_write_read_fallback<D: SmbusDevice>(
    device: &mut D,
    write: &[u8],
    read_length: u32,
) -> io::Result<Vec<u8>> {
    let [register] = write else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SMBus fallback requires a single register byte for write_read",
        ));
    };

    smbus_read_register_data_sequence(device, *register, read_length)
}

fn smbus_write_byte_data_sequence<D: SmbusDevice>(
    device: &mut D,
    register: u8,
    values: &[u8],
) -> io::Result<()> {
    for (index, value) in values.iter().enumerate() {
        let register = register.wrapping_add(index as u8);
        device.smbus_write_byte_data(register, *value)?;
    }
    Ok(())
}

fn smbus_read_register_data_sequence<D: SmbusDevice>(
    device: &mut D,
    register: u8,
    read_length: u32,
) -> io::Result<Vec<u8>> {
    let read_length = usize::try_from(read_length).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "requested read length does not fit into usize",
        )
    })?;

    if read_length == 0 {
        return Ok(Vec::new());
    }

    if read_length <= 32
        && let Ok(bytes) = device.smbus_read_i2c_block_data(register, read_length as u8)
        && bytes.len() == read_length
    {
        return Ok(bytes);
    }

    let mut bytes = Vec::with_capacity(read_length);
    for offset in 0..read_length {
        let register = register.wrapping_add(offset as u8);
        bytes.push(device.smbus_read_byte_data(register)?);
    }
    Ok(bytes)
}

fn is_smbus_unsupported_io_error(error: &io::Error) -> bool {
    is_smbus_unsupported_kind_or_errno(error.kind(), error.raw_os_error())
}

pub(super) fn is_smbus_unsupported_kind_or_errno(
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
) -> bool {
    matches!(kind, io::ErrorKind::Unsupported) || matches!(raw_os_error, Some(22 | 25 | 38 | 95))
}
