use i2cdev::core::I2CDevice;

pub(super) fn smbus_write_fallback<D>(device: &mut D, bytes: &[u8]) -> Result<(), D::Error>
where
    D: I2CDevice,
    D::Error: From<std::io::Error> + Into<std::io::Error>,
{
    if bytes.is_empty() {
        return Ok(());
    }

    match bytes {
        [] => Ok(()),
        [value] => device.smbus_write_byte(*value),
        [register, value] => device.smbus_write_byte_data(*register, *value),
        [register, values @ ..] if values.len() <= 32 => device
            .smbus_write_i2c_block_data(*register, values)
            .or_else(|error| {
                let io_error: std::io::Error = error.into();
                if is_smbus_unsupported_io_error(&io_error) {
                    smbus_write_byte_data_sequence(device, *register, values)
                } else {
                    Err(D::Error::from(io_error))
                }
            }),
        [register, values @ ..] => smbus_write_byte_data_sequence(device, *register, values),
    }
}

pub(super) fn smbus_write_read_fallback<D>(
    device: &mut D,
    write: &[u8],
    read_length: u32,
) -> Result<Vec<u8>, D::Error>
where
    D: I2CDevice,
    D::Error: From<std::io::Error>,
{
    let [register] = write else {
        return Err(D::Error::from(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "SMBus fallback requires a single register byte for write_read",
        )));
    };

    smbus_read_register_data_sequence(device, *register, read_length)
}

fn smbus_write_byte_data_sequence<D>(
    device: &mut D,
    register: u8,
    values: &[u8],
) -> Result<(), D::Error>
where
    D: I2CDevice,
{
    for (index, value) in values.iter().enumerate() {
        let register = register.wrapping_add(index as u8);
        device.smbus_write_byte_data(register, *value)?;
    }
    Ok(())
}

fn smbus_read_register_data_sequence<D>(
    device: &mut D,
    register: u8,
    read_length: u32,
) -> Result<Vec<u8>, D::Error>
where
    D: I2CDevice,
    D::Error: From<std::io::Error>,
{
    let read_length = usize::try_from(read_length).map_err(|_| {
        D::Error::from(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "requested read length does not fit into usize",
        ))
    })?;

    if read_length == 0 {
        return Ok(Vec::new());
    }

    if read_length <= 32
        && let Ok(bytes) = device.smbus_read_i2c_block_data(register, read_length as u8)
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

fn is_smbus_unsupported_io_error(error: &std::io::Error) -> bool {
    is_smbus_unsupported_kind_or_errno(error.kind(), error.raw_os_error())
}

pub(super) fn is_smbus_unsupported_kind_or_errno(
    kind: std::io::ErrorKind,
    raw_os_error: Option<i32>,
) -> bool {
    matches!(kind, std::io::ErrorKind::Unsupported)
        || matches!(raw_os_error, Some(22 | 25 | 38 | 95))
}
