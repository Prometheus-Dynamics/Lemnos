use super::*;
use crate::metadata::descriptor_devnode;
use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CError;
use i2cdev::mock::MockI2CDevice;
use lemnos_bus::{BusSession, I2cControllerSession, SessionAccess};
use lemnos_core::{DeviceDescriptor, I2cOperation};
use std::collections::BTreeMap;
use std::io;

struct MockTransport {
    device_id: lemnos_core::DeviceId,
    inner: MockI2CDevice,
}

impl MockTransport {
    fn new(device_id: lemnos_core::DeviceId) -> Self {
        let mut inner = MockI2CDevice::new();
        inner.regmap.write_regs(0x10, &[0xAA, 0xBB, 0xCC]);
        Self { device_id, inner }
    }

    fn invalid_request(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::InvalidRequest {
            device_id: self.device_id.clone(),
            operation,
            reason: reason.into(),
        }
    }
}

impl I2cTransport for MockTransport {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<()> {
        if buffer.is_empty() {
            return Err(self.invalid_request("i2c.read", "read length must be greater than zero"));
        }

        self.inner
            .read(buffer)
            .map_err(|error| BusError::TransportFailure {
                device_id: self.device_id.clone(),
                operation: "i2c.read",
                reason: error.to_string(),
            })
    }

    fn read(&mut self, length: u32) -> BusResult<Vec<u8>> {
        if length == 0 {
            return Err(self.invalid_request("i2c.read", "read length must be greater than zero"));
        }

        let mut buffer = vec![0; length as usize];
        self.inner
            .read(&mut buffer)
            .map_err(|error| BusError::TransportFailure {
                device_id: self.device_id.clone(),
                operation: "i2c.read",
                reason: error.to_string(),
            })?;
        Ok(buffer)
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        if bytes.is_empty() {
            return Err(self.invalid_request("i2c.write", "write payload must not be empty"));
        }

        self.inner
            .write(bytes)
            .map_err(|error| BusError::TransportFailure {
                device_id: self.device_id.clone(),
                operation: "i2c.write",
                reason: error.to_string(),
            })
    }

    fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        self.write(write)?;
        self.read_into(read)
    }

    fn write_read(&mut self, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        self.write(write)?;
        self.read(read_length)
    }

    fn transaction(&mut self, operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>> {
        let mut results = Vec::with_capacity(operations.len());
        for operation in operations {
            match operation {
                I2cOperation::Read { length } => results.push(self.read(*length)?),
                I2cOperation::Write { bytes } => {
                    self.write(bytes)?;
                    results.push(Vec::new());
                }
            }
        }
        Ok(results)
    }
}

struct MockControllerTransport {
    device_id: lemnos_core::DeviceId,
    memory_by_address: BTreeMap<u16, Vec<u8>>,
    pointers: BTreeMap<u16, usize>,
}

impl MockControllerTransport {
    fn new(device_id: lemnos_core::DeviceId) -> Self {
        let mut memory_by_address = BTreeMap::new();
        memory_by_address.insert(0x18, vec![0; 256]);
        memory_by_address.insert(0x68, vec![0; 256]);
        Self {
            device_id,
            memory_by_address,
            pointers: BTreeMap::new(),
        }
    }

    fn with_bytes(mut self, address: u16, offset: u8, bytes: impl AsRef<[u8]>) -> Self {
        let offset = offset as usize;
        let bytes = bytes.as_ref();
        let memory = self
            .memory_by_address
            .entry(address)
            .or_insert_with(|| vec![0; 256]);
        let required_len = offset + bytes.len();
        if memory.len() < required_len {
            memory.resize(required_len, 0);
        }
        memory[offset..required_len].copy_from_slice(bytes);
        self
    }

    fn invalid_request(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::InvalidRequest {
            device_id: self.device_id.clone(),
            operation,
            reason: reason.into(),
        }
    }
}

impl I2cControllerTransport for MockControllerTransport {
    fn read_into(&mut self, address: u16, buffer: &mut [u8]) -> BusResult<()> {
        let bytes = self.read(address, buffer.len() as u32)?;
        buffer.copy_from_slice(&bytes);
        Ok(())
    }

    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>> {
        if length == 0 {
            return Err(self.invalid_request("i2c.read", "read length must be greater than zero"));
        }
        let pointer = self.pointers.get(&address).copied().unwrap_or(0);
        let memory =
            self.memory_by_address
                .get(&address)
                .ok_or_else(|| BusError::SessionUnavailable {
                    device_id: self.device_id.clone(),
                    reason: format!("address 0x{address:02x} is not available"),
                })?;
        let bytes = (0..length as usize)
            .map(|index| memory.get(pointer + index).copied().unwrap_or(0))
            .collect::<Vec<_>>();
        self.pointers.insert(address, pointer + length as usize);
        Ok(bytes)
    }

    fn write(&mut self, address: u16, bytes: &[u8]) -> BusResult<()> {
        if bytes.is_empty() {
            return Err(self.invalid_request("i2c.write", "write payload must not be empty"));
        }
        let memory = self.memory_by_address.get_mut(&address).ok_or_else(|| {
            BusError::SessionUnavailable {
                device_id: self.device_id.clone(),
                reason: format!("address 0x{address:02x} is not available"),
            }
        })?;
        let offset = bytes[0] as usize;
        self.pointers.insert(address, offset);
        if bytes.len() > 1 {
            let required_len = offset + bytes.len() - 1;
            if memory.len() < required_len {
                memory.resize(required_len, 0);
            }
            memory[offset..required_len].copy_from_slice(&bytes[1..]);
            self.pointers.insert(address, required_len);
        }
        Ok(())
    }

    fn write_read_into(&mut self, address: u16, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        let bytes = self.write_read(address, write, read.len() as u32)?;
        read.copy_from_slice(&bytes);
        Ok(())
    }

    fn write_read(&mut self, address: u16, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        if read_length == 0 {
            return Err(
                self.invalid_request("i2c.write_read", "read length must be greater than zero")
            );
        }
        if write.len() == 1 {
            self.pointers.insert(address, write[0] as usize);
        } else if !write.is_empty() {
            self.write(address, write)?;
            self.pointers.insert(address, write[0] as usize);
        }
        self.read(address, read_length)
    }

    fn transaction(
        &mut self,
        address: u16,
        operations: &[I2cOperation],
    ) -> BusResult<Vec<Vec<u8>>> {
        let mut results = Vec::with_capacity(operations.len());
        for operation in operations {
            match operation {
                I2cOperation::Read { length } => results.push(self.read(address, *length)?),
                I2cOperation::Write { bytes } => {
                    self.write(address, bytes)?;
                    results.push(Vec::new());
                }
            }
        }
        Ok(results)
    }
}

fn test_device() -> DeviceDescriptor {
    DeviceDescriptor::builder_for_kind("linux.i2c.bus1.0x0050", lemnos_core::DeviceKind::I2cDevice)
        .expect("builder")
        .address(lemnos_core::DeviceAddress::I2cDevice {
            bus: 1,
            address: 0x50,
        })
        .property("devnode", "/dev/i2c-1")
        .build()
        .expect("descriptor")
}

#[test]
fn i2c_supports_descriptor_requires_typed_address() {
    let property_only =
        DeviceDescriptor::builder("linux.i2c.bus1.0x0050", lemnos_core::InterfaceKind::I2c)
            .expect("builder")
            .kind(lemnos_core::DeviceKind::I2cDevice)
            .property("devnode", "/dev/i2c-1")
            .build()
            .expect("descriptor");
    assert!(!super::supports_descriptor(&property_only));
}

struct SmbusFallbackDevice {
    registers: [u8; 256],
    pointer: u8,
    supports_block_read: bool,
    supports_block_write: bool,
}

impl SmbusFallbackDevice {
    fn new() -> Self {
        Self {
            registers: [0; 256],
            pointer: 0,
            supports_block_read: false,
            supports_block_write: false,
        }
    }

    fn with_block_read_support(mut self) -> Self {
        self.supports_block_read = true;
        self
    }

    fn with_block_write_support(mut self) -> Self {
        self.supports_block_write = true;
        self
    }
}

impl I2CDevice for SmbusFallbackDevice {
    type Error = io::Error;

    fn read(&mut self, data: &mut [u8]) -> io::Result<()> {
        for byte in data.iter_mut() {
            *byte = self.registers[self.pointer as usize];
            self.pointer = self.pointer.wrapping_add(1);
        }
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if let Some((register, values)) = data.split_first() {
            self.pointer = *register;
            for (offset, value) in values.iter().enumerate() {
                self.registers[register.wrapping_add(offset as u8) as usize] = *value;
            }
            self.pointer = register.wrapping_add(values.len() as u8);
        }
        Ok(())
    }

    fn smbus_write_quick(&mut self, _bit: bool) -> io::Result<()> {
        Ok(())
    }

    fn smbus_read_byte(&mut self) -> io::Result<u8> {
        let value = self.registers[self.pointer as usize];
        self.pointer = self.pointer.wrapping_add(1);
        Ok(value)
    }

    fn smbus_write_byte(&mut self, value: u8) -> io::Result<()> {
        self.pointer = value;
        Ok(())
    }

    fn smbus_read_byte_data(&mut self, register: u8) -> io::Result<u8> {
        Ok(self.registers[register as usize])
    }

    fn smbus_write_byte_data(&mut self, register: u8, value: u8) -> io::Result<()> {
        self.registers[register as usize] = value;
        Ok(())
    }

    fn smbus_read_block_data(&mut self, _register: u8) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SMBus block read not implemented in test mock",
        ))
    }

    fn smbus_read_i2c_block_data(&mut self, register: u8, len: u8) -> io::Result<Vec<u8>> {
        if !self.supports_block_read {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "I2C block read is unsupported",
            ));
        }

        let start = register as usize;
        let end = start + len as usize;
        Ok(self.registers[start..end].to_vec())
    }

    fn smbus_write_block_data(&mut self, _register: u8, _values: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SMBus block write not implemented in test mock",
        ))
    }

    fn smbus_write_i2c_block_data(&mut self, register: u8, values: &[u8]) -> io::Result<()> {
        if !self.supports_block_write {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "I2C block write is unsupported",
            ));
        }

        let start = register as usize;
        let end = start + values.len();
        self.registers[start..end].copy_from_slice(values);
        Ok(())
    }

    fn smbus_process_block(&mut self, _register: u8, _values: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SMBus process block is unsupported",
        ))
    }
}

#[test]
fn i2c_session_round_trips_through_transport() {
    let device = test_device();
    let mut session = LinuxI2cSession::with_transport(
        device.clone(),
        SessionAccess::Exclusive,
        Box::new(MockTransport::new(device.id.clone())),
    );

    let bytes = session.write_read(&[0x10], 2).expect("write_read");
    assert_eq!(bytes, vec![0xAA, 0xBB]);

    session.write(&[0x12, 0x11, 0x22]).expect("write");
    let transaction = session
        .transaction(&[
            I2cOperation::Write { bytes: vec![0x10] },
            I2cOperation::Read { length: 3 },
        ])
        .expect("transaction");

    assert_eq!(transaction.len(), 2);
    assert_eq!(transaction[1], vec![0xAA, 0xBB, 0x11]);
    assert_eq!(descriptor_devnode(session.device()), Some("/dev/i2c-1"));
}

#[test]
fn i2c_controller_session_can_talk_to_multiple_addresses_on_one_bus() {
    let owner = DeviceDescriptor::new("linux.i2c.controller4", InterfaceKind::I2c)
        .expect("owner descriptor");
    let transport = MockControllerTransport::new(owner.id.clone())
        .with_bytes(0x18, 0x00, [0x1E])
        .with_bytes(0x68, 0x00, [0x0F]);
    let mut session = LinuxI2cControllerSession::with_transport(
        owner,
        4,
        SessionAccess::ExclusiveController,
        Box::new(transport),
    );

    let accel = session
        .write_read(0x18, &[0x00], 1)
        .expect("accel chip id read");
    let gyro = session
        .write_read(0x68, &[0x00], 1)
        .expect("gyro chip id read");

    assert_eq!(session.bus(), 4);
    assert_eq!(accel, vec![0x1E]);
    assert_eq!(gyro, vec![0x0F]);
}

#[test]
fn i2c_sessions_reject_operations_after_close_and_fresh_sessions_can_reopen() {
    let device = test_device();
    let mut session = LinuxI2cSession::with_transport(
        device.clone(),
        SessionAccess::Exclusive,
        Box::new(MockTransport::new(device.id.clone())),
    );
    session.close().expect("close");
    assert!(matches!(
        session.write_read(&[0x10], 1),
        Err(BusError::SessionUnavailable { .. })
    ));

    let owner = DeviceDescriptor::new("linux.i2c.controller4", InterfaceKind::I2c)
        .expect("owner descriptor");
    let mut controller = LinuxI2cControllerSession::with_transport(
        owner.clone(),
        4,
        SessionAccess::ExclusiveController,
        Box::new(MockControllerTransport::new(owner.id.clone()).with_bytes(0x18, 0x00, [0x1E])),
    );
    controller.close().expect("close controller");
    assert!(matches!(
        controller.write_read(0x18, &[0x00], 1),
        Err(BusError::SessionUnavailable { .. })
    ));

    let reopened = LinuxI2cControllerSession::with_transport(
        owner,
        4,
        SessionAccess::ExclusiveController,
        Box::new(MockControllerTransport::new(
            lemnos_core::DeviceId::new("linux.i2c.controller4").expect("device id"),
        )),
    );
    assert_eq!(reopened.metadata().state, lemnos_bus::SessionState::Idle);
}

#[test]
fn smbus_write_read_fallback_reads_sequential_registers_without_i2c_rdwr() {
    let mut device = SmbusFallbackDevice::new();
    device.registers[0x10] = 0xAA;
    device.registers[0x11] = 0xBB;
    device.registers[0x12] = 0xCC;

    let bytes = smbus::smbus_write_read_fallback(&mut device, &[0x10], 3).expect("fallback read");
    assert_eq!(bytes, vec![0xAA, 0xBB, 0xCC]);
}

#[test]
fn smbus_write_fallback_writes_sequential_registers_without_i2c_block_write() {
    let mut device = SmbusFallbackDevice::new();

    smbus::smbus_write_fallback(&mut device, &[0x20, 0x11, 0x22, 0x33]).expect("fallback write");
    assert_eq!(device.registers[0x20], 0x11);
    assert_eq!(device.registers[0x21], 0x22);
    assert_eq!(device.registers[0x22], 0x33);
}

#[test]
fn smbus_write_read_fallback_rejects_multi_byte_register_prefixes() {
    let mut device = SmbusFallbackDevice::new();
    let error = smbus::smbus_write_read_fallback(&mut device, &[0x00, 0x10], 2)
        .expect_err("multi-byte register prefix should be rejected");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn smbus_helpers_use_block_operations_when_available() {
    let mut device = SmbusFallbackDevice::new()
        .with_block_read_support()
        .with_block_write_support();
    device.registers[0x30] = 0x10;
    device.registers[0x31] = 0x20;
    device.registers[0x32] = 0x30;

    let bytes = smbus::smbus_write_read_fallback(&mut device, &[0x30], 3).expect("block read");
    assert_eq!(bytes, vec![0x10, 0x20, 0x30]);

    smbus::smbus_write_fallback(&mut device, &[0x40, 0xAA, 0xBB]).expect("block write");
    assert_eq!(device.registers[0x40], 0xAA);
    assert_eq!(device.registers[0x41], 0xBB);
}

#[test]
fn classify_open_error_reports_access_conflict_for_kernel_owned_i2c_device() {
    let device = DeviceDescriptor::builder_for_kind(
        "linux.i2c.bus16.0x0050",
        lemnos_core::DeviceKind::I2cDevice,
    )
    .expect("builder")
    .address(lemnos_core::DeviceAddress::I2cDevice {
        bus: 16,
        address: 0x50,
    })
    .property("driver", "ee1004")
    .build()
    .expect("descriptor");

    let error =
        kernel::classify_open_error(&device, "/dev/i2c-16", 0x50, &LinuxI2CError::Errno(16));

    assert!(matches!(
        error,
        BusError::AccessConflict { device_id, reason }
            if device_id == device.id
                && reason.contains("kernel driver 'ee1004'")
    ));
}

#[test]
fn classify_open_error_reports_permission_denied_for_i2c_devnode_access() {
    let device = test_device();
    let error = kernel::classify_open_error(
        &device,
        "/dev/i2c-1",
        0x50,
        &LinuxI2CError::Io(io::Error::from_raw_os_error(13)),
    );

    assert!(matches!(
        error,
        BusError::PermissionDenied {
            device_id,
            operation: "open",
            reason,
        } if device_id == device.id && reason.contains("/dev/i2c-1")
    ));
}

#[test]
fn classify_open_error_reports_session_unavailable_for_missing_or_detached_target() {
    let device = test_device();
    let error = kernel::classify_open_error(&device, "/dev/i2c-1", 0x50, &LinuxI2CError::Errno(6));

    assert!(matches!(
        error,
        BusError::SessionUnavailable { device_id, reason }
            if device_id == device.id && reason.contains("not currently available")
    ));
}
