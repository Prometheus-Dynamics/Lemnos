use lemnos::core::{
    ConfiguredGpioSignal, CustomInteractionResponse, DeviceDescriptor, DeviceKind,
    DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest, InteractionResponse,
    InterfaceKind, OperationRecord, OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverMatchLevel, DriverPriority, DriverResult, I2cControllerIo,
    I2cControllerSession, SessionAccess, interaction_name,
};
use lemnos::macros::ConfiguredDevice;
use std::borrow::Cow;

const BMI_IMU_SAMPLE_INTERACTION: &str = "sensor.imu.sample";

const BMI_ACC_CHIP_ID_REGISTER: u8 = 0x00;
const BMI_GYR_CHIP_ID_REGISTER: u8 = 0x00;
const BMI055_ACC_EXPECTED_ID: u8 = 0xFA;
const BMI088_ACC_EXPECTED_ID: u8 = 0x1E;
const BMI_GYR_EXPECTED_ID: u8 = 0x0F;
const BMI055_ACC_DATA_START: u8 = 0x02;
const BMI088_ACC_DATA_START: u8 = 0x12;
const BMI_GYR_DATA_START: u8 = 0x02;

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "validator.bmi055",
    driver = "validator.sensor.bmi055",
    summary = "Configured Bosch BMI055/BMI088 IMU validator"
)]
pub struct Bmi055Config {
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
struct Bmi055Binding {
    logical_device_id: String,
    bus: u32,
    accel_address: u16,
    gyro_address: u16,
    label: String,
    accel_int: Option<ConfiguredGpioSignal>,
    gyro_int: Option<ConfiguredGpioSignal>,
}

impl Bmi055Binding {
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

impl From<&Bmi055Config> for Bmi055Binding {
    fn from(value: &Bmi055Config) -> Self {
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

pub struct Bmi055Driver {
    bindings: Vec<Bmi055Binding>,
}

impl Bmi055Driver {
    const DRIVER_ID: &str = "validator.sensor.bmi055";

    pub fn from_configs(configs: impl IntoIterator<Item = Bmi055Config>) -> Self {
        Self {
            bindings: configs.into_iter().map(|config| (&config).into()).collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&Bmi055Binding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&Bmi055Binding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this Bosch IMU driver".into(),
            })
    }
}

impl Driver for Bmi055Driver {
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
                "Configured Bosch BMI055/BMI088 validator driver",
                vec![InterfaceKind::I2c],
            )
            .with_description("Board validator driver for a configured Bosch BMI055/BMI088 IMU.")
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
        {
            let mut io = Bmi055Io::new(
                self.id(),
                device.clone(),
                &mut *controller,
                binding.accel_address,
                binding.gyro_address,
            );
            io.verify_ids()?;
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

        Ok(Box::new(Bmi055BoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct Bmi055BoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: Bmi055Binding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoschImuVariant {
    Bmi055,
    Bmi088,
}

impl BoschImuVariant {
    fn name(self) -> &'static str {
        match self {
            Self::Bmi055 => "BMI055",
            Self::Bmi088 => "BMI088",
        }
    }

    fn accel_data_start(self) -> u8 {
        match self {
            Self::Bmi055 => BMI055_ACC_DATA_START,
            Self::Bmi088 => BMI088_ACC_DATA_START,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bmi055Sample {
    variant: BoschImuVariant,
    accel_raw: [i16; 3],
    gyro_raw: [i16; 3],
    accel_chip_id: u8,
    gyro_chip_id: u8,
}

impl Bmi055Sample {
    fn into_value(self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("label".into(), Value::from(label));
        map.insert("variant".into(), Value::from(self.variant.name()));
        map.insert(
            "accel_chip_id".into(),
            Value::from(u64::from(self.accel_chip_id)),
        );
        map.insert(
            "gyro_chip_id".into(),
            Value::from(u64::from(self.gyro_chip_id)),
        );
        map.insert(
            "accel_raw".into(),
            Value::from(
                self.accel_raw
                    .into_iter()
                    .map(|value| Value::from(i64::from(value)))
                    .collect::<Vec<_>>(),
            ),
        );
        map.insert(
            "gyro_raw".into(),
            Value::from(
                self.gyro_raw
                    .into_iter()
                    .map(|value| Value::from(i64::from(value)))
                    .collect::<Vec<_>>(),
            ),
        );
        Value::from(map)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry(
                "accel_raw",
                Value::from(
                    self.accel_raw
                        .into_iter()
                        .map(|value| Value::from(i64::from(value)))
                        .collect::<Vec<_>>(),
                ),
            )
            .with_telemetry(
                "gyro_raw",
                Value::from(
                    self.gyro_raw
                        .into_iter()
                        .map(|value| Value::from(i64::from(value)))
                        .collect::<Vec<_>>(),
                ),
            )
            .with_telemetry("accel_chip_id", u64::from(self.accel_chip_id))
            .with_telemetry("gyro_chip_id", u64::from(self.gyro_chip_id))
            .with_telemetry("variant", self.variant.name())
    }
}

struct Bmi055Io<'a> {
    driver_id: &'a str,
    device: DeviceDescriptor,
    controller: &'a mut dyn I2cControllerSession,
    accel_address: u16,
    gyro_address: u16,
}

impl<'a> Bmi055Io<'a> {
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

    fn verify_ids(&mut self) -> DriverResult<()> {
        self.detect_variant().map(|_| ())
    }

    fn sample(&mut self) -> DriverResult<Bmi055Sample> {
        let variant = self.detect_variant()?;
        let (accel_chip_id, gyro_chip_id) = self.read_ids()?;
        let accel_raw = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut accel = bus.target(self.accel_address);
            let bytes = accel.read_exact_block::<6>(variant.accel_data_start())?;
            decode_accel_bytes(variant, bytes)
        };
        let gyro_raw = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut gyro = bus.target(self.gyro_address);
            let bytes = gyro.read_exact_block::<6>(BMI_GYR_DATA_START)?;
            [
                i16::from_le_bytes([bytes[0], bytes[1]]),
                i16::from_le_bytes([bytes[2], bytes[3]]),
                i16::from_le_bytes([bytes[4], bytes[5]]),
            ]
        };

        Ok(Bmi055Sample {
            variant,
            accel_raw,
            gyro_raw,
            accel_chip_id,
            gyro_chip_id,
        })
    }

    fn detect_variant(&mut self) -> DriverResult<BoschImuVariant> {
        let (accel_id, gyro_id) = self.read_ids()?;
        match (accel_id, gyro_id) {
            (BMI055_ACC_EXPECTED_ID, BMI_GYR_EXPECTED_ID) => Ok(BoschImuVariant::Bmi055),
            (BMI088_ACC_EXPECTED_ID, BMI_GYR_EXPECTED_ID) => Ok(BoschImuVariant::Bmi088),
            _ => Err(self.bind_error(format!(
                "unexpected Bosch IMU chip ids accel=0x{accel_id:02x}, gyro=0x{gyro_id:02x} (expected BMI055 0x{BMI055_ACC_EXPECTED_ID:02x}/0x{BMI_GYR_EXPECTED_ID:02x} or BMI088 0x{BMI088_ACC_EXPECTED_ID:02x}/0x{BMI_GYR_EXPECTED_ID:02x})"
            ))),
        }
    }

    fn read_ids(&mut self) -> DriverResult<(u8, u8)> {
        let accel_id = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut accel = bus.target(self.accel_address);
            accel.read_u8(BMI_ACC_CHIP_ID_REGISTER)?
        };
        let gyro_id = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut gyro = bus.target(self.gyro_address);
            gyro.read_u8(BMI_GYR_CHIP_ID_REGISTER)?
        };
        Ok((accel_id, gyro_id))
    }

    fn bind_error(&self, reason: String) -> DriverError {
        DriverError::BindRejected {
            driver_id: self.driver_id.to_string(),
            device_id: self.device.id.clone(),
            reason,
        }
    }
}

impl BoundDevice for Bmi055BoundDevice {
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
        let sample = {
            let mut io = Bmi055Io::new(
                &self.driver_id,
                self.device.clone(),
                &mut *self.controller,
                self.binding.accel_address,
                self.binding.gyro_address,
            );
            io.sample()?
        };
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
                let sample = {
                    let mut io = Bmi055Io::new(
                        &self.driver_id,
                        self.device.clone(),
                        &mut *self.controller,
                        self.binding.accel_address,
                        self.binding.gyro_address,
                    );
                    io.sample()?
                };
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

fn decode_accel_bytes(variant: BoschImuVariant, bytes: [u8; 6]) -> [i16; 3] {
    match variant {
        BoschImuVariant::Bmi055 => [
            i16::from_le_bytes([bytes[0], bytes[1]]) >> 4,
            i16::from_le_bytes([bytes[2], bytes[3]]) >> 4,
            i16::from_le_bytes([bytes[4], bytes[5]]) >> 4,
        ],
        BoschImuVariant::Bmi088 => [
            i16::from_le_bytes([bytes[0], bytes[1]]),
            i16::from_le_bytes([bytes[2], bytes[3]]),
            i16::from_le_bytes([bytes[4], bytes[5]]),
        ],
    }
}
