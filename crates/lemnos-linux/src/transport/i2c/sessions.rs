use super::{
    BusError, BusResult, DeviceDescriptor, I2cControllerSession, I2cControllerTransport,
    I2cOperation, I2cSession, I2cTransport, InterfaceKind, LinuxKernelI2cControllerTransport,
    LinuxKernelI2cTransport, LinuxPaths, SessionAccess, supports_descriptor,
};
use crate::backend::BACKEND_NAME;
use crate::metadata::descriptor_devnode;
use crate::transport::session;
use lemnos_bus::{BusSession, SessionMetadata, SessionState};

pub(crate) struct LinuxI2cSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    transport: Box<dyn I2cTransport>,
}

impl LinuxI2cSession {
    pub(super) fn open(
        paths: &LinuxPaths,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Self> {
        if !supports_descriptor(device) {
            return Err(BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            });
        }

        let (bus, address) = crate::transport::i2c_bus_address(device).ok_or_else(|| {
            BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            }
        })?;
        let devnode = descriptor_devnode(device)
            .map(str::to_owned)
            .unwrap_or_else(|| super::resolve_devnode(paths, bus));
        let transport = LinuxKernelI2cTransport::new(device, &devnode, address)?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport: Box::new(transport),
        })
    }

    #[cfg(test)]
    pub(super) fn with_transport(
        device: DeviceDescriptor,
        access: SessionAccess,
        transport: Box<dyn I2cTransport>,
    ) -> Self {
        Self {
            device,
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport,
        }
    }
}

impl BusSession for LinuxI2cSession {
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

impl I2cSession for LinuxI2cSession {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<()> {
        session::ensure_open(&self.metadata, &self.device.id, "I2C", "i2c.read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.read_into(buffer)
        })
    }

    fn read(&mut self, length: u32) -> BusResult<Vec<u8>> {
        session::ensure_open(&self.metadata, &self.device.id, "I2C", "i2c.read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.read(length)
        })
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        session::ensure_open(&self.metadata, &self.device.id, "I2C", "i2c.write")?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "i2c.write",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.write(bytes)
        })
    }

    fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        session::ensure_open(&self.metadata, &self.device.id, "I2C", "i2c.write_read")?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "i2c.write_read",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.write_read_into(write, read)
        })
    }

    fn write_read(&mut self, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        session::ensure_open(&self.metadata, &self.device.id, "I2C", "i2c.write_read")?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.device.id,
                "i2c.write_read",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.write_read(write, read_length)
        })
    }

    fn transaction(&mut self, operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>> {
        session::ensure_open(&self.metadata, &self.device.id, "I2C", "i2c.transaction")?;
        if operations
            .iter()
            .any(|operation| matches!(operation, I2cOperation::Write { .. }))
            && !self.metadata.access.can_write()
        {
            return Err(session::permission_denied(
                &self.device.id,
                "i2c.transaction",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.transaction(operations)
        })
    }
}

pub(crate) struct LinuxI2cControllerSession {
    owner: DeviceDescriptor,
    bus: u32,
    metadata: SessionMetadata,
    transport: Box<dyn I2cControllerTransport>,
}

impl LinuxI2cControllerSession {
    pub(super) fn open(
        paths: &LinuxPaths,
        owner: &DeviceDescriptor,
        bus: u32,
        access: SessionAccess,
    ) -> BusResult<Self> {
        if owner.interface != InterfaceKind::I2c {
            return Err(BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: owner.id.clone(),
            });
        }

        let devnode = descriptor_devnode(owner)
            .map(str::to_owned)
            .unwrap_or_else(|| paths.i2c_devnode(bus).display().to_string());
        let transport = LinuxKernelI2cControllerTransport::new(owner, devnode);

        Ok(Self {
            owner: owner.clone(),
            bus,
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport: Box::new(transport),
        })
    }

    #[cfg(test)]
    pub(super) fn with_transport(
        owner: DeviceDescriptor,
        bus: u32,
        access: SessionAccess,
        transport: Box<dyn I2cControllerTransport>,
    ) -> Self {
        Self {
            owner,
            bus,
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport,
        }
    }
}

impl BusSession for LinuxI2cControllerSession {
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

impl I2cControllerSession for LinuxI2cControllerSession {
    fn bus(&self) -> u32 {
        self.bus
    }

    fn read_into(&mut self, address: u16, buffer: &mut [u8]) -> BusResult<()> {
        session::ensure_open(&self.metadata, &self.owner.id, "I2C controller", "i2c.read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.read_into(address, buffer)
        })
    }

    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>> {
        session::ensure_open(&self.metadata, &self.owner.id, "I2C controller", "i2c.read")?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.read(address, length)
        })
    }

    fn write(&mut self, address: u16, bytes: &[u8]) -> BusResult<()> {
        session::ensure_open(
            &self.metadata,
            &self.owner.id,
            "I2C controller",
            "i2c.write",
        )?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.owner.id,
                "i2c.write",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.write(address, bytes)
        })
    }

    fn write_read_into(&mut self, address: u16, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        session::ensure_open(
            &self.metadata,
            &self.owner.id,
            "I2C controller",
            "i2c.write_read",
        )?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.owner.id,
                "i2c.write_read",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.write_read_into(address, write, read)
        })
    }

    fn write_read(&mut self, address: u16, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        session::ensure_open(
            &self.metadata,
            &self.owner.id,
            "I2C controller",
            "i2c.write_read",
        )?;
        if !self.metadata.access.can_write() {
            return Err(session::permission_denied(
                &self.owner.id,
                "i2c.write_read",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.write_read(address, write, read_length)
        })
    }

    fn transaction(
        &mut self,
        address: u16,
        operations: &[I2cOperation],
    ) -> BusResult<Vec<Vec<u8>>> {
        session::ensure_open(
            &self.metadata,
            &self.owner.id,
            "I2C controller",
            "i2c.transaction",
        )?;
        if operations
            .iter()
            .any(|operation| matches!(operation, I2cOperation::Write { .. }))
            && !self.metadata.access.can_write()
        {
            return Err(session::permission_denied(
                &self.owner.id,
                "i2c.transaction",
                "session access is read-only",
            ));
        }
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            transport.transaction(address, operations)
        })
    }
}
