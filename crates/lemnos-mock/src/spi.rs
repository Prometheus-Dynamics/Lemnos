use crate::hardware::{MOCK_BACKEND_NAME, MockHardwareState, take_injected_error};
use lemnos_bus::{
    BusError, BusResult, BusSession, SessionAccess, SessionMetadata, SessionState, SpiSession,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceKind,
    InterfaceKind, SpiBitOrder, SpiConfiguration, SpiMode,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct MockSpiDevice {
    descriptor: DeviceDescriptor,
    configuration: SpiConfiguration,
    transfer_responses: BTreeMap<Vec<u8>, Vec<u8>>,
    last_write: Vec<u8>,
}

impl MockSpiDevice {
    pub fn new(bus: u32, chip_select: u16) -> Self {
        let device_id = format!("mock.spi.bus{bus}.cs{chip_select}");
        let display_name = format!("spi-{bus}.{chip_select}");
        let descriptor = DeviceDescriptor::builder_for_kind(device_id, DeviceKind::SpiDevice)
            .expect("mock spi builder")
            .display_name(display_name)
            .summary("Mock SPI device")
            .address(DeviceAddress::SpiDevice { bus, chip_select })
            .driver_hint("lemnos.spi.generic")
            .label("bus", bus.to_string())
            .property("bus", u64::from(bus))
            .property("chip_select", u64::from(chip_select))
            .capability(
                CapabilityDescriptor::new("spi.transfer", CapabilityAccess::READ_WRITE)
                    .expect("spi.transfer capability"),
            )
            .capability(
                CapabilityDescriptor::new("spi.write", CapabilityAccess::WRITE)
                    .expect("spi.write capability"),
            )
            .capability(
                CapabilityDescriptor::new("spi.configure", CapabilityAccess::CONFIGURE)
                    .expect("spi.configure capability"),
            )
            .capability(
                CapabilityDescriptor::new("spi.get_configuration", CapabilityAccess::READ)
                    .expect("spi.get_configuration capability"),
            )
            .build()
            .expect("mock spi descriptor");

        Self {
            descriptor,
            configuration: SpiConfiguration {
                mode: SpiMode::Mode0,
                max_frequency_hz: Some(1_000_000),
                bits_per_word: Some(8),
                bit_order: SpiBitOrder::MsbFirst,
            },
            transfer_responses: BTreeMap::new(),
            last_write: Vec::new(),
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.descriptor.display_name = Some(display_name.into());
        self
    }

    pub fn with_configuration(mut self, configuration: SpiConfiguration) -> Self {
        validate_configuration(&self.descriptor, &configuration)
            .expect("mock SPI configuration must be valid");
        self.configuration = configuration;
        self
    }

    pub fn with_transfer_response(
        mut self,
        write: impl Into<Vec<u8>>,
        read: impl Into<Vec<u8>>,
    ) -> Self {
        self.transfer_responses.insert(write.into(), read.into());
        self
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }
}

#[derive(Clone)]
pub(crate) struct MockSpiDeviceState {
    pub descriptor: DeviceDescriptor,
    pub configuration: SpiConfiguration,
    pub transfer_responses: BTreeMap<Vec<u8>, Vec<u8>>,
    pub last_write: Vec<u8>,
}

impl From<MockSpiDevice> for MockSpiDeviceState {
    fn from(value: MockSpiDevice) -> Self {
        Self {
            descriptor: value.descriptor,
            configuration: value.configuration,
            transfer_responses: value.transfer_responses,
            last_write: value.last_write,
        }
    }
}

pub(crate) struct MockSpiSession {
    state: Arc<Mutex<MockHardwareState>>,
    device: DeviceDescriptor,
    metadata: SessionMetadata,
}

impl MockSpiSession {
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
        BusError::InvalidRequest {
            device_id: self.device.id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn permission_denied(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::PermissionDenied {
            device_id: self.device.id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn device_state(&self) -> BusResult<MockSpiDeviceState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .spi_devices
            .get(&self.device.id)
            .cloned()
            .ok_or_else(|| BusError::Disconnected {
                device_id: self.device.id.clone(),
            })
    }

    fn device_state_mut(&self) -> BusResult<MutexGuard<'_, MockHardwareState>> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.spi_devices.contains_key(&self.device.id) {
            return Err(BusError::Disconnected {
                device_id: self.device.id.clone(),
            });
        }
        Ok(guard)
    }

    fn run_call<T>(&mut self, call: impl FnOnce(&mut Self) -> BusResult<T>) -> BusResult<T> {
        self.metadata.begin_call();
        let result = call(self);
        self.metadata.finish_call(&result);
        result
    }
}

impl BusSession for MockSpiSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Spi
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

impl SpiSession for MockSpiSession {
    fn transfer(&mut self, write: &[u8]) -> BusResult<Vec<u8>> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("spi.transfer", "session access is read-only"));
        }
        if write.is_empty() {
            return Err(self.invalid_request("spi.transfer", "transfer payload must not be empty"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "spi.transfer")?;
            let mut state = session.device_state_mut()?;
            let device = state
                .spi_devices
                .get_mut(&session.device.id)
                .expect("device existence checked before mutation");
            device.last_write = write.to_vec();
            Ok(device
                .transfer_responses
                .get(write)
                .cloned()
                .unwrap_or_else(|| vec![0; write.len()]))
        })
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("spi.write", "session access is read-only"));
        }
        if bytes.is_empty() {
            return Err(self.invalid_request("spi.write", "write payload must not be empty"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "spi.write")?;
            let mut state = session.device_state_mut()?;
            let device = state
                .spi_devices
                .get_mut(&session.device.id)
                .expect("device existence checked before mutation");
            device.last_write = bytes.to_vec();
            Ok(())
        })
    }

    fn configure(&mut self, configuration: &SpiConfiguration) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied(
                "spi.configure",
                "session access does not allow configuration changes",
            ));
        }
        validate_configuration(&self.device, configuration)?;

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "spi.configure")?;
            let mut state = session.device_state_mut()?;
            let device = state
                .spi_devices
                .get_mut(&session.device.id)
                .expect("device existence checked before mutation");
            device.configuration = configuration.clone();
            Ok(())
        })
    }

    fn configuration(&self) -> BusResult<SpiConfiguration> {
        take_injected_error(&self.state, &self.device.id, "spi.get_configuration")?;
        Ok(self.device_state()?.configuration)
    }
}

fn validate_configuration(
    device: &DeviceDescriptor,
    configuration: &SpiConfiguration,
) -> BusResult<()> {
    if configuration.max_frequency_hz == Some(0) {
        return Err(BusError::InvalidConfiguration {
            device_id: device.id.clone(),
            reason: "SPI max frequency must be greater than zero".into(),
        });
    }
    if configuration.bits_per_word == Some(0) {
        return Err(BusError::InvalidConfiguration {
            device_id: device.id.clone(),
            reason: "SPI bits per word must be greater than zero".into(),
        });
    }
    Ok(())
}
