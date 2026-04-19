use crate::GpioDriver;
use lemnos_bus::{
    BusBackend, BusResult, BusSession, GpioSession, SessionAccess, SessionMetadata, SessionState,
};
use lemnos_core::{
    DeviceAddress, DeviceDescriptor, DeviceKind, DeviceLifecycleState, GpioDirection, GpioLevel,
    GpioLineConfiguration, GpioRequest, InteractionRequest, InteractionResponse, StandardRequest,
    StandardResponse, Value,
};
use lemnos_driver_sdk::{Driver, DriverBindContext, gpio};
use std::sync::{Arc, Mutex, MutexGuard};

struct FakeGpioBackend {
    state: Arc<Mutex<FakeLineState>>,
}

#[derive(Clone)]
struct FakeLineState {
    device: DeviceDescriptor,
    level: GpioLevel,
    configuration: GpioLineConfiguration,
}

struct FakeGpioSession {
    state: Arc<Mutex<FakeLineState>>,
    device: DeviceDescriptor,
    metadata: SessionMetadata,
}

impl FakeGpioBackend {
    fn new(device: DeviceDescriptor) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeLineState {
                device,
                level: GpioLevel::Low,
                configuration: GpioLineConfiguration {
                    direction: GpioDirection::Output,
                    active_low: false,
                    bias: None,
                    drive: None,
                    edge: None,
                    debounce_us: None,
                    initial_level: Some(GpioLevel::Low),
                },
            })),
        }
    }
}

impl BusBackend for FakeGpioBackend {
    fn name(&self) -> &str {
        "fake-gpio"
    }

    fn supported_interfaces(&self) -> &'static [lemnos_core::InterfaceKind] {
        &[lemnos_core::InterfaceKind::Gpio]
    }

    fn supports_device(&self, device: &DeviceDescriptor) -> bool {
        self.state.lock().expect("state").device.id.eq(&device.id)
    }
}

impl lemnos_bus::GpioBusBackend for FakeGpioBackend {
    fn open_gpio(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn GpioSession>> {
        if !self.supports_device(device) {
            return Err(lemnos_bus::BusError::UnsupportedDevice {
                backend: self.name().to_string(),
                device_id: device.id.clone(),
            });
        }

        Ok(Box::new(FakeGpioSession {
            state: Arc::clone(&self.state),
            device: device.clone(),
            metadata: SessionMetadata::new(self.name(), access).with_state(SessionState::Idle),
        }))
    }
}

impl FakeGpioSession {
    fn state(&self) -> MutexGuard<'_, FakeLineState> {
        self.state.lock().expect("state")
    }
}

impl BusSession for FakeGpioSession {
    fn interface(&self) -> lemnos_core::InterfaceKind {
        lemnos_core::InterfaceKind::Gpio
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.metadata.state = SessionState::Closed;
        Ok(())
    }
}

impl GpioSession for FakeGpioSession {
    fn read_level(&mut self) -> BusResult<GpioLevel> {
        Ok(self.state().level)
    }

    fn write_level(&mut self, level: GpioLevel) -> BusResult<()> {
        let mut state = self.state();
        state.level = level;
        Ok(())
    }

    fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> BusResult<()> {
        let mut state = self.state();
        state.configuration = configuration.clone();
        if let Some(level) = configuration.initial_level {
            state.level = level;
        }
        Ok(())
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        Ok(self.state().configuration.clone())
    }
}

fn gpio_line() -> DeviceDescriptor {
    DeviceDescriptor::builder_for_kind("mock.gpio.gpiochip0.3", DeviceKind::GpioLine)
        .expect("builder")
        .address(DeviceAddress::GpioLine {
            chip_name: "gpiochip0".into(),
            offset: 3,
        })
        .build()
        .expect("descriptor")
}

#[test]
fn binds_and_handles_gpio_requests() {
    let device = gpio_line();
    let backend = FakeGpioBackend::new(device.clone());
    let mut bound = GpioDriver
        .bind(&device, &DriverBindContext::default().with_gpio(&backend))
        .expect("bind");

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Gpio(
            GpioRequest::Write {
                level: GpioLevel::High,
            },
        )))
        .expect("write");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Gpio(lemnos_core::GpioResponse::Applied))
    );

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Gpio(
            GpioRequest::Read,
        )))
        .expect("read");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Gpio(lemnos_core::GpioResponse::Level(
            GpioLevel::High
        )))
    );
}

#[test]
fn state_reports_configuration_and_level() {
    let device = gpio_line();
    let backend = FakeGpioBackend::new(device.clone());
    let mut bound = GpioDriver
        .bind(&device, &DriverBindContext::default().with_gpio(&backend))
        .expect("bind");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");

    assert_eq!(state.lifecycle, DeviceLifecycleState::Idle);
    assert_eq!(
        state.realized_config.get("direction"),
        Some(&Value::from("output"))
    );
    assert_eq!(state.telemetry.get("level"), Some(&Value::from("low")));
    assert_eq!(state.telemetry.get("read_ops"), Some(&Value::from(0_u64)));
    assert_eq!(state.telemetry.get("write_ops"), Some(&Value::from(0_u64)));
    assert_eq!(
        state.telemetry.get("configure_ops"),
        Some(&Value::from(0_u64))
    );
}

#[test]
fn state_reports_gpio_operation_stats() {
    let device = gpio_line();
    let backend = FakeGpioBackend::new(device.clone());
    let mut bound = GpioDriver
        .bind(&device, &DriverBindContext::default().with_gpio(&backend))
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Gpio(
            GpioRequest::Write {
                level: GpioLevel::High,
            },
        )))
        .expect("write");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Gpio(
            GpioRequest::Read,
        )))
        .expect("read");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");

    assert_eq!(state.telemetry.get("read_ops"), Some(&Value::from(1_u64)));
    assert_eq!(state.telemetry.get("write_ops"), Some(&Value::from(1_u64)));
    assert_eq!(
        state.telemetry.get("configure_ops"),
        Some(&Value::from(0_u64))
    );
    assert_eq!(
        state
            .last_operation
            .as_ref()
            .map(|operation| operation.interaction.as_str()),
        Some(gpio::READ_INTERACTION)
    );
}
