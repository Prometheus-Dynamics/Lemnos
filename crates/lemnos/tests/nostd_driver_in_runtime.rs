//! A no_std embedded-hal device driver (`lemnos-drivers-bmi088`) running
//! inside the Lemnos runtime: the runtime binds a thin adapter driver, which
//! drives the chip through `lemnos::bus::hal::HalI2cBus` over the bind
//! context's I2C controller session.
#![cfg(feature = "mock")]

use lemnos::bus::BusError;
use lemnos::bus::hal::HalI2cBus;
use lemnos::core::{
    CustomInteractionResponse, DeviceDescriptor, DeviceKind, InteractionRequest,
    InteractionResponse, InterfaceKind, Value,
};
use lemnos::discovery::DiscoveryContext;
use lemnos::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverPriority, DriverResult, I2cControllerSession, MatchCondition, MatchRule,
    SessionAccess, interaction_name,
};
use lemnos::mock::{MockHardware, MockI2cDevice};
use lemnos::prelude::*;
use lemnos_drivers_bmi088::{
    ACCEL_ADDRESS, ACCEL_CHIP_ID, Bmi088, Config, GYRO_ADDRESS, GYRO_CHIP_ID, STANDARD_GRAVITY,
};
use std::borrow::Cow;

const DRIVER_ID: &str = "test.bmi088";
const SAMPLE: &str = "imu.sample";
const BUS: u32 = 4;

struct NoDelay;

impl embedded_hal::delay::DelayNs for NoDelay {
    fn delay_ns(&mut self, _ns: u32) {}
}

/// Bus failures keep their `BusError`; chip-level failures (wrong chip ID)
/// reject the bind.
fn driver_error(
    device: &DeviceDescriptor,
    error: lemnos_drivers_bmi088::Error<BusError>,
) -> DriverError {
    if let lemnos_drivers_bmi088::Error::Register(register) = &error
        && let Some(source) = register.bus_error()
    {
        return DriverError::Transport {
            driver_id: DRIVER_ID.into(),
            device_id: device.id.clone(),
            source: source.clone(),
        };
    }
    DriverError::BindRejected {
        driver_id: DRIVER_ID.into(),
        device_id: device.id.clone(),
        reason: error.to_string(),
    }
}

struct Bmi088Adapter;

impl Driver for Bmi088Adapter {
    fn id(&self) -> &str {
        DRIVER_ID
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                DRIVER_ID,
                "BMI088 via lemnos-drivers-bmi088",
                vec![InterfaceKind::I2c],
            )
            .with_priority(DriverPriority::Exact)
            .with_kind(DeviceKind::I2cDevice)
            .with_custom_interaction(SAMPLE, "Read accel and gyro in SI units")
            .with_rule(
                MatchRule::new(500)
                    .require(MatchCondition::PropertyEq {
                        key: "bus".into(),
                        value: Value::from(u64::from(BUS)),
                    })
                    .require(MatchCondition::PropertyEq {
                        key: "address".into(),
                        value: Value::from(u64::from(ACCEL_ADDRESS)),
                    }),
            ),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        self.manifest_ref().match_device(device).into()
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        let mut controller = context.open_i2c_controller(
            DRIVER_ID,
            device,
            BUS,
            SessionAccess::ExclusiveController,
        )?;
        let config = Config::default();
        Bmi088::new(HalI2cBus::new(&mut *controller))
            .init(&mut NoDelay, config)
            .map_err(|error| driver_error(device, error))?;
        Ok(Box::new(Bmi088Bound {
            device: device.clone(),
            controller,
            config,
            interactions: vec![CustomInteraction::new(SAMPLE, "Read accel and gyro").expect("id")],
        }))
    }
}

struct Bmi088Bound {
    device: DeviceDescriptor,
    controller: Box<dyn I2cControllerSession>,
    config: Config,
    interactions: Vec<CustomInteraction>,
}

impl BoundDevice for Bmi088Bound {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        DRIVER_ID
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        &self.interactions
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(custom) if custom.id.as_str() == SAMPLE => {
                let bus = HalI2cBus::new(&mut *self.controller);
                let sample = Bmi088::resume(bus, ACCEL_ADDRESS, GYRO_ADDRESS, self.config)
                    .read()
                    .map_err(|error| driver_error(&self.device, error))?;
                let axes = |values: [f32; 3]| {
                    Value::from(values.map(|v| Value::from(f64::from(v))).to_vec())
                };
                let mut output = lemnos::core::ValueMap::new();
                output.insert("accel_mps2".into(), axes(sample.accel_mps2));
                output.insert("gyro_radps".into(), axes(sample.gyro_radps));
                Ok(InteractionResponse::Custom(
                    CustomInteractionResponse::new(self.interactions[0].id.clone())
                        .with_output(Value::from(output)),
                ))
            }
            _ => Err(DriverError::UnsupportedAction {
                driver_id: DRIVER_ID.into(),
                device_id: self.device.id.clone(),
                action: interaction_name(request).into_owned(),
            }),
        }
    }
}

fn axes(values: [i16; 3]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn nostd_bmi088_driver_runs_inside_the_runtime_over_session_hal() {
    let hardware = MockHardware::builder()
        .with_i2c_device(
            MockI2cDevice::new(BUS, ACCEL_ADDRESS.into())
                .with_bytes(0x00, [ACCEL_CHIP_ID])
                .with_bytes(0x12, axes([16384, 0, -16384])),
        )
        .with_i2c_device(
            MockI2cDevice::new(BUS, GYRO_ADDRESS.into())
                .with_bytes(0x00, [GYRO_CHIP_ID])
                .with_bytes(0x02, axes([0, 16384, 0])),
        )
        .build();
    let accel_id = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.properties.get("address") == Some(&Value::from(0x18_u64)))
        .expect("accel descriptor")
        .id;

    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_driver(Bmi088Adapter)
        .expect("register adapter")
        .build();
    lemnos
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    lemnos
        .bind(&accel_id)
        .expect("bind runs BMI088 init over the session");

    let response = lemnos.request_custom(accel_id, SAMPLE).expect("sample");
    let InteractionResponse::Custom(CustomInteractionResponse {
        output: Some(Value::Map(output)),
        ..
    }) = response.interaction
    else {
        panic!("unexpected response {:?}", response.interaction);
    };
    let axis = |key: &str, index: usize| {
        output[key].as_list().expect("list")[index]
            .as_f64()
            .expect("f64")
    };
    // Half of the default ±6 g and ±2000 °/s.
    assert!((axis("accel_mps2", 0) - 3.0 * f64::from(STANDARD_GRAVITY)).abs() < 1e-3);
    assert!((axis("accel_mps2", 2) + 3.0 * f64::from(STANDARD_GRAVITY)).abs() < 1e-3);
    assert!((axis("gyro_radps", 1) - 1000_f64.to_radians()).abs() < 1e-4);
}
