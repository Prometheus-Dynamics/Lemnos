#![allow(clippy::print_stdout)]

use lemnos_bus::{
    BusBackend, BusError, BusResult, BusSession, GpioBusBackend, GpioSession, SessionAccess,
    SessionMetadata, SessionState,
};
use lemnos_core::{
    DeviceDescriptor, DeviceKind, DeviceStateSnapshot, GpioDirection, GpioLevel,
    GpioLineConfiguration, GpioRequest, GpioResponse, InteractionRequest, InteractionResponse,
    InterfaceKind, StandardRequest, StandardResponse,
};
use lemnos_driver_manifest::{DriverManifest, DriverPriority, MatchCondition, MatchRule};
use lemnos_driver_sdk::{
    BoundDevice, CONFIG_ACTIVE_LOW, CONFIG_DIRECTION, CONFIG_LEVEL, Driver, DriverBindContext,
    DriverError, DriverResult, GpioDeviceIo, gpio, interaction_name,
};
use std::borrow::Cow;
use std::error::Error;

struct ExampleGpioDriver;

impl Driver for ExampleGpioDriver {
    fn id(&self) -> &str {
        "example.gpio.read-only"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                self.id(),
                "Example read-only GPIO driver",
                vec![InterfaceKind::Gpio],
            )
            .with_priority(DriverPriority::Preferred)
            .with_kind(DeviceKind::GpioLine)
            .with_standard_interaction(gpio::READ_INTERACTION, "Read the current GPIO level")
            .with_standard_interaction(
                gpio::GET_CONFIGURATION_INTERACTION,
                "Inspect the realized GPIO line configuration",
            )
            .with_rule(
                MatchRule::new(50)
                    .described("matches any GPIO line descriptor")
                    .require(MatchCondition::Kind(DeviceKind::GpioLine)),
            ),
        )
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        let session = context.open_gpio(self.id(), device, SessionAccess::SharedReadOnly)?;

        Ok(Box::new(ExampleGpioBoundDevice {
            driver_id: self.id().to_string(),
            session,
        }))
    }
}

struct ExampleGpioBoundDevice {
    driver_id: String,
    session: Box<dyn GpioSession>,
}

impl ExampleGpioBoundDevice {
    fn io(&mut self) -> GpioDeviceIo<'_> {
        let device_id = self.session.device().id.clone();
        GpioDeviceIo::with_device_id(self.session.as_mut(), self.driver_id.as_str(), device_id)
    }

    fn state_snapshot(&mut self) -> DriverResult<DeviceStateSnapshot> {
        let configuration = self.io().configuration()?;
        let level = self.io().read_level()?;

        Ok(DeviceStateSnapshot::new(self.session.device().id.clone())
            .with_config(
                CONFIG_DIRECTION,
                format!("{:?}", configuration.direction).to_lowercase(),
            )
            .with_config(CONFIG_ACTIVE_LOW, configuration.active_low)
            .with_telemetry(CONFIG_LEVEL, format!("{:?}", level).to_lowercase()))
    }
}

impl BoundDevice for ExampleGpioBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        self.session.device()
    }

    fn driver_id(&self) -> &str {
        self.driver_id.as_str()
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        Ok(Some(self.state_snapshot()?))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        request
            .validate()
            .map_err(|source| DriverError::InvalidRequest {
                driver_id: self.driver_id.clone(),
                device_id: self.session.device().id.clone(),
                source,
            })?;

        match request {
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)) => {
                let level = self.io().read_level()?;
                Ok(InteractionResponse::Standard(StandardResponse::Gpio(
                    GpioResponse::Level(level),
                )))
            }
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::GetConfiguration)) => {
                let configuration = self.io().configuration()?;
                Ok(InteractionResponse::Standard(StandardResponse::Gpio(
                    GpioResponse::Configuration(configuration),
                )))
            }
            _ => Err(DriverError::UnsupportedAction {
                driver_id: self.driver_id.clone(),
                device_id: self.session.device().id.clone(),
                action: interaction_name(request).into_owned(),
            }),
        }
    }
}

#[derive(Clone)]
struct FakeGpioBackend {
    descriptor: DeviceDescriptor,
    level: GpioLevel,
    configuration: GpioLineConfiguration,
}

impl BusBackend for FakeGpioBackend {
    fn name(&self) -> &str {
        "example-fake-gpio"
    }

    fn supported_interfaces(&self) -> &'static [InterfaceKind] {
        &[InterfaceKind::Gpio]
    }

    fn supports_device(&self, device: &DeviceDescriptor) -> bool {
        device.id == self.descriptor.id
    }
}

impl GpioBusBackend for FakeGpioBackend {
    fn open_gpio(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn GpioSession>> {
        if !self.supports_device(device) {
            return Err(BusError::UnsupportedDevice {
                backend: self.name().to_string(),
                device_id: device.id.clone(),
            });
        }

        Ok(Box::new(FakeGpioSession {
            descriptor: self.descriptor.clone(),
            metadata: SessionMetadata::new(self.name(), access).with_state(SessionState::Idle),
            level: self.level,
            configuration: self.configuration.clone(),
        }))
    }
}

struct FakeGpioSession {
    descriptor: DeviceDescriptor,
    metadata: SessionMetadata,
    level: GpioLevel,
    configuration: GpioLineConfiguration,
}

impl BusSession for FakeGpioSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.descriptor
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
        Ok(self.level)
    }

    fn write_level(&mut self, level: GpioLevel) -> BusResult<()> {
        self.level = level;
        Ok(())
    }

    fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> BusResult<()> {
        self.configuration = configuration.clone();
        Ok(())
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        Ok(self.configuration.clone())
    }
}

fn output_configuration() -> GpioLineConfiguration {
    GpioLineConfiguration {
        direction: GpioDirection::Input,
        active_low: false,
        bias: None,
        drive: None,
        edge: None,
        debounce_us: None,
        initial_level: Some(GpioLevel::High),
    }
}

fn example_device() -> DeviceDescriptor {
    DeviceDescriptor::builder_for_kind("example.gpio.line0", DeviceKind::GpioLine)
        .expect("example device builder")
        .display_name("status-line")
        .summary("Example GPIO line")
        .build()
        .expect("example device descriptor")
}

fn main() -> Result<(), Box<dyn Error>> {
    let descriptor = example_device();
    let backend = FakeGpioBackend {
        descriptor: descriptor.clone(),
        level: GpioLevel::High,
        configuration: output_configuration(),
    };
    let driver = ExampleGpioDriver;
    let bind_context = DriverBindContext::default().with_gpio(&backend);

    let mut bound = driver.bind(&descriptor, &bind_context)?;
    let response = bound.execute(&InteractionRequest::Standard(StandardRequest::Gpio(
        GpioRequest::Read,
    )))?;
    let state = bound.state()?.expect("state snapshot should exist");

    println!("response: {response:?}");
    println!("state: {state:?}");
    Ok(())
}
