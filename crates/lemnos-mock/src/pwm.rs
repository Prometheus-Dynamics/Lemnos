use crate::hardware::{MOCK_BACKEND_NAME, MockHardwareState, take_injected_error};
use lemnos_bus::{
    BusError, BusResult, BusSession, PwmSession, SessionAccess, SessionMetadata, SessionState,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceKind,
    InterfaceKind, PwmConfiguration, PwmPolarity,
};
use std::sync::{Arc, Mutex, MutexGuard};

const DEFAULT_PERIOD_NS: u64 = 20_000_000;

#[derive(Clone)]
pub struct MockPwmChannel {
    descriptor: DeviceDescriptor,
    configuration: PwmConfiguration,
}

impl MockPwmChannel {
    pub fn new(chip_name: impl AsRef<str>, channel: u32) -> Self {
        let chip_name = chip_name.as_ref().to_string();
        let device_id = format!("mock.pwm.{chip_name}.{channel}");
        let display_name = format!("{chip_name}:{channel}");
        let descriptor = DeviceDescriptor::builder_for_kind(device_id, DeviceKind::PwmChannel)
            .expect("mock pwm builder")
            .display_name(display_name)
            .summary("Mock PWM channel")
            .address(DeviceAddress::PwmChannel {
                chip_name: chip_name.clone(),
                channel,
            })
            .driver_hint("lemnos.pwm.generic")
            .label("chip_name", chip_name.clone())
            .property("chip_name", chip_name.clone())
            .property("channel", u64::from(channel))
            .capability(
                CapabilityDescriptor::new("pwm.enable", CapabilityAccess::WRITE)
                    .expect("pwm.enable capability"),
            )
            .capability(
                CapabilityDescriptor::new("pwm.configure", CapabilityAccess::CONFIGURE)
                    .expect("pwm.configure capability"),
            )
            .capability(
                CapabilityDescriptor::new("pwm.set_period", CapabilityAccess::CONFIGURE)
                    .expect("pwm.set_period capability"),
            )
            .capability(
                CapabilityDescriptor::new("pwm.set_duty_cycle", CapabilityAccess::CONFIGURE)
                    .expect("pwm.set_duty_cycle capability"),
            )
            .capability(
                CapabilityDescriptor::new("pwm.get_configuration", CapabilityAccess::READ)
                    .expect("pwm.get_configuration capability"),
            )
            .build()
            .expect("mock pwm descriptor");

        Self {
            descriptor,
            configuration: PwmConfiguration {
                period_ns: DEFAULT_PERIOD_NS,
                duty_cycle_ns: 0,
                enabled: false,
                polarity: PwmPolarity::Normal,
            },
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.descriptor.display_name = Some(display_name.into());
        self
    }

    pub fn with_configuration(mut self, configuration: PwmConfiguration) -> Self {
        validate_configuration(&self.descriptor, &configuration)
            .expect("mock PWM configuration must be valid");
        self.configuration = configuration;
        self
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }
}

#[derive(Clone)]
pub(crate) struct MockPwmChannelState {
    pub descriptor: DeviceDescriptor,
    pub configuration: PwmConfiguration,
}

impl From<MockPwmChannel> for MockPwmChannelState {
    fn from(value: MockPwmChannel) -> Self {
        Self {
            descriptor: value.descriptor,
            configuration: value.configuration,
        }
    }
}

pub(crate) struct MockPwmSession {
    state: Arc<Mutex<MockHardwareState>>,
    device: DeviceDescriptor,
    metadata: SessionMetadata,
}

impl MockPwmSession {
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

    fn permission_denied(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::PermissionDenied {
            device_id: self.device.id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn channel_state(&self) -> BusResult<MockPwmChannelState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pwm_channels
            .get(&self.device.id)
            .cloned()
            .ok_or_else(|| BusError::Disconnected {
                device_id: self.device.id.clone(),
            })
    }

    fn channel_state_mut(&self) -> BusResult<MutexGuard<'_, MockHardwareState>> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.pwm_channels.contains_key(&self.device.id) {
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

impl BusSession for MockPwmSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Pwm
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

impl PwmSession for MockPwmSession {
    fn set_enabled(&mut self, enabled: bool) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("pwm.enable", "session access is read-only"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "pwm.enable")?;
            let mut state = session.channel_state_mut()?;
            let channel = state
                .pwm_channels
                .get_mut(&session.device.id)
                .expect("channel existence checked before mutation");
            channel.configuration.enabled = enabled;
            Ok(())
        })
    }

    fn set_period_ns(&mut self, period_ns: u64) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("pwm.set_period", "session access is read-only"));
        }
        if period_ns == 0 {
            return Err(BusError::InvalidConfiguration {
                device_id: self.device.id.clone(),
                reason: "PWM period must be greater than zero".into(),
            });
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "pwm.set_period")?;
            let mut state = session.channel_state_mut()?;
            let channel = state
                .pwm_channels
                .get_mut(&session.device.id)
                .expect("channel existence checked before mutation");
            if channel.configuration.duty_cycle_ns > period_ns {
                return Err(BusError::InvalidConfiguration {
                    device_id: session.device.id.clone(),
                    reason: "PWM duty cycle must not exceed the period".into(),
                });
            }
            channel.configuration.period_ns = period_ns;
            Ok(())
        })
    }

    fn set_duty_cycle_ns(&mut self, duty_cycle_ns: u64) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("pwm.set_duty_cycle", "session access is read-only"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "pwm.set_duty_cycle")?;
            let mut state = session.channel_state_mut()?;
            let channel = state
                .pwm_channels
                .get_mut(&session.device.id)
                .expect("channel existence checked before mutation");
            if duty_cycle_ns > channel.configuration.period_ns {
                return Err(BusError::InvalidConfiguration {
                    device_id: session.device.id.clone(),
                    reason: "PWM duty cycle must not exceed the period".into(),
                });
            }
            channel.configuration.duty_cycle_ns = duty_cycle_ns;
            Ok(())
        })
    }

    fn configure(&mut self, configuration: &PwmConfiguration) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied(
                "pwm.configure",
                "session access does not allow configuration changes",
            ));
        }

        validate_configuration(&self.device, configuration)?;

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "pwm.configure")?;
            let mut state = session.channel_state_mut()?;
            let channel = state
                .pwm_channels
                .get_mut(&session.device.id)
                .expect("channel existence checked before mutation");
            channel.configuration = configuration.clone();
            Ok(())
        })
    }

    fn configuration(&self) -> BusResult<PwmConfiguration> {
        take_injected_error(&self.state, &self.device.id, "pwm.get_configuration")?;
        Ok(self.channel_state()?.configuration)
    }
}

fn validate_configuration(
    device: &DeviceDescriptor,
    configuration: &PwmConfiguration,
) -> BusResult<()> {
    if configuration.period_ns == 0 {
        return Err(BusError::InvalidConfiguration {
            device_id: device.id.clone(),
            reason: "PWM period must be greater than zero".into(),
        });
    }
    if configuration.duty_cycle_ns > configuration.period_ns {
        return Err(BusError::InvalidConfiguration {
            device_id: device.id.clone(),
            reason: "PWM duty cycle must not exceed the period".into(),
        });
    }
    Ok(())
}
