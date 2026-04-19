use crate::linux_test_root::ExampleLinuxTestRoot;
use lemnos::core::{
    CoreError, CustomInteractionResponse, DeviceDescriptor, DeviceId, DeviceKind,
    DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest, InteractionResponse,
    InterfaceKind, OperationRecord, OperationStatus, Value, ValueMap,
};
use lemnos::driver::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverManifest,
    DriverMatch, DriverPriority, DriverResult, LinuxClassDeviceIo, MatchCondition, MatchRule,
    interaction_name,
};
use lemnos::linux::LinuxPaths;
use lemnos::prelude::*;
use std::borrow::Cow;
use std::error::Error;
use std::path::Path;

pub const FAN_READ_INTERACTION: &str = "fan.read";
pub const FAN_SET_PWM_INTERACTION: &str = "fan.set_pwm";
pub const FAN_SET_MODE_INTERACTION: &str = "fan.set_mode";

#[derive(Debug)]
pub struct LinuxHwmonFanTestRoot {
    root: ExampleLinuxTestRoot,
}

impl LinuxHwmonFanTestRoot {
    pub fn new() -> Self {
        Self {
            root: ExampleLinuxTestRoot::new("lemnos-linux-hwmon-fan-example"),
        }
    }

    pub fn paths(&self) -> LinuxPaths {
        self.root.paths()
    }

    pub fn create_fan(
        &self,
        hwmon_name: &str,
        logical_name: &str,
        pwm: u64,
        pwm_mode: u64,
        rpm: u64,
        driver: &str,
    ) {
        self.write(
            format!("sys/class/hwmon/{hwmon_name}/name"),
            &format!("{logical_name}\n"),
        );
        self.write(
            format!("sys/class/hwmon/{hwmon_name}/pwm1"),
            &format!("{pwm}\n"),
        );
        self.write(
            format!("sys/class/hwmon/{hwmon_name}/pwm1_enable"),
            &format!("{pwm_mode}\n"),
        );
        self.write(
            format!("sys/class/hwmon/{hwmon_name}/fan1_input"),
            &format!("{rpm}\n"),
        );
        self.create_dir("sys/devices/platform");
        self.create_dir(format!("sys/devices/platform/{logical_name}"));
        self.create_dir(format!("sys/bus/platform/drivers/{driver}"));
        std::os::unix::fs::symlink(
            self.root
                .root_path(format!("sys/devices/platform/{logical_name}")),
            self.root
                .root_path(format!("sys/class/hwmon/{hwmon_name}/device")),
        )
        .expect("device symlink");
        std::os::unix::fs::symlink(
            self.root
                .root_path(format!("sys/bus/platform/drivers/{driver}")),
            self.root
                .root_path(format!("sys/devices/platform/{logical_name}/driver")),
        )
        .expect("driver symlink");
    }

    pub fn read(&self, relative: impl AsRef<Path>) -> String {
        self.root.read(relative)
    }

    pub fn create_dir(&self, relative: impl AsRef<Path>) {
        self.root.create_dir(relative);
    }

    pub fn write(&self, relative: impl AsRef<Path>, contents: &str) {
        self.root.write(relative, contents);
    }
}

pub struct ExampleLinuxHwmonFanDriver;

impl ExampleLinuxHwmonFanDriver {
    pub const DRIVER_ID: &str = "example.linux.hwmon-fan";
}

impl Driver for ExampleLinuxHwmonFanDriver {
    fn id(&self) -> &str {
        Self::DRIVER_ID
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Pwm
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                self.id(),
                "Example Linux hwmon/pwm-fan driver",
                vec![InterfaceKind::Pwm],
            )
            .with_priority(DriverPriority::Preferred)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::Pwm))
            .with_custom_interaction(FAN_READ_INTERACTION, "Read fan state")
            .with_custom_interaction(
                FAN_SET_PWM_INTERACTION,
                "Set raw PWM value from a u64 input",
            )
            .with_custom_interaction(
                FAN_SET_MODE_INTERACTION,
                "Set pwm1_enable mode from a u64 input",
            )
            .with_rule(
                MatchRule::new(200)
                    .described("Linux hwmon fan control device")
                    .require(MatchCondition::PropertyEq {
                        key: "linux.subsystem".into(),
                        value: Value::from("hwmon"),
                    }),
            )
            .with_tag("linux")
            .with_tag("fan")
            .with_tag("kernel-managed"),
        )
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        self.manifest_ref().match_device(device).into()
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        let io = LinuxClassDeviceIo::from_device(self.id(), device)?;
        let interactions = [
            (FAN_READ_INTERACTION, "Read fan state"),
            (
                FAN_SET_PWM_INTERACTION,
                "Set raw PWM value from a u64 input",
            ),
            (
                FAN_SET_MODE_INTERACTION,
                "Set pwm1_enable mode from a u64 input",
            ),
        ]
        .into_iter()
        .map(|(id, summary)| {
            CustomInteraction::new(id, summary).map_err(|source| DriverError::BindFailed {
                driver_id: self.id().to_string(),
                device_id: device.id.clone(),
                reason: source.to_string(),
            })
        })
        .collect::<DriverResult<Vec<_>>>()?;

        let mut bound = ExampleLinuxHwmonFanBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            io,
            interactions,
        };
        bound.read_sample()?;
        Ok(Box::new(bound))
    }
}

struct ExampleLinuxHwmonFanBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    io: LinuxClassDeviceIo,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxHwmonFanSample {
    pwm: u64,
    pwm_mode: u64,
    rpm: Option<u64>,
}

impl LinuxHwmonFanSample {
    fn into_value(self, name: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("fan_name".into(), Value::from(name));
        map.insert("pwm".into(), Value::from(self.pwm));
        map.insert("pwm_mode".into(), Value::from(self.pwm_mode));
        if let Some(rpm) = self.rpm {
            map.insert("rpm".into(), Value::from(rpm));
        }
        Value::from(map)
    }

    fn apply_telemetry(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        let mut state = state
            .with_telemetry("pwm", self.pwm)
            .with_telemetry("pwm_mode", self.pwm_mode);
        if let Some(rpm) = self.rpm {
            state = state.with_telemetry("rpm", rpm);
        }
        state
    }
}

impl ExampleLinuxHwmonFanBoundDevice {
    fn read_sample(&mut self) -> DriverResult<LinuxHwmonFanSample> {
        Ok(LinuxHwmonFanSample {
            pwm: self.io.read_u64("pwm1")?,
            pwm_mode: self.io.read_u64("pwm1_enable")?,
            rpm: self.io.read_optional_u64("fan1_input")?,
        })
    }

    fn set_pwm(&mut self, pwm: u64) -> DriverResult<LinuxHwmonFanSample> {
        if pwm > 255 {
            return Err(DriverError::InvalidRequest {
                driver_id: self.driver_id.clone(),
                device_id: self.device.id.clone(),
                source: CoreError::InvalidRequest {
                    request: FAN_SET_PWM_INTERACTION,
                    reason: "PWM must be between 0 and 255".into(),
                },
            });
        }
        self.io.write_u64("pwm1", pwm)?;
        self.read_sample()
    }

    fn set_mode(&mut self, mode: u64) -> DriverResult<LinuxHwmonFanSample> {
        if !matches!(mode, 0..=3) {
            return Err(DriverError::InvalidRequest {
                driver_id: self.driver_id.clone(),
                device_id: self.device.id.clone(),
                source: CoreError::InvalidRequest {
                    request: FAN_SET_MODE_INTERACTION,
                    reason: "pwm1_enable must be between 0 and 3".into(),
                },
            });
        }
        self.io.write_u64("pwm1_enable", mode)?;
        self.read_sample()
    }

    fn fan_name(&self) -> &str {
        self.device
            .properties
            .get("hwmon.name")
            .and_then(Value::as_str)
            .or(self.device.display_name.as_deref())
            .unwrap_or(self.device.id.as_str())
    }

    fn state_from_sample(&self, sample: &LinuxHwmonFanSample) -> DeviceStateSnapshot {
        sample.apply_telemetry(
            DeviceStateSnapshot::new(self.device.id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle)
                .with_config("label", self.fan_name().to_string())
                .with_config("linux.class_root", self.io.root().display().to_string())
                .with_config("fan_name", self.fan_name().to_string()),
        )
    }

    fn response_for(
        interaction_id: lemnos::core::InteractionId,
        fan_name: &str,
        sample: LinuxHwmonFanSample,
    ) -> InteractionResponse {
        InteractionResponse::Custom(
            CustomInteractionResponse::new(interaction_id).with_output(sample.into_value(fan_name)),
        )
    }
}

impl BoundDevice for ExampleLinuxHwmonFanBoundDevice {
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
        let sample = self.read_sample()?;
        let output = sample.clone().into_value(self.fan_name());
        Ok(Some(
            self.state_from_sample(&sample).with_last_operation(
                OperationRecord::new(FAN_READ_INTERACTION, OperationStatus::Succeeded)
                    .with_output(output),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(request) if request.id.as_str() == FAN_READ_INTERACTION => {
                let sample = self.read_sample()?;
                let interaction_id = self.interactions[0].id.clone();
                let fan_name = self.fan_name().to_string();
                Ok(Self::response_for(interaction_id, &fan_name, sample))
            }
            InteractionRequest::Custom(request)
                if request.id.as_str() == FAN_SET_PWM_INTERACTION =>
            {
                let pwm = require_u64_input(
                    &self.driver_id,
                    &self.device.id,
                    request.input.as_ref(),
                    FAN_SET_PWM_INTERACTION,
                )?;
                let sample = self.set_pwm(pwm)?;
                let interaction_id = self.interactions[1].id.clone();
                let fan_name = self.fan_name().to_string();
                Ok(Self::response_for(interaction_id, &fan_name, sample))
            }
            InteractionRequest::Custom(request)
                if request.id.as_str() == FAN_SET_MODE_INTERACTION =>
            {
                let mode = require_u64_input(
                    &self.driver_id,
                    &self.device.id,
                    request.input.as_ref(),
                    FAN_SET_MODE_INTERACTION,
                )?;
                let sample = self.set_mode(mode)?;
                let interaction_id = self.interactions[2].id.clone();
                let fan_name = self.fan_name().to_string();
                Ok(Self::response_for(interaction_id, &fan_name, sample))
            }
            _ => Err(DriverError::UnsupportedAction {
                driver_id: self.driver_id.clone(),
                device_id: self.device.id.clone(),
                action: interaction_name(request).into_owned(),
            }),
        }
    }
}

fn require_u64_input(
    driver_id: &str,
    device_id: &DeviceId,
    input: Option<&Value>,
    interaction: &'static str,
) -> DriverResult<u64> {
    input
        .and_then(Value::as_u64)
        .ok_or_else(|| DriverError::InvalidRequest {
            driver_id: driver_id.to_string(),
            device_id: device_id.clone(),
            source: CoreError::InvalidRequest {
                request: interaction,
                reason: "expected a u64 custom input".into(),
            },
        })
}

pub fn fan_device_id(lemnos: &Lemnos) -> Result<DeviceId, Box<dyn Error>> {
    lemnos
        .inventory()
        .by_kind(DeviceKind::Unspecified(InterfaceKind::Pwm))
        .into_iter()
        .find(|device| device.properties.get("linux.subsystem") == Some(&Value::from("hwmon")))
        .map(|device| device.id.clone())
        .ok_or_else(|| "failed to find Linux hwmon fan device".into())
}
