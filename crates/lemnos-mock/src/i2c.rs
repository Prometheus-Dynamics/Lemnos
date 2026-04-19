use crate::hardware::{MOCK_BACKEND_NAME, MockHardwareState, take_injected_error};
use lemnos_bus::{
    BusError, BusResult, BusSession, I2cControllerSession, I2cSession, SessionAccess,
    SessionMetadata, SessionState,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceKind,
    I2cOperation, InterfaceKind,
};
use std::sync::{Arc, Mutex};

const DEFAULT_I2C_MEMORY_SIZE: usize = 256;

#[derive(Clone)]
pub struct MockI2cDevice {
    descriptor: DeviceDescriptor,
    memory: Vec<u8>,
    pointer: usize,
}

impl MockI2cDevice {
    pub fn new(bus: u32, address: u16) -> Self {
        let device_id = format!("mock.i2c.bus{bus}.0x{address:02x}");
        let display_name = format!("i2c-{bus}-0x{address:02x}");
        let descriptor = DeviceDescriptor::builder_for_kind(device_id, DeviceKind::I2cDevice)
            .expect("mock i2c builder")
            .display_name(display_name)
            .summary("Mock I2C device")
            .address(DeviceAddress::I2cDevice { bus, address })
            .driver_hint("lemnos.i2c.generic")
            .label("bus", bus.to_string())
            .property("bus", u64::from(bus))
            .property("address", u64::from(address))
            .capability(
                CapabilityDescriptor::new("i2c.read", CapabilityAccess::READ)
                    .expect("i2c.read capability"),
            )
            .capability(
                CapabilityDescriptor::new("i2c.write", CapabilityAccess::WRITE)
                    .expect("i2c.write capability"),
            )
            .capability(
                CapabilityDescriptor::new("i2c.write_read", CapabilityAccess::READ_WRITE)
                    .expect("i2c.write_read capability"),
            )
            .capability(
                CapabilityDescriptor::new("i2c.transaction", CapabilityAccess::FULL)
                    .expect("i2c.transaction capability"),
            )
            .build()
            .expect("mock i2c descriptor");

        Self {
            descriptor,
            memory: vec![0; DEFAULT_I2C_MEMORY_SIZE],
            pointer: 0,
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.descriptor.display_name = Some(display_name.into());
        self
    }

    pub fn with_memory(mut self, memory: impl Into<Vec<u8>>) -> Self {
        let memory = memory.into();
        self.memory = if memory.is_empty() {
            vec![0; DEFAULT_I2C_MEMORY_SIZE]
        } else {
            memory
        };
        self
    }

    pub fn with_bytes(mut self, offset: u8, bytes: impl AsRef<[u8]>) -> Self {
        let offset = offset as usize;
        let bytes = bytes.as_ref();
        let required_len = offset + bytes.len();
        if self.memory.len() < required_len {
            self.memory.resize(required_len, 0);
        }
        self.memory[offset..required_len].copy_from_slice(bytes);
        self
    }

    pub fn with_u8(self, offset: u8, value: u8) -> Self {
        self.with_bytes(offset, [value])
    }

    pub fn with_be_u16(self, offset: u8, value: u16) -> Self {
        self.with_bytes(offset, value.to_be_bytes())
    }

    pub fn with_le_u16(self, offset: u8, value: u16) -> Self {
        self.with_bytes(offset, value.to_le_bytes())
    }

    pub fn with_be_i16(self, offset: u8, value: i16) -> Self {
        self.with_bytes(offset, value.to_be_bytes())
    }

    pub fn with_le_i16(self, offset: u8, value: i16) -> Self {
        self.with_bytes(offset, value.to_le_bytes())
    }

    pub fn with_pointer(mut self, pointer: usize) -> Self {
        self.pointer = pointer;
        self
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }
}

#[derive(Clone)]
pub(crate) struct MockI2cDeviceState {
    pub descriptor: DeviceDescriptor,
    pub memory: Vec<u8>,
    pub pointer: usize,
}

impl From<MockI2cDevice> for MockI2cDeviceState {
    fn from(value: MockI2cDevice) -> Self {
        Self {
            descriptor: value.descriptor,
            memory: value.memory,
            pointer: value.pointer,
        }
    }
}

fn invalid_i2c_request(
    device_id: &lemnos_core::DeviceId,
    operation: &'static str,
    reason: impl Into<String>,
) -> BusError {
    BusError::InvalidRequest {
        device_id: device_id.clone(),
        operation,
        reason: reason.into(),
    }
}

fn read_from_state(device: &mut MockI2cDeviceState, length: u32) -> Vec<u8> {
    let start = device.pointer;
    let length = length as usize;
    let mut bytes = Vec::with_capacity(length);
    for index in 0..length {
        bytes.push(device.memory.get(start + index).copied().unwrap_or(0));
    }
    device.pointer = start.saturating_add(length);
    bytes
}

fn write_to_state(
    device_id: &lemnos_core::DeviceId,
    device: &mut MockI2cDeviceState,
    bytes: &[u8],
) -> BusResult<()> {
    if bytes.is_empty() {
        return Err(invalid_i2c_request(
            device_id,
            "i2c.write",
            "write payload must not be empty",
        ));
    }

    let offset = bytes[0] as usize;
    device.pointer = offset;
    if bytes.len() > 1 {
        let write_len = bytes.len() - 1;
        let required_len = offset + write_len;
        if device.memory.len() < required_len {
            device.memory.resize(required_len, 0);
        }
        device.memory[offset..required_len].copy_from_slice(&bytes[1..]);
        device.pointer = required_len;
    }
    Ok(())
}

pub(crate) struct MockI2cSession {
    state: Arc<Mutex<MockHardwareState>>,
    device: DeviceDescriptor,
    metadata: SessionMetadata,
}

impl MockI2cSession {
    pub(crate) fn new(
        state: Arc<Mutex<MockHardwareState>>,
        device: DeviceDescriptor,
        access: SessionAccess,
    ) -> Self {
        Self {
            state,
            device,
            metadata: SessionMetadata::new(MOCK_BACKEND_NAME, access)
                .with_state(SessionState::Idle),
        }
    }

    fn invalid_request(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        invalid_i2c_request(&self.device.id, operation, reason)
    }

    fn with_device_state_mut<R>(
        &self,
        update: impl FnOnce(&mut MockI2cDeviceState) -> BusResult<R>,
    ) -> BusResult<R> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let device =
            state
                .i2c_devices
                .get_mut(&self.device.id)
                .ok_or_else(|| BusError::Disconnected {
                    device_id: self.device.id.clone(),
                })?;
        update(device)
    }

    fn run_call<T>(&mut self, call: impl FnOnce(&mut Self) -> BusResult<T>) -> BusResult<T> {
        self.metadata.begin_call();
        let result = call(self);
        self.metadata.finish_call(&result);
        result
    }
}

impl BusSession for MockI2cSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.metadata.mark_closed();
        Ok(())
    }
}

impl I2cSession for MockI2cSession {
    fn read(&mut self, length: u32) -> BusResult<Vec<u8>> {
        if length == 0 {
            return Err(self.invalid_request("i2c.read", "read length must be greater than zero"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "i2c.read")?;
            session.with_device_state_mut(|device| Ok(read_from_state(device, length)))
        })
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "i2c.write")?;
            session
                .with_device_state_mut(|device| write_to_state(&session.device.id, device, bytes))
        })
    }

    fn write_read(&mut self, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        if read_length == 0 {
            return Err(
                self.invalid_request("i2c.write_read", "read length must be greater than zero")
            );
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "i2c.write_read")?;
            session.with_device_state_mut(|device| {
                if write.len() == 1 {
                    device.pointer = write[0] as usize;
                } else {
                    write_to_state(&session.device.id, device, write)?;
                    device.pointer = write.first().copied().unwrap_or(0) as usize;
                }

                Ok(read_from_state(device, read_length))
            })
        })
    }

    fn transaction(&mut self, operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>> {
        if operations.is_empty() {
            return Err(self.invalid_request(
                "i2c.transaction",
                "transaction operations must not be empty",
            ));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "i2c.transaction")?;
            session.with_device_state_mut(|device| {
                let mut results = Vec::with_capacity(operations.len());
                for operation in operations {
                    match operation {
                        I2cOperation::Read { length } => {
                            results.push(read_from_state(device, *length))
                        }
                        I2cOperation::Write { bytes } => {
                            write_to_state(&session.device.id, device, bytes)?;
                            results.push(Vec::new());
                        }
                    }
                }
                Ok(results)
            })
        })
    }
}

pub(crate) struct MockI2cControllerSession {
    state: Arc<Mutex<MockHardwareState>>,
    owner: DeviceDescriptor,
    bus: u32,
    metadata: SessionMetadata,
}

impl MockI2cControllerSession {
    pub(crate) fn new(
        state: Arc<Mutex<MockHardwareState>>,
        owner: DeviceDescriptor,
        bus: u32,
        access: SessionAccess,
    ) -> Self {
        Self {
            state,
            owner,
            bus,
            metadata: SessionMetadata::new(MOCK_BACKEND_NAME, access)
                .with_state(SessionState::Idle),
        }
    }

    fn invalid_request(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        invalid_i2c_request(&self.owner.id, operation, reason)
    }

    fn with_target_device_state_mut<R>(
        &self,
        address: u16,
        update: impl FnOnce(&mut MockI2cDeviceState) -> BusResult<R>,
    ) -> BusResult<R> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(device_id) =
            state.i2c_devices.iter().find_map(|(device_id, device)| {
                match device.descriptor.address.as_ref() {
                    Some(DeviceAddress::I2cDevice {
                        bus,
                        address: candidate,
                    }) if *bus == self.bus && *candidate == address => Some(device_id.clone()),
                    _ => None,
                }
            })
        else {
            return Err(BusError::SessionUnavailable {
                device_id: self.owner.id.clone(),
                reason: format!(
                    "mock I2C address 0x{address:02x} is not available on bus {}",
                    self.bus
                ),
            });
        };
        let device = state
            .i2c_devices
            .get_mut(&device_id)
            .expect("device exists after lookup");
        update(device)
    }

    fn run_call<T>(&mut self, call: impl FnOnce(&mut Self) -> BusResult<T>) -> BusResult<T> {
        self.metadata.begin_call();
        let result = call(self);
        self.metadata.finish_call(&result);
        result
    }
}

impl BusSession for MockI2cControllerSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.owner
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.metadata.mark_closed();
        Ok(())
    }
}

impl I2cControllerSession for MockI2cControllerSession {
    fn bus(&self) -> u32 {
        self.bus
    }

    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>> {
        if length == 0 {
            return Err(self.invalid_request("i2c.read", "read length must be greater than zero"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.owner.id, "i2c.read")?;
            session
                .with_target_device_state_mut(address, |device| Ok(read_from_state(device, length)))
        })
    }

    fn write(&mut self, address: u16, bytes: &[u8]) -> BusResult<()> {
        self.run_call(|session| {
            take_injected_error(&session.state, &session.owner.id, "i2c.write")?;
            session.with_target_device_state_mut(address, |device| {
                write_to_state(&session.owner.id, device, bytes)
            })
        })
    }

    fn write_read(&mut self, address: u16, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        if read_length == 0 {
            return Err(
                self.invalid_request("i2c.write_read", "read length must be greater than zero")
            );
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.owner.id, "i2c.write_read")?;
            session.with_target_device_state_mut(address, |device| {
                if write.len() == 1 {
                    device.pointer = write[0] as usize;
                } else {
                    write_to_state(&session.owner.id, device, write)?;
                    device.pointer = write.first().copied().unwrap_or(0) as usize;
                }
                Ok(read_from_state(device, read_length))
            })
        })
    }

    fn transaction(
        &mut self,
        address: u16,
        operations: &[I2cOperation],
    ) -> BusResult<Vec<Vec<u8>>> {
        if operations.is_empty() {
            return Err(self.invalid_request(
                "i2c.transaction",
                "transaction operations must not be empty",
            ));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.owner.id, "i2c.transaction")?;
            session.with_target_device_state_mut(address, |device| {
                let mut results = Vec::with_capacity(operations.len());
                for operation in operations {
                    match operation {
                        I2cOperation::Read { length } => {
                            results.push(read_from_state(device, *length))
                        }
                        I2cOperation::Write { bytes } => {
                            write_to_state(&session.owner.id, device, bytes)?;
                            results.push(Vec::new());
                        }
                    }
                }
                Ok(results)
            })
        })
    }
}
