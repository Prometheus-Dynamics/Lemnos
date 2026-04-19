use crate::core::{
    ConfiguredGpioSignal, CustomInteractionResponse, DeviceDescriptor, DeviceKind,
    DeviceLifecycleState, DeviceStateSnapshot, GpioEdge, GpioLevel, GpioLineConfiguration,
    InteractionRequest, InteractionResponse, InterfaceKind, OperationRecord, OperationStatus,
    PwmConfiguration, PwmPolarity, PwmRequest, PwmResponse, StandardRequest, StandardResponse,
    Value, ValueMap,
};
use crate::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverMatchLevel, DriverPriority, DriverResult, I2cControllerIo,
    I2cControllerSession, SessionAccess, interaction_name,
};
#[cfg(feature = "linux")]
use crate::linux::{LinuxPaths, LinuxTransportConfig};
use crate::macros::ConfiguredDevice;
use crate::mock::{MockGpioLine, MockHardware, MockI2cDevice, MockPwmChannel, MockUsbDevice};
use crate::prelude::*;
use crate::{BuiltInDriverBundle, Lemnos};
use lemnos_runtime::Runtime;
use std::borrow::Cow;
use std::sync::Arc;

mod builder;

const COMPOSITE_SAMPLE_INTERACTION: &str = "sensor.composite.sample";
const COMPOSITE_ACCEL_CHIP_ID_REGISTER: u8 = 0x00;
const COMPOSITE_ACCEL_EXPECTED_ID: u8 = 0x1E;
const COMPOSITE_GYRO_CHIP_ID_REGISTER: u8 = 0x00;
const COMPOSITE_GYRO_EXPECTED_ID: u8 = 0x0F;

fn output_config() -> GpioLineConfiguration {
    GpioLineConfiguration {
        direction: GpioDirection::Output,
        active_low: false,
        bias: None,
        drive: None,
        edge: None,
        debounce_us: None,
        initial_level: Some(GpioLevel::Low),
    }
}

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "test.composite-imu",
    driver = "test.sensor.composite-imu",
    summary = "Configured composite IMU for facade tests"
)]
struct CompositeImuConfig {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "accel"))]
    accel_address: u16,
    #[lemnos(endpoint(i2c, name = "gyro"))]
    gyro_address: u16,
    #[lemnos(display_name, label)]
    label: String,
    #[lemnos(signal(gpio, name = "accel_int"))]
    accel_int: Option<ConfiguredGpioSignal>,
}

#[derive(Debug, Clone, PartialEq)]
struct CompositeImuBinding {
    logical_device_id: String,
    bus: u32,
    accel_address: u16,
    gyro_address: u16,
    label: String,
    accel_int: Option<ConfiguredGpioSignal>,
}

impl CompositeImuBinding {
    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config("label", self.label.clone())
            .with_config("bus", u64::from(self.bus))
            .with_config("accel_address", u64::from(self.accel_address))
            .with_config("gyro_address", u64::from(self.gyro_address))
            .with_config("accel_interrupt_wired", self.accel_int.is_some())
    }
}

impl From<&CompositeImuConfig> for CompositeImuBinding {
    fn from(value: &CompositeImuConfig) -> Self {
        Self {
            logical_device_id: value.configured_device_id(),
            bus: value.bus,
            accel_address: value.accel_address,
            gyro_address: value.gyro_address,
            label: value.label.clone(),
            accel_int: value.accel_int.clone(),
        }
    }
}

struct CompositeImuDriver {
    bindings: Vec<CompositeImuBinding>,
}

impl CompositeImuDriver {
    fn new(bindings: impl IntoIterator<Item = CompositeImuBinding>) -> Self {
        Self {
            bindings: bindings.into_iter().collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&CompositeImuBinding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&CompositeImuBinding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this composite IMU driver".into(),
            })
    }
}

impl Driver for CompositeImuDriver {
    fn id(&self) -> &str {
        "test.sensor.composite-imu"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                self.id(),
                "Configured composite IMU driver for facade tests",
                vec![InterfaceKind::I2c],
            )
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .with_custom_interaction(
                COMPOSITE_SAMPLE_INTERACTION,
                "Read the accel and gyro chip ids",
            )
            .with_tag("test")
            .with_tag("composite")
            .with_tag("imu"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        let base: DriverMatch = self.manifest_ref().match_device(device).into();
        if !base.is_supported() {
            return base;
        }

        if let Some(binding) = self.binding_for(device) {
            DriverMatch {
                level: DriverMatchLevel::Exact,
                score: base.score + 500,
                reasons: vec![format!(
                    "configured composite IMU '{}' matched logical device '{}'",
                    binding.label, binding.logical_device_id
                )],
                matched_rule: base.matched_rule,
            }
        } else {
            DriverMatch::unsupported("device is not listed in the composite IMU binding set")
        }
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        let binding = self.binding_for_device(device)?.clone();
        let mut controller = context.open_i2c_controller(
            self.id(),
            device,
            binding.bus,
            SessionAccess::ExclusiveController,
        )?;
        {
            let mut io = CompositeImuIo::new(
                self.id(),
                device.clone(),
                &mut *controller,
                binding.accel_address,
                binding.gyro_address,
            );
            io.verify_ids()?;
        }

        let interaction = CustomInteraction::new(
            COMPOSITE_SAMPLE_INTERACTION,
            "Read the accel and gyro chip ids",
        )
        .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(CompositeImuBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct CompositeImuBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: CompositeImuBinding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompositeImuSample {
    accel_id: u8,
    gyro_id: u8,
}

impl CompositeImuSample {
    fn into_value(self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("label".into(), Value::from(label));
        map.insert("accel_id".into(), Value::from(u64::from(self.accel_id)));
        map.insert("gyro_id".into(), Value::from(u64::from(self.gyro_id)));
        Value::from(map)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry("accel_id", u64::from(self.accel_id))
            .with_telemetry("gyro_id", u64::from(self.gyro_id))
    }
}

struct CompositeImuIo<'a> {
    driver_id: &'a str,
    device: DeviceDescriptor,
    controller: &'a mut dyn I2cControllerSession,
    accel_address: u16,
    gyro_address: u16,
}

impl<'a> CompositeImuIo<'a> {
    fn new(
        driver_id: &'a str,
        device: DeviceDescriptor,
        controller: &'a mut dyn I2cControllerSession,
        accel_address: u16,
        gyro_address: u16,
    ) -> Self {
        Self {
            driver_id,
            device,
            controller,
            accel_address,
            gyro_address,
        }
    }

    fn read_ids(&mut self) -> DriverResult<CompositeImuSample> {
        let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
        let accel_id = bus
            .target(self.accel_address)
            .read_u8(COMPOSITE_ACCEL_CHIP_ID_REGISTER)?;
        let gyro_id = bus
            .target(self.gyro_address)
            .read_u8(COMPOSITE_GYRO_CHIP_ID_REGISTER)?;

        Ok(CompositeImuSample { accel_id, gyro_id })
    }

    fn verify_ids(&mut self) -> DriverResult<()> {
        let sample = self.read_ids()?;
        if sample.accel_id != COMPOSITE_ACCEL_EXPECTED_ID {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device.id.clone(),
                reason: format!("unexpected accel chip id 0x{:02x}", sample.accel_id),
            });
        }
        if sample.gyro_id != COMPOSITE_GYRO_EXPECTED_ID {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device.id.clone(),
                reason: format!("unexpected gyro chip id 0x{:02x}", sample.gyro_id),
            });
        }
        Ok(())
    }
}

impl CompositeImuBoundDevice {
    fn io(&mut self) -> CompositeImuIo<'_> {
        CompositeImuIo::new(
            &self.driver_id,
            self.device.clone(),
            &mut *self.controller,
            self.binding.accel_address,
            self.binding.gyro_address,
        )
    }
}

impl BoundDevice for CompositeImuBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        &self.driver_id
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        self.interactions.as_slice()
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        let sample = self.io().read_ids()?;
        let state = self.binding.apply_state_config(
            DeviceStateSnapshot::new(self.device.id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle),
        );
        Ok(Some(
            sample.apply_telemetry(state).with_last_operation(
                OperationRecord::new(COMPOSITE_SAMPLE_INTERACTION, OperationStatus::Succeeded)
                    .with_output(sample.into_value(&self.binding.label)),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(request)
                if request.id.as_str() == COMPOSITE_SAMPLE_INTERACTION =>
            {
                let sample = self.io().read_ids()?;
                Ok(InteractionResponse::Custom(
                    CustomInteractionResponse::new(self.interactions[0].id.clone())
                        .with_output(sample.into_value(&self.binding.label)),
                ))
            }
            _ => Err(DriverError::UnsupportedAction {
                driver_id: self.driver_id.clone(),
                device_id: self.device.id.clone(),
                action: interaction_name(request).into_owned(),
            }),
        }
    }
}

fn composite_ids_from_response(response: &DeviceResponse) -> (u8, u8) {
    let InteractionResponse::Custom(custom) = &response.interaction else {
        panic!("expected custom response, got {:?}", response.interaction);
    };
    let Some(Value::Map(output)) = &custom.output else {
        panic!("expected custom output map, got {:?}", custom.output);
    };
    let accel_id = output
        .get("accel_id")
        .and_then(Value::as_u64)
        .expect("accel id") as u8;
    let gyro_id = output
        .get("gyro_id")
        .and_then(Value::as_u64)
        .expect("gyro id") as u8;
    (accel_id, gyro_id)
}

#[test]
fn builder_with_mock_hardware_and_builtin_drivers_handles_requests() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 21)
                .with_line_name("status")
                .with_configuration(output_config()),
        )
        .build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("register built-in drivers")
        .build();

    lemnos
        .refresh_with_mock_default(&hardware)
        .expect("refresh mock inventory");

    let device_id = lemnos
        .inventory()
        .first_id_by_kind(DeviceKind::GpioLine)
        .expect("gpio line");

    let response = lemnos
        .write_gpio(device_id.clone(), GpioLevel::High)
        .expect("write request");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(GpioResponse::Applied))
    );
    assert_eq!(hardware.gpio_level(&device_id), Some(GpioLevel::High));
}

#[test]
fn facade_simple_request_helpers_cover_common_gpio_pwm_and_usb_flows() {
    let usb = MockUsbDevice::new(1, [4]).with_interface(0);
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 7)
                .with_line_name("status")
                .with_configuration(output_config()),
        )
        .with_pwm_channel(
            MockPwmChannel::new("pwmchip0", 0).with_configuration(PwmConfiguration {
                period_ns: 20_000_000,
                duty_cycle_ns: 5_000_000,
                enabled: false,
                polarity: PwmPolarity::Normal,
            }),
        )
        .with_usb_device(usb)
        .build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("register built-in drivers")
        .build();

    lemnos
        .refresh_with_mock_default(&hardware)
        .expect("refresh mock inventory");

    let gpio_id = lemnos
        .inventory()
        .first_id_by_kind(DeviceKind::GpioLine)
        .expect("gpio line");
    let pwm_id = lemnos
        .inventory()
        .first_id_by_kind(DeviceKind::PwmChannel)
        .expect("pwm channel");
    let usb_id = lemnos
        .inventory()
        .first_id_by_kind(DeviceKind::UsbInterface)
        .expect("usb interface");

    let gpio_config = lemnos
        .gpio_configuration(gpio_id.clone())
        .expect("gpio configuration");
    assert!(matches!(
        gpio_config.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(
            lemnos_core::GpioResponse::Configuration(_)
        ))
    ));

    let gpio_read = lemnos.read_gpio(gpio_id.clone()).expect("gpio read");
    assert!(matches!(
        gpio_read.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(lemnos_core::GpioResponse::Level(
            GpioLevel::Low
        )))
    ));

    lemnos
        .write_gpio(gpio_id.clone(), GpioLevel::High)
        .expect("gpio write");
    assert_eq!(hardware.gpio_level(&gpio_id), Some(GpioLevel::High));

    let pwm_config = lemnos
        .pwm_configuration(pwm_id.clone())
        .expect("pwm configuration");
    assert!(matches!(
        pwm_config.interaction,
        InteractionResponse::Standard(StandardResponse::Pwm(PwmResponse::Configuration(_)))
    ));

    lemnos.enable_pwm(pwm_id.clone(), true).expect("enable pwm");
    lemnos
        .set_pwm_duty_cycle(pwm_id.clone(), 10_000_000)
        .expect("set duty cycle");
    assert_eq!(
        hardware
            .pwm_configuration(&pwm_id)
            .expect("pwm configuration"),
        PwmConfiguration {
            period_ns: 20_000_000,
            duty_cycle_ns: 10_000_000,
            enabled: true,
            polarity: PwmPolarity::Normal,
        }
    );

    let claim = lemnos
        .claim_usb_interface(usb_id.clone(), 0, None)
        .expect("claim usb interface");
    assert!(matches!(
        claim.interaction,
        InteractionResponse::Standard(StandardResponse::Usb(
            lemnos_core::UsbResponse::InterfaceClaimed {
                interface_number: 0,
                alternate_setting: None,
            }
        ))
    ));
    let release = lemnos
        .release_usb_interface(usb_id, 0)
        .expect("release usb interface");
    assert!(matches!(
        release.interaction,
        InteractionResponse::Standard(StandardResponse::Usb(
            lemnos_core::UsbResponse::InterfaceReleased {
                interface_number: 0,
            }
        ))
    ));
}

#[test]
fn facade_exposes_typed_driver_id_preferences() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 31)
                .with_line_name("status")
                .with_configuration(output_config()),
        )
        .build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("register built-in drivers")
        .build();

    lemnos
        .refresh_with_mock_default(&hardware)
        .expect("refresh mock inventory");

    let device_id = lemnos
        .inventory()
        .first_id_by_kind(DeviceKind::GpioLine)
        .expect("gpio line");

    let driver_id = DriverId::from("lemnos.gpio.generic");
    lemnos
        .prefer_driver_id_for_device(device_id.clone(), driver_id.clone())
        .expect("set preferred driver");

    assert_eq!(
        lemnos.preferred_driver_id_for_device(&device_id),
        Some(&driver_id)
    );
    assert_eq!(
        lemnos.clear_preferred_driver_id_for_device(&device_id),
        Some(driver_id.clone())
    );
    assert_eq!(lemnos.preferred_driver_id_for_device(&device_id), None);
    assert_eq!(lemnos.preferred_driver_for_device(&device_id), None);
}

#[test]
fn facade_refresh_state_shared_reuses_cached_snapshot_arc() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 32)
                .with_line_name("status")
                .with_configuration(output_config()),
        )
        .build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("register built-in drivers")
        .build();

    lemnos
        .refresh_with_mock_default(&hardware)
        .expect("refresh mock inventory");

    let device_id = lemnos
        .inventory()
        .first_id_by_kind(DeviceKind::GpioLine)
        .expect("gpio line");

    lemnos.bind(&device_id).expect("bind gpio");
    let refreshed = lemnos
        .refresh_state_shared(&device_id)
        .expect("refresh shared state")
        .expect("shared state");
    let cached = lemnos
        .shared_state(&device_id)
        .expect("cached shared state");

    assert!(Arc::ptr_eq(&refreshed, &cached));
}

#[test]
fn builtin_bundle_registers_into_runtime_directly() {
    let mut runtime = Runtime::new();
    BuiltInDriverBundle::register_into(&mut runtime).expect("register built-in drivers");

    let hardware = MockHardware::builder()
        .with_pwm_channel(
            MockPwmChannel::new("pwmchip0", 0).with_configuration(PwmConfiguration {
                period_ns: 20_000_000,
                duty_cycle_ns: 5_000_000,
                enabled: false,
                polarity: PwmPolarity::Normal,
            }),
        )
        .build();
    runtime.set_pwm_backend(hardware.clone());
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh mock inventory");

    let device_id = runtime
        .inventory()
        .by_kind(DeviceKind::PwmChannel)
        .into_iter()
        .next()
        .expect("pwm channel")
        .id
        .clone();

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Pwm(PwmRequest::Enable {
                enabled: true,
            })),
        ))
        .expect("enable request");

    assert_eq!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Pwm(PwmResponse::Applied))
    );
    assert_eq!(
        hardware
            .pwm_configuration(&device_id)
            .expect("pwm configuration"),
        PwmConfiguration {
            period_ns: 20_000_000,
            duty_cycle_ns: 5_000_000,
            enabled: true,
            polarity: PwmPolarity::Normal,
        }
    );
}

#[test]
fn facade_handles_configured_composite_device_transport_churn() {
    let accel_interrupt = ConfiguredGpioSignal::by_device_id("mock.gpio.board-imu.5")
        .expect("interrupt signal")
        .with_edge(GpioEdge::Rising);
    let config = CompositeImuConfig::builder()
        .bus(4_u32)
        .accel_address(0x18_u16)
        .gyro_address(0x68_u16)
        .label("board-imu")
        .accel_int(accel_interrupt)
        .build()
        .expect("build composite config");
    let probe =
        CompositeImuConfig::configured_probe("test-configured-composite-imu", vec![config.clone()]);

    let accel = MockI2cDevice::new(config.bus, config.accel_address).with_u8(0x00, 0x1E);
    let accel_id = accel.descriptor().id.clone();
    let gyro = MockI2cDevice::new(config.bus, config.gyro_address).with_u8(0x00, 0x0F);
    let gyro_id = gyro.descriptor().id.clone();
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("board-imu", 5).with_line_name("imu-int"))
        .with_i2c_device(accel)
        .with_i2c_device(gyro)
        .build();

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_driver(CompositeImuDriver::new([CompositeImuBinding::from(
            &config,
        )]))
        .expect("register composite driver")
        .build();

    lemnos
        .refresh(&DiscoveryContext::new(), &[&hardware, &probe])
        .expect("initial refresh");

    let logical_id = config.logical_device_id().expect("logical device id");
    let configured_child_ids = config
        .configured_child_descriptors()
        .expect("configured child descriptors")
        .into_iter()
        .map(|device| device.id)
        .collect::<Vec<_>>();

    lemnos.bind(&logical_id).expect("bind logical device");
    let response = lemnos
        .request_custom(logical_id.clone(), COMPOSITE_SAMPLE_INTERACTION)
        .expect("initial sample");
    assert_eq!(
        composite_ids_from_response(&response),
        (COMPOSITE_ACCEL_EXPECTED_ID, COMPOSITE_GYRO_EXPECTED_ID)
    );
    let initial_state = lemnos
        .refresh_state(&logical_id)
        .expect("refresh state")
        .cloned()
        .expect("state snapshot");
    assert_eq!(
        initial_state
            .realized_config
            .get("accel_interrupt_wired")
            .and_then(Value::as_bool),
        Some(true)
    );

    assert!(hardware.remove_device(&gyro_id), "remove gyro device");
    lemnos
        .refresh_incremental(&DiscoveryContext::new(), &[&hardware])
        .expect("incremental refresh after gyro removal");

    assert!(lemnos.inventory().get(&logical_id).is_some());
    assert!(lemnos.inventory().get(&accel_id).is_some());
    assert!(lemnos.inventory().get(&gyro_id).is_none());
    for child_id in &configured_child_ids {
        assert!(
            lemnos.inventory().get(child_id).is_some(),
            "configured child descriptor {child_id} should survive hardware-only refresh"
        );
    }

    let error = lemnos
        .request_custom(logical_id.clone(), COMPOSITE_SAMPLE_INTERACTION)
        .expect_err("sample should fail while gyro transport is missing");
    let message = error.to_string();
    assert!(message.contains("0x68"), "unexpected error: {message}");

    hardware
        .attach_i2c_device(MockI2cDevice::new(config.bus, config.gyro_address).with_u8(0x00, 0x0F));
    lemnos
        .refresh_incremental(&DiscoveryContext::new(), &[&hardware])
        .expect("incremental refresh after gyro reattach");

    let recovered = lemnos
        .request_custom(logical_id.clone(), COMPOSITE_SAMPLE_INTERACTION)
        .expect("sample after recovery");
    assert_eq!(
        composite_ids_from_response(&recovered),
        (COMPOSITE_ACCEL_EXPECTED_ID, COMPOSITE_GYRO_EXPECTED_ID)
    );
}

#[cfg(feature = "linux")]
#[test]
fn facade_exposes_linux_backend_configuration_helpers() {
    let paths = LinuxPaths::default();
    let transport_config = LinuxTransportConfig::new()
        .with_uart_default_baud_rate(57_600)
        .with_uart_timeout_ms(125)
        .with_usb_timeout_ms(250);

    let _ = Lemnos::builder()
        .with_linux_paths(paths.clone())
        .with_linux_transport_config(transport_config)
        .with_linux_paths_and_config(paths.clone(), transport_config)
        .build();

    let mut lemnos = Lemnos::new();
    lemnos.set_linux_paths(paths.clone());
    lemnos.set_linux_transport_config(transport_config);
    lemnos.set_linux_paths_and_config(paths, transport_config);
}
