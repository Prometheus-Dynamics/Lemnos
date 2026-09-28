//! Kernel-managed fan control through the Linux hwmon sysfs ABI.
//!
//! Binds `pwm1`/`pwm1_enable`/`fan1_input` class devices that the Linux
//! backend's hwmon probe reports (for example `pwm-fan`). The kernel owns the
//! PWM timing, so this driver models the fan itself rather than a PWM channel:
//! a normalized duty ratio, a control mode, and an optional tachometer reading.
//!
//! The driver only touches files under the device's
//! [`DeviceControlSurface::LinuxClass`](lemnos_core::DeviceControlSurface)
//! root, so it depends on no Linux-specific crate and can be exercised against
//! a fake sysfs tree.

use lemnos_core::{
    CoreError, CustomInteractionResponse, DeviceDescriptor, DeviceKind, DeviceLifecycleState,
    DeviceStateSnapshot, InteractionId, InteractionRequest, InteractionResponse, InterfaceKind,
    OperationRecord, OperationStatus, Value, ValueMap,
};
use lemnos_driver_manifest::{DriverManifest, DriverPriority, MatchCondition, MatchRule};
use lemnos_driver_sdk::{
    BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverMatch,
    DriverResult, LinuxClassDeviceIo, interaction_name,
};
use std::borrow::Cow;

pub const DRIVER_ID: &str = "lemnos.pwm.hwmon-fan";

/// Reads the fan. Output: the fan state map (see [`HwmonFanDriver`]).
pub const FAN_READ_INTERACTION: &str = "fan.read";
/// Sets the normalized duty ratio. Input: `f64` in `0.0..=1.0`.
pub const FAN_SET_DUTY_INTERACTION: &str = "fan.set_duty";
/// Sets the raw hwmon `pwm1` value. Input: `u64` in `0..=255`.
pub const FAN_SET_PWM_INTERACTION: &str = "fan.set_pwm";
/// Sets the control mode. Input: a [`FanMode`] name (`"full-speed"`,
/// `"manual"`, `"automatic"`) or the raw `pwm1_enable` value as `u64`.
pub const FAN_SET_MODE_INTERACTION: &str = "fan.set_mode";

/// Largest raw value the hwmon `pwm1` attribute accepts.
pub const HWMON_PWM_MAX: u64 = 255;
/// Largest raw `pwm1_enable` value accepted. Values from 2 upward select
/// chip-specific automatic modes.
pub const HWMON_PWM_ENABLE_MAX: u64 = 5;

pub const TELEMETRY_DUTY_RATIO: &str = "duty_ratio";
pub const TELEMETRY_PWM: &str = "pwm";
pub const TELEMETRY_PWM_MODE: &str = "pwm_mode";
pub const TELEMETRY_MODE: &str = "mode";
pub const TELEMETRY_RPM: &str = "rpm";
pub const CONFIG_FAN_NAME: &str = "fan_name";
pub const CONFIG_LABEL: &str = "label";
pub const CONFIG_CLASS_ROOT: &str = "linux.class_root";

const INTERACTIONS: [(&str, &str); 4] = [
    (FAN_READ_INTERACTION, "Read fan duty, mode, and speed"),
    (
        FAN_SET_DUTY_INTERACTION,
        "Set fan duty ratio from 0.0 to 1.0",
    ),
    (FAN_SET_PWM_INTERACTION, "Set raw hwmon pwm1 from 0 to 255"),
    (
        FAN_SET_MODE_INTERACTION,
        "Set fan control mode (full-speed, manual, automatic)",
    ),
];

/// Fan control mode, from the hwmon `pwm1_enable` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    /// `0`: no speed control; the fan runs at full speed.
    FullSpeed,
    /// `1`: manual control; `pwm1` sets the speed.
    Manual,
    /// `2` and up: the chip or kernel controls speed. The raw value is kept
    /// because its meaning is driver-specific.
    Automatic(u64),
}

impl FanMode {
    pub fn from_raw(raw: u64) -> Self {
        match raw {
            0 => Self::FullSpeed,
            1 => Self::Manual,
            other => Self::Automatic(other),
        }
    }

    pub fn raw(self) -> u64 {
        match self {
            Self::FullSpeed => 0,
            Self::Manual => 1,
            Self::Automatic(raw) => raw,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::FullSpeed => "full-speed",
            Self::Manual => "manual",
            Self::Automatic(_) => "automatic",
        }
    }

    /// Parses a mode name. `"automatic"` selects the default automatic mode, `2`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "full-speed" => Some(Self::FullSpeed),
            "manual" => Some(Self::Manual),
            "automatic" => Some(Self::Automatic(2)),
            _ => None,
        }
    }
}

/// Driver for Linux hwmon fans, matched by `linux.subsystem = "hwmon"`.
///
/// State telemetry: `duty_ratio` (`f64`, `0.0..=1.0`), `pwm` (raw `0..=255`),
/// `pwm_mode` (raw `pwm1_enable`), `mode` (a [`FanMode`] name), and `rpm`
/// when the chip has a tachometer. Realized config: `label`, `fan_name`, and
/// `linux.class_root`.
///
/// Interactions: [`FAN_READ_INTERACTION`], [`FAN_SET_DUTY_INTERACTION`],
/// [`FAN_SET_PWM_INTERACTION`], and [`FAN_SET_MODE_INTERACTION`]. Each
/// returns the fan state map after the operation. The kernel only applies
/// `pwm1` in [`FanMode::Manual`].
#[derive(Debug, Clone, Copy, Default)]
pub struct HwmonFanDriver;

impl HwmonFanDriver {
    pub const DRIVER_ID: &'static str = DRIVER_ID;
}

pub fn manifest() -> DriverManifest {
    INTERACTIONS
        .iter()
        .fold(
            DriverManifest::new(
                DRIVER_ID,
                "Linux hwmon fan driver",
                vec![InterfaceKind::Pwm],
            )
            .with_priority(DriverPriority::Preferred)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::Pwm)),
            |manifest, (id, summary)| manifest.with_custom_interaction(*id, *summary),
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
        .with_tag("hwmon")
}

impl Driver for HwmonFanDriver {
    fn id(&self) -> &str {
        DRIVER_ID
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Pwm
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(manifest())
    }

    fn matches(&self, device: &DeviceDescriptor) -> DriverMatch {
        manifest().match_device(device).into()
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        let io = LinuxClassDeviceIo::from_device(DRIVER_ID, device)?;
        let interactions = INTERACTIONS
            .iter()
            .map(|(id, summary)| {
                CustomInteraction::new(*id, *summary).map_err(|source| DriverError::BindFailed {
                    driver_id: DRIVER_ID.to_string(),
                    device_id: device.id.clone(),
                    reason: source.to_string(),
                })
            })
            .collect::<DriverResult<Vec<_>>>()?;

        let bound = HwmonFanBoundDevice {
            device: device.clone(),
            io,
            interactions,
        };
        bound.read_sample()?;
        Ok(Box::new(bound))
    }
}

struct HwmonFanBoundDevice {
    device: DeviceDescriptor,
    io: LinuxClassDeviceIo,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FanSample {
    pwm: u64,
    pwm_mode: u64,
    rpm: Option<u64>,
}

impl FanSample {
    fn duty_ratio(self) -> f64 {
        self.pwm.min(HWMON_PWM_MAX) as f64 / HWMON_PWM_MAX as f64
    }

    fn mode(self) -> FanMode {
        FanMode::from_raw(self.pwm_mode)
    }

    fn to_value(self, fan_name: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert(CONFIG_FAN_NAME.into(), Value::from(fan_name));
        map.insert(TELEMETRY_DUTY_RATIO.into(), Value::from(self.duty_ratio()));
        map.insert(TELEMETRY_PWM.into(), Value::from(self.pwm));
        map.insert(TELEMETRY_PWM_MODE.into(), Value::from(self.pwm_mode));
        map.insert(TELEMETRY_MODE.into(), Value::from(self.mode().name()));
        if let Some(rpm) = self.rpm {
            map.insert(TELEMETRY_RPM.into(), Value::from(rpm));
        }
        Value::from(map)
    }
}

impl HwmonFanBoundDevice {
    fn read_sample(&self) -> DriverResult<FanSample> {
        Ok(FanSample {
            pwm: self.io.read_u64("pwm1")?,
            pwm_mode: self.io.read_u64("pwm1_enable")?,
            rpm: self.io.read_optional_u64("fan1_input")?,
        })
    }

    fn fan_name(&self) -> &str {
        self.device
            .properties
            .get("hwmon.name")
            .and_then(Value::as_str)
            .or(self.device.display_name.as_deref())
            .unwrap_or(self.device.id.as_str())
    }

    fn invalid(&self, interaction: &'static str, reason: impl Into<String>) -> DriverError {
        DriverError::InvalidRequest {
            driver_id: DRIVER_ID.to_string(),
            device_id: self.device.id.clone(),
            source: CoreError::InvalidRequest {
                request: interaction,
                reason: reason.into(),
            },
        }
    }

    fn set_pwm(&self, pwm: u64) -> DriverResult<FanSample> {
        if pwm > HWMON_PWM_MAX {
            return Err(self.invalid(
                FAN_SET_PWM_INTERACTION,
                format!("pwm must be between 0 and {HWMON_PWM_MAX}"),
            ));
        }
        self.io.write_u64("pwm1", pwm)?;
        self.read_sample()
    }

    fn set_duty(&self, input: Option<&Value>) -> DriverResult<FanSample> {
        let ratio = input
            .and_then(Value::as_f64)
            .filter(|ratio| (0.0..=1.0).contains(ratio))
            .ok_or_else(|| {
                self.invalid(
                    FAN_SET_DUTY_INTERACTION,
                    "expected an f64 duty ratio between 0.0 and 1.0",
                )
            })?;
        self.set_pwm((ratio * HWMON_PWM_MAX as f64).round() as u64)
    }

    fn set_mode(&self, input: Option<&Value>) -> DriverResult<FanSample> {
        let raw = match input {
            Some(Value::U64(raw)) => Some(*raw),
            Some(Value::String(name)) => FanMode::from_name(name).map(FanMode::raw),
            _ => None,
        }
        .filter(|raw| *raw <= HWMON_PWM_ENABLE_MAX)
        .ok_or_else(|| {
            self.invalid(
                FAN_SET_MODE_INTERACTION,
                format!(
                    "expected \"full-speed\", \"manual\", \"automatic\", or a pwm1_enable value from 0 to {HWMON_PWM_ENABLE_MAX}"
                ),
            )
        })?;
        self.io.write_u64("pwm1_enable", raw)?;
        self.read_sample()
    }

    fn state_from_sample(&self, sample: FanSample) -> DeviceStateSnapshot {
        let fan_name = self.fan_name().to_string();
        let mut state = DeviceStateSnapshot::new(self.device.id.clone())
            .with_lifecycle(DeviceLifecycleState::Idle)
            .with_config(CONFIG_LABEL, fan_name.clone())
            .with_config(CONFIG_FAN_NAME, fan_name)
            .with_config(CONFIG_CLASS_ROOT, self.io.root().display().to_string())
            .with_telemetry(TELEMETRY_DUTY_RATIO, sample.duty_ratio())
            .with_telemetry(TELEMETRY_PWM, sample.pwm)
            .with_telemetry(TELEMETRY_PWM_MODE, sample.pwm_mode)
            .with_telemetry(TELEMETRY_MODE, sample.mode().name());
        if let Some(rpm) = sample.rpm {
            state = state.with_telemetry(TELEMETRY_RPM, rpm);
        }
        state
    }

    fn respond(&self, index: usize, sample: FanSample) -> InteractionResponse {
        let id: InteractionId = self.interactions[index].id.clone();
        InteractionResponse::Custom(
            CustomInteractionResponse::new(id).with_output(sample.to_value(self.fan_name())),
        )
    }
}

impl BoundDevice for HwmonFanBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        DRIVER_ID
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        &self.interactions
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        let sample = self.read_sample()?;
        Ok(Some(
            self.state_from_sample(sample).with_last_operation(
                OperationRecord::new(FAN_READ_INTERACTION, OperationStatus::Succeeded)
                    .with_output(sample.to_value(self.fan_name())),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        let InteractionRequest::Custom(custom) = request else {
            return Err(self.unsupported(request));
        };
        let (index, sample) = match custom.id.as_str() {
            FAN_READ_INTERACTION => (0, self.read_sample()?),
            FAN_SET_DUTY_INTERACTION => (1, self.set_duty(custom.input.as_ref())?),
            FAN_SET_PWM_INTERACTION => {
                let pwm = custom
                    .input
                    .as_ref()
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        self.invalid(FAN_SET_PWM_INTERACTION, "expected a u64 pwm input")
                    })?;
                (2, self.set_pwm(pwm)?)
            }
            FAN_SET_MODE_INTERACTION => (3, self.set_mode(custom.input.as_ref())?),
            _ => return Err(self.unsupported(request)),
        };
        Ok(self.respond(index, sample))
    }
}

impl HwmonFanBoundDevice {
    fn unsupported(&self, request: &InteractionRequest) -> DriverError {
        DriverError::UnsupportedAction {
            driver_id: DRIVER_ID.to_string(),
            device_id: self.device.id.clone(),
            action: interaction_name(request).into_owned(),
        }
    }
}
