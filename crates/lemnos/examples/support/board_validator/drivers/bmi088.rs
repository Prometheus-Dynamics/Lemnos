use lemnos::core::{
    ConfiguredGpioSignal, CustomInteractionResponse, DeviceDescriptor, DeviceKind,
    DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest, InteractionResponse,
    InterfaceKind, OperationRecord, OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverMatchLevel, DriverPriority, DriverResult, I2cControllerSession,
    SessionAccess, interaction_name,
};
use lemnos::macros::ConfiguredDevice;
use lemnos_bus::hal::HalI2cBus;
use lemnos_drivers_bmi088::{ACCEL_CHIP_ID, Bmi088, Config, GYRO_CHIP_ID};
use lemnos_hal::HalError;
use std::borrow::Cow;

const BMI_IMU_SAMPLE_INTERACTION: &str = "sensor.imu.sample";

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "validator.bmi088",
    driver = "validator.sensor.bmi088",
    summary = "Configured Bosch BMI088 IMU validator"
)]
pub struct Bmi088Config {
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
    #[lemnos(signal(gpio, name = "gyro_int"))]
    gyro_int: Option<ConfiguredGpioSignal>,
}

#[derive(Debug, Clone, PartialEq)]
struct Bmi088Binding {
    logical_device_id: String,
    bus: u32,
    accel_address: u16,
    gyro_address: u16,
    label: String,
    accel_int: Option<ConfiguredGpioSignal>,
    gyro_int: Option<ConfiguredGpioSignal>,
}

impl Bmi088Binding {
    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config("label", self.label.clone())
            .with_config("bus", u64::from(self.bus))
            .with_config("accel_address", u64::from(self.accel_address))
            .with_config("gyro_address", u64::from(self.gyro_address))
            .with_config("accel_interrupt_wired", self.accel_int.is_some())
            .with_config("gyro_interrupt_wired", self.gyro_int.is_some())
    }
}

impl From<&Bmi088Config> for Bmi088Binding {
    fn from(value: &Bmi088Config) -> Self {
        Self {
            logical_device_id: value.configured_device_id(),
            bus: value.bus,
            accel_address: value.accel_address,
            gyro_address: value.gyro_address,
            label: value.label.clone(),
            accel_int: value.accel_int.clone(),
            gyro_int: value.gyro_int.clone(),
        }
    }
}

pub struct Bmi088Driver {
    bindings: Vec<Bmi088Binding>,
}

impl Bmi088Driver {
    const DRIVER_ID: &str = "validator.sensor.bmi088";

    pub fn from_configs(configs: impl IntoIterator<Item = Bmi088Config>) -> Self {
        Self {
            bindings: configs.into_iter().map(|config| (&config).into()).collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&Bmi088Binding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&Bmi088Binding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this Bosch IMU driver".into(),
            })
    }
}

impl Driver for Bmi088Driver {
    fn id(&self) -> &str {
        Self::DRIVER_ID
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                self.id(),
                "Configured Bosch BMI088 validator driver",
                vec![InterfaceKind::I2c],
            )
            .with_description(
                "Board validator driver for a configured Bosch BMI088 IMU. Read-only: it checks \
                 the chip IDs and reads raw counts, and never resets or reconfigures the chip.",
            )
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .with_custom_interaction(
                BMI_IMU_SAMPLE_INTERACTION,
                "Read a raw accel + gyro sample from the configured Bosch IMU",
            )
            .with_tag("validator")
            .with_tag("sensor")
            .with_tag("imu")
            .with_tag("bosch"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        let base: DriverMatch = self.manifest_ref().match_device(device).into();
        if !base.is_supported() {
            return base;
        }

        let Some(binding) = self.binding_for(device) else {
            return DriverMatch::unsupported(
                "device is not listed in the configured Bosch IMU binding set",
            );
        };

        DriverMatch {
            level: DriverMatchLevel::Exact,
            score: base.score + 500,
            reasons: vec![format!(
                "configured Bosch IMU '{}' matched logical device '{}'",
                binding.label, binding.logical_device_id
            )],
            matched_rule: base.matched_rule,
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
        let (accel_id, gyro_id) = {
            let mut io = Bmi088Io::new(self.id(), device, &mut *controller, &binding);
            io.chip_ids()?
        };
        if (accel_id, gyro_id) != (ACCEL_CHIP_ID, GYRO_CHIP_ID) {
            return Err(DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: format!(
                    "unexpected BMI088 chip ids accel=0x{accel_id:02x}, gyro=0x{gyro_id:02x} \
                     (expected 0x{ACCEL_CHIP_ID:02x}/0x{GYRO_CHIP_ID:02x})"
                ),
            });
        }

        let interaction = CustomInteraction::new(
            BMI_IMU_SAMPLE_INTERACTION,
            "Read raw accel + gyro values from the configured Bosch IMU",
        )
        .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(Bmi088BoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct Bmi088BoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: Bmi088Binding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bmi088Sample {
    accel_raw: [i16; 3],
    gyro_raw: [i16; 3],
    accel_chip_id: u8,
    gyro_chip_id: u8,
}

impl Bmi088Sample {
    fn into_value(self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("label".into(), Value::from(label));
        map.insert("variant".into(), Value::from("BMI088"));
        map.insert(
            "accel_chip_id".into(),
            Value::from(u64::from(self.accel_chip_id)),
        );
        map.insert(
            "gyro_chip_id".into(),
            Value::from(u64::from(self.gyro_chip_id)),
        );
        map.insert("accel_raw".into(), raw_axes(self.accel_raw));
        map.insert("gyro_raw".into(), raw_axes(self.gyro_raw));
        Value::from(map)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry("accel_raw", raw_axes(self.accel_raw))
            .with_telemetry("gyro_raw", raw_axes(self.gyro_raw))
            .with_telemetry("accel_chip_id", u64::from(self.accel_chip_id))
            .with_telemetry("gyro_chip_id", u64::from(self.gyro_chip_id))
            .with_telemetry("variant", "BMI088")
    }
}

/// A failed chip operation, with the chip layer's error kind.
fn device_error<E: HalError>(
    driver_id: &str,
    device: &DeviceDescriptor,
    action: &str,
    error: &E,
) -> DriverError {
    DriverError::Device {
        driver_id: driver_id.to_string(),
        device_id: device.id.clone(),
        action: action.to_string(),
        kind: error.kind(),
    }
}

fn raw_axes(axes: [i16; 3]) -> Value {
    Value::from(
        axes.into_iter()
            .map(|value| Value::from(i64::from(value)))
            .collect::<Vec<_>>(),
    )
}

/// The chip layer: `lemnos-drivers-bmi088` over the session, one short
/// borrow per operation. It attaches to a chip that is already configured
/// (`Bmi088::resume`), so a validator run never resets or reconfigures it.
struct Bmi088Io<'a> {
    driver_id: &'a str,
    device: &'a DeviceDescriptor,
    controller: &'a mut dyn I2cControllerSession,
    binding: &'a Bmi088Binding,
}

impl<'a> Bmi088Io<'a> {
    fn new(
        driver_id: &'a str,
        device: &'a DeviceDescriptor,
        controller: &'a mut dyn I2cControllerSession,
        binding: &'a Bmi088Binding,
    ) -> Self {
        Self {
            driver_id,
            device,
            controller,
            binding,
        }
    }

    fn imu(&mut self) -> Bmi088<HalI2cBus<'_, dyn I2cControllerSession + '_>> {
        Bmi088::resume(
            HalI2cBus::new(&mut *self.controller),
            self.binding.accel_address as u8,
            self.binding.gyro_address as u8,
            Config::default(),
        )
    }

    fn chip_ids(&mut self) -> DriverResult<(u8, u8)> {
        let (driver_id, device) = (self.driver_id, self.device);
        self.imu()
            .chip_ids()
            .map_err(|e| device_error(driver_id, device, "read the BMI088 chip ids", &e))
    }

    fn sample(&mut self) -> DriverResult<Bmi088Sample> {
        let (accel_chip_id, gyro_chip_id) = self.chip_ids()?;
        let (driver_id, device) = (self.driver_id, self.device);
        let mut imu = self.imu();
        let accel_raw = imu
            .read_accel_raw()
            .map_err(|e| device_error(driver_id, device, "read the BMI088 accelerometer", &e))?;
        let gyro_raw = imu
            .read_gyro_raw()
            .map_err(|e| device_error(driver_id, device, "read the BMI088 gyroscope", &e))?;
        Ok(Bmi088Sample {
            accel_raw,
            gyro_raw,
            accel_chip_id,
            gyro_chip_id,
        })
    }
}

impl BoundDevice for Bmi088BoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        &self.driver_id
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        &self.interactions
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        let sample = Bmi088Io::new(
            &self.driver_id,
            &self.device,
            &mut *self.controller,
            &self.binding,
        )
        .sample()?;
        let state = self.binding.apply_state_config(
            DeviceStateSnapshot::new(self.device.id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle),
        );
        let output = sample.into_value(&self.binding.label);
        Ok(Some(
            sample.apply_telemetry(state).with_last_operation(
                OperationRecord::new(BMI_IMU_SAMPLE_INTERACTION, OperationStatus::Succeeded)
                    .with_output(output),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(request)
                if request.id.as_str() == BMI_IMU_SAMPLE_INTERACTION =>
            {
                let sample = Bmi088Io::new(
                    &self.driver_id,
                    &self.device,
                    &mut *self.controller,
                    &self.binding,
                )
                .sample()?;
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
