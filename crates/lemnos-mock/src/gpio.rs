use crate::hardware::{MOCK_BACKEND_NAME, MockHardwareState, take_injected_error};
use lemnos_bus::{
    BusError, BusResult, BusSession, GpioSession, SessionAccess, SessionMetadata, SessionState,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceKind,
    GpioDirection, GpioLevel, GpioLineConfiguration, InterfaceKind,
};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct MockGpioLine {
    descriptor: DeviceDescriptor,
    level: GpioLevel,
    configuration: GpioLineConfiguration,
}

impl MockGpioLine {
    pub fn new(chip_name: impl AsRef<str>, offset: u32) -> Self {
        let chip_name = chip_name.as_ref().to_string();
        let device_id = format!("mock.gpio.{chip_name}.{offset}");
        let display_name = format!("{chip_name}:{offset}");
        let descriptor = DeviceDescriptor::builder_for_kind(device_id, DeviceKind::GpioLine)
            .expect("mock gpio builder")
            .display_name(display_name)
            .summary("Mock GPIO line")
            .address(DeviceAddress::GpioLine {
                chip_name: chip_name.clone(),
                offset,
            })
            .driver_hint("lemnos.gpio.generic")
            .label("chip_name", chip_name.clone())
            .property("offset", u64::from(offset))
            .capability(
                CapabilityDescriptor::new("gpio.read", CapabilityAccess::READ)
                    .expect("gpio.read capability"),
            )
            .capability(
                CapabilityDescriptor::new("gpio.write", CapabilityAccess::WRITE)
                    .expect("gpio.write capability"),
            )
            .capability(
                CapabilityDescriptor::new("gpio.configure", CapabilityAccess::CONFIGURE)
                    .expect("gpio.configure capability"),
            )
            .capability(
                CapabilityDescriptor::new("gpio.get_configuration", CapabilityAccess::READ)
                    .expect("gpio.get_configuration capability"),
            )
            .build()
            .expect("mock gpio descriptor");

        Self {
            descriptor,
            level: GpioLevel::Low,
            configuration: GpioLineConfiguration {
                direction: GpioDirection::Input,
                active_low: false,
                bias: None,
                drive: None,
                edge: None,
                debounce_us: None,
                initial_level: None,
            },
        }
    }

    pub fn with_level(mut self, level: GpioLevel) -> Self {
        self.level = level;
        self
    }

    pub fn with_configuration(mut self, configuration: GpioLineConfiguration) -> Self {
        if let Some(level) = configuration.initial_level {
            self.level = level;
        }
        self.configuration = configuration;
        self
    }

    pub fn with_line_name(mut self, line_name: impl Into<String>) -> Self {
        self.descriptor.add_label("line_name", line_name.into());
        self
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }
}

#[derive(Clone)]
pub(crate) struct MockGpioLineState {
    pub descriptor: DeviceDescriptor,
    pub level: GpioLevel,
    pub configuration: GpioLineConfiguration,
}

impl From<MockGpioLine> for MockGpioLineState {
    fn from(value: MockGpioLine) -> Self {
        Self {
            descriptor: value.descriptor,
            level: value.level,
            configuration: value.configuration,
        }
    }
}

pub(crate) struct MockGpioSession {
    state: Arc<Mutex<MockHardwareState>>,
    device: DeviceDescriptor,
    metadata: SessionMetadata,
}

impl MockGpioSession {
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

    fn line_state(&self) -> BusResult<MockGpioLineState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .gpio_lines
            .get(&self.device.id)
            .cloned()
            .ok_or_else(|| BusError::Disconnected {
                device_id: self.device.id.clone(),
            })
    }

    fn line_state_mut(&self) -> BusResult<MutexGuard<'_, MockHardwareState>> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.gpio_lines.contains_key(&self.device.id) {
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

impl BusSession for MockGpioSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
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

impl GpioSession for MockGpioSession {
    fn read_level(&mut self) -> BusResult<GpioLevel> {
        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "gpio.read")?;
            Ok(session.line_state()?.level)
        })
    }

    fn write_level(&mut self, level: GpioLevel) -> BusResult<()> {
        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "gpio.write")?;
            let mut state = session.line_state_mut()?;
            let line = state
                .gpio_lines
                .get_mut(&session.device.id)
                .expect("line existence checked before mutation");
            if line.configuration.direction != GpioDirection::Output {
                return Err(BusError::PermissionDenied {
                    device_id: session.device.id.clone(),
                    operation: "gpio.write",
                    reason: "line is not configured for output".into(),
                });
            }
            line.level = level;
            Ok(())
        })
    }

    fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> BusResult<()> {
        if configuration.direction == GpioDirection::Input && configuration.initial_level.is_some()
        {
            return Err(BusError::InvalidConfiguration {
                device_id: self.device.id.clone(),
                reason: "input lines cannot set an initial level".into(),
            });
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "gpio.configure")?;
            let mut state = session.line_state_mut()?;
            let line = state
                .gpio_lines
                .get_mut(&session.device.id)
                .expect("line existence checked before mutation");
            line.configuration = configuration.clone();
            if let Some(level) = configuration.initial_level {
                line.level = level;
            }
            Ok(())
        })
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        take_injected_error(&self.state, &self.device.id, "gpio.get_configuration")?;
        Ok(self.line_state()?.configuration)
    }
}
