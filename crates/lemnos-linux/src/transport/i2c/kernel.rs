use super::{
    BusError, BusResult, DeviceDescriptor, I2cControllerTransport, I2cOperation, I2cTransport,
};
use super::{invalid_i2c_request, transport_i2c_failure};
use crate::metadata::descriptor_driver;
use i2cdev::core::{I2CDevice, I2CMessage, I2CTransfer};
use i2cdev::linux::{LinuxI2CDevice, LinuxI2CError, LinuxI2CMessage};

pub(super) struct LinuxKernelI2cTransport {
    device_id: lemnos_core::DeviceId,
    device: LinuxI2CDevice,
}

impl LinuxKernelI2cTransport {
    pub(super) fn new(device: &DeviceDescriptor, devnode: &str, address: u16) -> BusResult<Self> {
        let device_id = device.id.clone();
        let device = LinuxI2CDevice::new(devnode, address)
            .map_err(|error| classify_open_error(device, devnode, address, &error))?;

        Ok(Self { device_id, device })
    }
}

impl I2cTransport for LinuxKernelI2cTransport {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<()> {
        linux_i2c_read_into(&self.device_id, &mut self.device, buffer)
    }

    fn read(&mut self, length: u32) -> BusResult<Vec<u8>> {
        linux_i2c_read(&self.device_id, &mut self.device, length)
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        linux_i2c_write(&self.device_id, &mut self.device, bytes)
    }

    fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        linux_i2c_write_read_into(&self.device_id, &mut self.device, write, read)
    }

    fn write_read(&mut self, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        linux_i2c_write_read(&self.device_id, &mut self.device, write, read_length)
    }

    fn transaction(&mut self, operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>> {
        linux_i2c_transaction(&self.device_id, &mut self.device, operations)
    }
}

pub(super) struct LinuxKernelI2cControllerTransport {
    owner_id: lemnos_core::DeviceId,
    devnode: String,
    device: Option<LinuxI2CDevice>,
    current_address: Option<u16>,
}

impl LinuxKernelI2cControllerTransport {
    pub(super) fn new(owner: &DeviceDescriptor, devnode: String) -> Self {
        Self {
            owner_id: owner.id.clone(),
            devnode,
            device: None,
            current_address: None,
        }
    }

    fn ensure_address(
        &mut self,
        address: u16,
        operation: &'static str,
    ) -> BusResult<&mut LinuxI2CDevice> {
        if let Some(device) = self.device.as_mut() {
            if self.current_address != Some(address) {
                device.set_slave_address(address).map_err(|error| {
                    classify_controller_address_error(
                        &self.owner_id,
                        &self.devnode,
                        address,
                        operation,
                        &error,
                    )
                })?;
                self.current_address = Some(address);
            }
        } else {
            let device = LinuxI2CDevice::new(&self.devnode, address).map_err(|error| {
                classify_controller_address_error(
                    &self.owner_id,
                    &self.devnode,
                    address,
                    operation,
                    &error,
                )
            })?;
            self.device = Some(device);
            self.current_address = Some(address);
        }

        self.device
            .as_mut()
            .ok_or_else(|| BusError::SessionUnavailable {
                device_id: self.owner_id.clone(),
                reason: format!(
                    "controller device for address 0x{address:02x} was not opened before {operation}"
                ),
            })
    }
}

impl I2cControllerTransport for LinuxKernelI2cControllerTransport {
    fn read_into(&mut self, address: u16, buffer: &mut [u8]) -> BusResult<()> {
        let owner_id = self.owner_id.clone();
        let device = self.ensure_address(address, "i2c.read")?;
        linux_i2c_read_into(&owner_id, device, buffer)
    }

    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>> {
        let owner_id = self.owner_id.clone();
        let device = self.ensure_address(address, "i2c.read")?;
        linux_i2c_read(&owner_id, device, length)
    }

    fn write(&mut self, address: u16, bytes: &[u8]) -> BusResult<()> {
        let owner_id = self.owner_id.clone();
        let device = self.ensure_address(address, "i2c.write")?;
        linux_i2c_write(&owner_id, device, bytes)
    }

    fn write_read_into(&mut self, address: u16, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        let owner_id = self.owner_id.clone();
        let device = self.ensure_address(address, "i2c.write_read")?;
        linux_i2c_write_read_into(&owner_id, device, write, read)
    }

    fn write_read(&mut self, address: u16, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        let owner_id = self.owner_id.clone();
        let device = self.ensure_address(address, "i2c.write_read")?;
        linux_i2c_write_read(&owner_id, device, write, read_length)
    }

    fn transaction(
        &mut self,
        address: u16,
        operations: &[I2cOperation],
    ) -> BusResult<Vec<Vec<u8>>> {
        let owner_id = self.owner_id.clone();
        let device = self.ensure_address(address, "i2c.transaction")?;
        linux_i2c_transaction(&owner_id, device, operations)
    }
}

pub(super) fn linux_i2c_read(
    device_id: &lemnos_core::DeviceId,
    device: &mut LinuxI2CDevice,
    length: u32,
) -> BusResult<Vec<u8>> {
    if length == 0 {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.read",
            "read length must be greater than zero",
        ));
    }

    let mut buffer = vec![0; length as usize];
    linux_i2c_read_into(device_id, device, &mut buffer)?;
    Ok(buffer)
}

pub(super) fn linux_i2c_read_into(
    device_id: &lemnos_core::DeviceId,
    device: &mut LinuxI2CDevice,
    buffer: &mut [u8],
) -> BusResult<()> {
    if buffer.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.read",
            "read length must be greater than zero",
        ));
    }

    device.read(buffer).map_err(|error| {
        transport_i2c_failure(
            device_id,
            "i2c.read",
            format!("Linux I2C read failed: {error}"),
        )
    })?;
    Ok(())
}

pub(super) fn linux_i2c_write(
    device_id: &lemnos_core::DeviceId,
    device: &mut LinuxI2CDevice,
    bytes: &[u8],
) -> BusResult<()> {
    if bytes.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.write",
            "write payload must not be empty",
        ));
    }

    device.write(bytes).or_else(|error| {
        if should_try_smbus_fallback(&error) {
            super::smbus::smbus_write_fallback(device, bytes).map_err(|fallback_error| {
                transport_i2c_failure(
                    device_id,
                    "i2c.write",
                    format!(
                        "Linux I2C write failed: {error}; SMBus fallback also failed: {fallback_error}"
                    ),
                )
            })
        } else {
            Err(transport_i2c_failure(
                device_id,
                "i2c.write",
                format!("Linux I2C write failed: {error}"),
            ))
        }
    })
}

pub(super) fn linux_i2c_write_read(
    device_id: &lemnos_core::DeviceId,
    device: &mut LinuxI2CDevice,
    write: &[u8],
    read_length: u32,
) -> BusResult<Vec<u8>> {
    if write.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.write_read",
            "write buffer must not be empty",
        ));
    }
    if read_length == 0 {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.write_read",
            "read length must be greater than zero",
        ));
    }

    let mut buffer = vec![0; read_length as usize];
    linux_i2c_write_read_into(device_id, device, write, &mut buffer)?;
    Ok(buffer)
}

pub(super) fn linux_i2c_write_read_into(
    device_id: &lemnos_core::DeviceId,
    device: &mut LinuxI2CDevice,
    write: &[u8],
    read: &mut [u8],
) -> BusResult<()> {
    if write.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.write_read",
            "write buffer must not be empty",
        ));
    }
    if read.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.write_read",
            "read length must be greater than zero",
        ));
    }

    let mut messages = [LinuxI2CMessage::write(write), LinuxI2CMessage::read(read)];
    device.transfer(&mut messages).map(|_| ()).or_else(|error| {
        if should_try_smbus_fallback(&error) {
            super::smbus::smbus_write_read_fallback(device, write, read.len() as u32)
                .and_then(|bytes| {
                    if bytes.len() != read.len() {
                        return Err(LinuxI2CError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "SMBus fallback returned {} bytes, expected {}",
                                bytes.len(),
                                read.len()
                            ),
                        )));
                    }
                    read.copy_from_slice(&bytes);
                    Ok(())
                })
                .map_err(|fallback_error| {
                    transport_i2c_failure(
                        device_id,
                        "i2c.write_read",
                        format!(
                            "Linux I2C write_read transfer failed: {error}; SMBus fallback also failed: {fallback_error}"
                        ),
                    )
                })
        } else {
            Err(transport_i2c_failure(
                device_id,
                "i2c.write_read",
                format!("Linux I2C write_read transfer failed: {error}"),
            ))
        }
    })
}

pub(super) fn linux_i2c_transaction(
    device_id: &lemnos_core::DeviceId,
    device: &mut LinuxI2CDevice,
    operations: &[I2cOperation],
) -> BusResult<Vec<Vec<u8>>> {
    if operations.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.transaction",
            "transaction operations must not be empty",
        ));
    }

    let mut results = Vec::with_capacity(operations.len());
    for operation in operations {
        match operation {
            I2cOperation::Read { length } => {
                results.push(linux_i2c_read(device_id, device, *length)?)
            }
            I2cOperation::Write { bytes } => {
                linux_i2c_write(device_id, device, bytes)?;
                results.push(Vec::new());
            }
        }
    }
    Ok(results)
}

fn should_try_smbus_fallback(error: &LinuxI2CError) -> bool {
    let (kind, raw_os_error) = linux_i2c_error_kind_and_errno(error);
    super::smbus::is_smbus_unsupported_kind_or_errno(kind, raw_os_error)
}

pub(super) fn classify_open_error(
    device: &DeviceDescriptor,
    devnode: &str,
    address: u16,
    error: &LinuxI2CError,
) -> BusError {
    let (kind, raw_os_error) = linux_i2c_error_kind_and_errno(error);
    let device_id = device.id.clone();
    let address_note = format!("Linux I2C address 0x{address:04x} on '{devnode}'");

    if kind == std::io::ErrorKind::PermissionDenied || matches!(raw_os_error, Some(1 | 13)) {
        return BusError::PermissionDenied {
            device_id,
            operation: "open",
            reason: format!("failed to open {address_note}: {error}"),
        };
    }

    if matches!(raw_os_error, Some(16)) {
        let reason = if let Some(driver) = descriptor_driver(device) {
            format!("{address_note} is already claimed by kernel driver '{driver}'")
        } else {
            format!("{address_note} is already in use by another kernel or userspace client")
        };
        return BusError::AccessConflict { device_id, reason };
    }

    if kind == std::io::ErrorKind::NotFound || matches!(raw_os_error, Some(6 | 19)) {
        return BusError::SessionUnavailable {
            device_id,
            reason: format!("{address_note} is not currently available: {error}"),
        };
    }

    BusError::TransportFailure {
        device_id,
        operation: "open",
        reason: format!("failed to open Linux I2C device '{devnode}': {error}"),
    }
}

fn classify_controller_address_error(
    owner_id: &lemnos_core::DeviceId,
    devnode: &str,
    address: u16,
    operation: &'static str,
    error: &LinuxI2CError,
) -> BusError {
    let (kind, raw_os_error) = linux_i2c_error_kind_and_errno(error);
    let address_note = format!("Linux I2C address 0x{address:04x} on '{devnode}'");

    if kind == std::io::ErrorKind::PermissionDenied || matches!(raw_os_error, Some(1 | 13)) {
        return BusError::PermissionDenied {
            device_id: owner_id.clone(),
            operation,
            reason: format!("failed to select {address_note}: {error}"),
        };
    }

    if matches!(raw_os_error, Some(16)) {
        return BusError::AccessConflict {
            device_id: owner_id.clone(),
            reason: format!(
                "{address_note} is already in use by another kernel or userspace client"
            ),
        };
    }

    if kind == std::io::ErrorKind::NotFound || matches!(raw_os_error, Some(6 | 19)) {
        return BusError::SessionUnavailable {
            device_id: owner_id.clone(),
            reason: format!("{address_note} is not currently available: {error}"),
        };
    }

    BusError::TransportFailure {
        device_id: owner_id.clone(),
        operation,
        reason: format!("failed to select {address_note}: {error}"),
    }
}

fn linux_i2c_error_kind_and_errno(error: &LinuxI2CError) -> (std::io::ErrorKind, Option<i32>) {
    match error {
        LinuxI2CError::Errno(errno) => {
            let io_error = std::io::Error::from_raw_os_error(*errno);
            (io_error.kind(), Some(*errno))
        }
        LinuxI2CError::Io(io_error) => (io_error.kind(), io_error.raw_os_error()),
    }
}
