use super::smbus::SmbusTarget;
use super::{
    BusError, BusResult, DeviceDescriptor, I2cControllerTransport, I2cOperation, I2cTransport,
};
use super::{invalid_i2c_request, transport_i2c_failure};
use crate::hal::{I2cBus, I2cMessage};
use crate::metadata::descriptor_driver;
use std::io;

/// One target on an i2c-dev bus, claimed with `I2C_SLAVE`.
pub(super) struct Target {
    bus: I2cBus,
    address: u16,
}

impl Target {
    fn open(devnode: &str, address: u16) -> io::Result<Self> {
        let mut bus = I2cBus::open_path(devnode)?;
        bus.claim(address)?;
        Ok(Self { bus, address })
    }

    fn transfer(&mut self, messages: &mut [I2cMessage<'_>]) -> io::Result<()> {
        if !self.bus.supports_i2c() {
            return Err(io::Error::from_raw_os_error(95));
        }
        self.bus.transfer(self.address, messages)
    }

    fn smbus(&mut self) -> SmbusTarget<'_> {
        SmbusTarget {
            bus: &mut self.bus,
            address: self.address,
        }
    }
}

pub(super) struct LinuxKernelI2cTransport {
    device_id: lemnos_core::DeviceId,
    device: Target,
}

impl LinuxKernelI2cTransport {
    pub(super) fn new(device: &DeviceDescriptor, devnode: &str, address: u16) -> BusResult<Self> {
        let device_id = device.id.clone();
        let device = Target::open(devnode, address)
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
    device: Option<Target>,
}

impl LinuxKernelI2cControllerTransport {
    pub(super) fn new(owner: &DeviceDescriptor, devnode: String) -> Self {
        Self {
            owner_id: owner.id.clone(),
            devnode,
            device: None,
        }
    }

    fn ensure_address(&mut self, address: u16, operation: &'static str) -> BusResult<&mut Target> {
        if let Some(device) = self.device.as_mut() {
            if device.address != address {
                device.bus.claim(address).map_err(|error| {
                    classify_controller_address_error(
                        &self.owner_id,
                        &self.devnode,
                        address,
                        operation,
                        &error,
                    )
                })?;
                device.address = address;
            }
        } else {
            let device = Target::open(&self.devnode, address).map_err(|error| {
                classify_controller_address_error(
                    &self.owner_id,
                    &self.devnode,
                    address,
                    operation,
                    &error,
                )
            })?;
            self.device = Some(device);
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
    device: &mut Target,
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
    device: &mut Target,
    buffer: &mut [u8],
) -> BusResult<()> {
    if buffer.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.read",
            "read length must be greater than zero",
        ));
    }

    device
        .transfer(&mut [I2cMessage::Read(buffer)])
        .map_err(|error| {
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
    device: &mut Target,
    bytes: &[u8],
) -> BusResult<()> {
    if bytes.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.write",
            "write payload must not be empty",
        ));
    }

    device.transfer(&mut [I2cMessage::Write(bytes)]).or_else(|error| {
        if should_try_smbus_fallback(&error) {
            super::smbus::smbus_write_fallback(&mut device.smbus(), bytes).map_err(|fallback_error| {
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
    device: &mut Target,
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
    device: &mut Target,
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

    let result = device.transfer(&mut [I2cMessage::Write(write), I2cMessage::Read(read)]);
    result.or_else(|error| {
        if should_try_smbus_fallback(&error) {
            super::smbus::smbus_write_read_fallback(&mut device.smbus(), write, read.len() as u32)
                .and_then(|bytes| {
                    if bytes.len() != read.len() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "SMBus fallback returned {} bytes, expected {}",
                                bytes.len(),
                                read.len()
                            ),
                        ));
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
    device: &mut Target,
    operations: &[I2cOperation],
) -> BusResult<Vec<Vec<u8>>> {
    if operations.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.transaction",
            "transaction operations must not be empty",
        ));
    }

    // One combined transfer (repeated starts, one stop) when the adapter does
    // plain I2C; otherwise each operation on its own (SMBus fallbacks).
    if device.bus.supports_i2c() && operations.len() <= lemnos_linux_sys::i2c::MAX_MESSAGES {
        let mut results: Vec<Vec<u8>> = operations
            .iter()
            .map(|op| match op {
                I2cOperation::Read { length } => vec![0u8; *length as usize],
                I2cOperation::Write { .. } => Vec::new(),
            })
            .collect();
        if operations.iter().any(|op| match op {
            I2cOperation::Read { length } => *length == 0,
            I2cOperation::Write { bytes } => bytes.is_empty(),
        }) {
            return Err(invalid_i2c_request(
                device_id,
                "i2c.transaction",
                "transaction operations must not be empty",
            ));
        }
        {
            let mut messages: Vec<I2cMessage<'_>> = operations
                .iter()
                .zip(results.iter_mut())
                .map(|(op, out)| match op {
                    I2cOperation::Read { .. } => I2cMessage::Read(out.as_mut_slice()),
                    I2cOperation::Write { bytes } => I2cMessage::Write(bytes.as_slice()),
                })
                .collect();
            device.transfer(&mut messages).map_err(|error| {
                transport_i2c_failure(
                    device_id,
                    "i2c.transaction",
                    format!("Linux I2C combined transfer failed: {error}"),
                )
            })?;
        }
        return Ok(results);
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

fn should_try_smbus_fallback(error: &io::Error) -> bool {
    super::smbus::is_smbus_unsupported_kind_or_errno(error.kind(), error.raw_os_error())
}

pub(super) fn classify_open_error(
    device: &DeviceDescriptor,
    devnode: &str,
    address: u16,
    error: &io::Error,
) -> BusError {
    let (kind, raw_os_error) = (error.kind(), error.raw_os_error());
    let device_id = device.id.clone();
    let address_note = format!("Linux I2C address 0x{address:04x} on '{devnode}'");

    if kind == io::ErrorKind::PermissionDenied || matches!(raw_os_error, Some(1 | 13)) {
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

    if kind == io::ErrorKind::NotFound || matches!(raw_os_error, Some(6 | 19)) {
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
    error: &io::Error,
) -> BusError {
    let (kind, raw_os_error) = (error.kind(), error.raw_os_error());
    let address_note = format!("Linux I2C address 0x{address:04x} on '{devnode}'");

    if kind == io::ErrorKind::PermissionDenied || matches!(raw_os_error, Some(1 | 13)) {
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

    if kind == io::ErrorKind::NotFound || matches!(raw_os_error, Some(6 | 19)) {
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
