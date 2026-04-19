use lemnos::core::{
    CustomInteractionResponse, DeviceDescriptor, DeviceKind, DeviceLifecycleState,
    DeviceStateSnapshot, InteractionRequest, InteractionResponse, InterfaceKind, OperationRecord,
    OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverMatchLevel, DriverPriority, DriverResult, I2cControllerIo,
    I2cControllerSession, SessionAccess, interaction_name,
};
use lemnos::macros::ConfiguredDevice;
use std::borrow::Cow;
use std::thread;
use std::time::Duration;

const BMM150_SAMPLE_INTERACTION: &str = "sensor.magnetometer.sample";
const BMM150_CHIP_ID_REGISTER: u8 = 0x40;
const BMM150_EXPECTED_ID: u8 = 0x32;
const BMM150_POWER_CONTROL_REGISTER: u8 = 0x4B;
const BMM150_OP_MODE_REGISTER: u8 = 0x4C;
const BMM150_AXES_ENABLE_REGISTER: u8 = 0x4E;
const BMM150_REP_XY_REGISTER: u8 = 0x51;
const BMM150_REP_Z_REGISTER: u8 = 0x52;
const BMM150_DATA_REGISTER: u8 = 0x42;

#[derive(Debug, Clone, PartialEq, ConfiguredDevice)]
#[lemnos(
    interface = I2c,
    id = "validator.bmm150",
    driver = "validator.sensor.bmm150",
    summary = "Configured BMM150 magnetometer validator"
)]
pub struct Bmm150Config {
    #[lemnos(bus(i2c))]
    bus: u32,
    #[lemnos(endpoint(i2c, name = "sensor"))]
    address: u16,
    #[lemnos(display_name, label)]
    label: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Bmm150Binding {
    logical_device_id: String,
    bus: u32,
    address: u16,
    label: String,
}

impl Bmm150Binding {
    fn apply_state_config(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_config("label", self.label.clone())
            .with_config("bus", u64::from(self.bus))
            .with_config("address", u64::from(self.address))
    }
}

impl From<&Bmm150Config> for Bmm150Binding {
    fn from(value: &Bmm150Config) -> Self {
        Self {
            logical_device_id: value.configured_device_id(),
            bus: value.bus,
            address: value.address,
            label: value.label.clone(),
        }
    }
}

pub struct Bmm150Driver {
    bindings: Vec<Bmm150Binding>,
}

impl Bmm150Driver {
    const DRIVER_ID: &str = "validator.sensor.bmm150";

    pub fn from_configs(configs: impl IntoIterator<Item = Bmm150Config>) -> Self {
        Self {
            bindings: configs.into_iter().map(|config| (&config).into()).collect(),
        }
    }

    fn binding_for(&self, device: &DeviceDescriptor) -> Option<&Bmm150Binding> {
        self.bindings
            .iter()
            .find(|binding| binding.logical_device_id == device.id.as_str())
    }

    fn binding_for_device(&self, device: &DeviceDescriptor) -> DriverResult<&Bmm150Binding> {
        self.binding_for(device)
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: "device is not configured for this BMM150 driver".into(),
            })
    }
}

impl Driver for Bmm150Driver {
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
                "Configured BMM150 validator driver",
                vec![InterfaceKind::I2c],
            )
            .with_description("Board validator driver for a configured BMM150 magnetometer.")
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::I2c))
            .with_custom_interaction(
                BMM150_SAMPLE_INTERACTION,
                "Read a raw magnetic sample from the BMM150",
            )
            .with_tag("validator")
            .with_tag("sensor")
            .with_tag("magnetometer")
            .with_tag("bmm150"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        let base: DriverMatch = self.manifest_ref().match_device(device).into();
        if !base.is_supported() {
            return base;
        }

        let Some(binding) = self.binding_for(device) else {
            return DriverMatch::unsupported("device is not listed in the BMM150 binding set");
        };

        DriverMatch {
            level: DriverMatchLevel::Exact,
            score: base.score + 450,
            reasons: vec![format!(
                "configured BMM150 '{}' matched logical device '{}'",
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
            let mut io =
                Bmm150Io::new(self.id(), device.clone(), &mut *controller, binding.address);
            io.power_up()?;
            io.verify_id()?;
            io.configure()?;
        }

        let interaction = CustomInteraction::new(
            BMM150_SAMPLE_INTERACTION,
            "Read raw x/y/z/rhall values from the BMM150",
        )
        .map_err(|source| DriverError::BindFailed {
            driver_id: self.id().to_string(),
            device_id: device.id.clone(),
            reason: source.to_string(),
        })?;

        Ok(Box::new(Bmm150BoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            controller,
            binding,
            interactions: vec![interaction],
        }))
    }
}

struct Bmm150BoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    binding: Bmm150Binding,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bmm150Sample {
    x_raw: i16,
    y_raw: i16,
    z_raw: i16,
    rhall_raw: u16,
}

impl Bmm150Sample {
    fn into_value(self, label: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("label".into(), Value::from(label));
        map.insert("x_raw".into(), Value::from(i64::from(self.x_raw)));
        map.insert("y_raw".into(), Value::from(i64::from(self.y_raw)));
        map.insert("z_raw".into(), Value::from(i64::from(self.z_raw)));
        map.insert("rhall_raw".into(), Value::from(u64::from(self.rhall_raw)));
        Value::from(map)
    }

    fn apply_telemetry(self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        state
            .with_telemetry("x_raw", i64::from(self.x_raw))
            .with_telemetry("y_raw", i64::from(self.y_raw))
            .with_telemetry("z_raw", i64::from(self.z_raw))
            .with_telemetry("rhall_raw", u64::from(self.rhall_raw))
    }
}

struct Bmm150Io<'a> {
    driver_id: &'a str,
    device: DeviceDescriptor,
    controller: &'a mut dyn I2cControllerSession,
    address: u16,
}

impl<'a> Bmm150Io<'a> {
    fn new(
        driver_id: &'a str,
        device: DeviceDescriptor,
        controller: &'a mut dyn I2cControllerSession,
        address: u16,
    ) -> Self {
        Self {
            driver_id,
            device,
            controller,
            address,
        }
    }

    fn verify_id(&mut self) -> DriverResult<()> {
        let chip_id = {
            let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
            let mut sensor = bus.target(self.address);
            sensor.read_u8(BMM150_CHIP_ID_REGISTER)?
        };
        if chip_id != BMM150_EXPECTED_ID {
            return Err(DriverError::BindRejected {
                driver_id: self.driver_id.to_string(),
                device_id: self.device.id.clone(),
                reason: format!(
                    "unexpected BMM150 chip id 0x{chip_id:02x} (expected 0x{BMM150_EXPECTED_ID:02x})"
                ),
            });
        }
        Ok(())
    }

    fn power_up(&mut self) -> DriverResult<()> {
        let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
        let mut sensor = bus.target(self.address);
        sensor.write_u8(BMM150_POWER_CONTROL_REGISTER, 0x01)?;
        thread::sleep(Duration::from_millis(2));
        Ok(())
    }

    fn configure(&mut self) -> DriverResult<()> {
        let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
        let mut sensor = bus.target(self.address);
        sensor.write_u8(BMM150_AXES_ENABLE_REGISTER, 0x00)?;
        sensor.write_u8(BMM150_REP_XY_REGISTER, 0x04)?;
        sensor.write_u8(BMM150_REP_Z_REGISTER, 0x07)?;
        let register = sensor.read_u8(BMM150_OP_MODE_REGISTER)?;
        let register = (register & !0x38_u8) | (0x07_u8 << 3);
        let register = register & !0x06_u8;
        sensor.write_u8(BMM150_OP_MODE_REGISTER, register)?;
        Ok(())
    }

    fn sample(&mut self) -> DriverResult<Bmm150Sample> {
        let mut bus = I2cControllerIo::new(self.controller, self.driver_id, &self.device);
        let mut sensor = bus.target(self.address);
        let bytes = sensor.read_exact_block::<8>(BMM150_DATA_REGISTER)?;
        Ok(Bmm150Sample {
            x_raw: i16::from_le_bytes([bytes[0], bytes[1]]) >> 3,
            y_raw: i16::from_le_bytes([bytes[2], bytes[3]]) >> 3,
            z_raw: i16::from_le_bytes([bytes[4], bytes[5]]) >> 1,
            rhall_raw: u16::from_le_bytes([bytes[6], bytes[7]]) >> 2,
        })
    }
}

impl BoundDevice for Bmm150BoundDevice {
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
            let mut io = Bmm150Io::new(
                &self.driver_id,
                self.device.clone(),
                &mut *self.controller,
                self.binding.address,
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
                OperationRecord::new(BMM150_SAMPLE_INTERACTION, OperationStatus::Succeeded)
                    .with_output(output),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(request)
                if request.id.as_str() == BMM150_SAMPLE_INTERACTION =>
            {
                let sample = {
                    let mut io = Bmm150Io::new(
                        &self.driver_id,
                        self.device.clone(),
                        &mut *self.controller,
                        self.binding.address,
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
