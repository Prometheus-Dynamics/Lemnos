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

pub const LED_READ_INTERACTION: &str = "led.read";
pub const LED_ON_INTERACTION: &str = "led.on";
pub const LED_OFF_INTERACTION: &str = "led.off";
pub const LED_SET_BRIGHTNESS_INTERACTION: &str = "led.set_brightness";
pub const LED_SET_TRIGGER_INTERACTION: &str = "led.set_trigger";

#[derive(Debug)]
pub struct LinuxLedTestRoot {
    root: ExampleLinuxTestRoot,
}

impl LinuxLedTestRoot {
    pub fn new() -> Self {
        Self {
            root: ExampleLinuxTestRoot::new("lemnos-linux-led-example"),
        }
    }

    pub fn paths(&self) -> LinuxPaths {
        self.root.paths()
    }

    pub fn create_led(
        &self,
        led_name: &str,
        brightness: u64,
        max_brightness: u64,
        trigger: &str,
        driver: &str,
    ) {
        self.write(
            format!("sys/class/leds/{led_name}/brightness"),
            &format!("{brightness}\n"),
        );
        self.write(
            format!("sys/class/leds/{led_name}/max_brightness"),
            &format!("{max_brightness}\n"),
        );
        self.write(format!("sys/class/leds/{led_name}/trigger"), trigger);
        self.create_dir("sys/devices/platform");
        self.create_dir(format!("sys/devices/platform/{led_name}"));
        self.create_dir(format!("sys/bus/platform/drivers/{driver}"));
        std::os::unix::fs::symlink(
            self.root
                .root_path(format!("sys/devices/platform/{led_name}")),
            self.root
                .root_path(format!("sys/class/leds/{led_name}/device")),
        )
        .expect("device symlink");
        std::os::unix::fs::symlink(
            self.root
                .root_path(format!("sys/bus/platform/drivers/{driver}")),
            self.root
                .root_path(format!("sys/devices/platform/{led_name}/driver")),
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

pub struct ExampleLinuxLedDriver;

impl ExampleLinuxLedDriver {
    pub const DRIVER_ID: &str = "example.linux.led";
}

impl Driver for ExampleLinuxLedDriver {
    fn id(&self) -> &str {
        Self::DRIVER_ID
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                self.id(),
                "Example Linux LED class driver",
                vec![InterfaceKind::Gpio],
            )
            .with_priority(DriverPriority::Preferred)
            .with_kind(DeviceKind::Unspecified(InterfaceKind::Gpio))
            .with_custom_interaction(LED_READ_INTERACTION, "Read LED state")
            .with_custom_interaction(LED_ON_INTERACTION, "Set LED to max brightness")
            .with_custom_interaction(LED_OFF_INTERACTION, "Turn LED off")
            .with_custom_interaction(
                LED_SET_BRIGHTNESS_INTERACTION,
                "Set LED brightness from a u64 input",
            )
            .with_custom_interaction(
                LED_SET_TRIGGER_INTERACTION,
                "Set LED trigger from a string input",
            )
            .with_rule(
                MatchRule::new(200)
                    .described("Linux LED class device")
                    .require(MatchCondition::PropertyEq {
                        key: "linux.subsystem".into(),
                        value: Value::from("leds"),
                    }),
            )
            .with_tag("linux")
            .with_tag("led")
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
            (LED_READ_INTERACTION, "Read LED state"),
            (LED_ON_INTERACTION, "Set LED to max brightness"),
            (LED_OFF_INTERACTION, "Turn LED off"),
            (
                LED_SET_BRIGHTNESS_INTERACTION,
                "Set LED brightness from a u64 input",
            ),
            (
                LED_SET_TRIGGER_INTERACTION,
                "Set LED trigger from a string input",
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

        let mut bound = ExampleLinuxLedBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            io,
            interactions,
        };
        bound.read_sample()?;
        Ok(Box::new(bound))
    }
}

struct ExampleLinuxLedBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    io: LinuxClassDeviceIo,
    interactions: Vec<CustomInteraction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxLedSample {
    brightness: u64,
    max_brightness: u64,
    active_trigger: Option<String>,
}

impl LinuxLedSample {
    fn into_value(self, led_name: &str) -> Value {
        let mut map = ValueMap::new();
        map.insert("led_name".into(), Value::from(led_name));
        map.insert("brightness".into(), Value::from(self.brightness));
        map.insert("max_brightness".into(), Value::from(self.max_brightness));
        if let Some(trigger) = self.active_trigger {
            map.insert("active_trigger".into(), Value::from(trigger));
        }
        Value::from(map)
    }

    fn apply_telemetry(&self, state: DeviceStateSnapshot) -> DeviceStateSnapshot {
        let mut state = state
            .with_telemetry("brightness", self.brightness)
            .with_telemetry("max_brightness", self.max_brightness);
        if let Some(trigger) = &self.active_trigger {
            state = state.with_telemetry("active_trigger", trigger.clone());
        }
        state
    }
}

impl ExampleLinuxLedBoundDevice {
    fn read_sample(&mut self) -> DriverResult<LinuxLedSample> {
        Ok(LinuxLedSample {
            brightness: self.io.read_u64("brightness")?,
            max_brightness: self.io.read_u64("max_brightness")?,
            active_trigger: parse_active_trigger(self.io.read_optional_trimmed("trigger")?),
        })
    }

    fn set_brightness(&mut self, brightness: u64) -> DriverResult<LinuxLedSample> {
        let max_brightness = self.io.read_u64("max_brightness")?;
        if brightness > max_brightness {
            return Err(DriverError::InvalidRequest {
                driver_id: self.driver_id.clone(),
                device_id: self.device.id.clone(),
                source: CoreError::InvalidRequest {
                    request: LED_SET_BRIGHTNESS_INTERACTION,
                    reason: format!(
                        "brightness {brightness} exceeds max brightness {max_brightness}"
                    ),
                },
            });
        }
        self.io.write_u64("brightness", brightness)?;
        self.read_sample()
    }

    fn set_trigger(&mut self, trigger: &str) -> DriverResult<LinuxLedSample> {
        self.io.write_str("trigger", trigger)?;
        self.read_sample()
    }

    fn led_name(&self) -> &str {
        self.device
            .properties
            .get("led.name")
            .and_then(Value::as_str)
            .or(self.device.display_name.as_deref())
            .unwrap_or(self.device.id.as_str())
    }

    fn state_from_sample(&self, sample: &LinuxLedSample) -> DeviceStateSnapshot {
        sample.apply_telemetry(
            DeviceStateSnapshot::new(self.device.id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle)
                .with_config("label", self.led_name().to_string())
                .with_config("linux.class_root", self.io.root().display().to_string())
                .with_config("led_name", self.led_name().to_string()),
        )
    }

    fn response_for(
        interaction_id: lemnos::core::InteractionId,
        led_name: &str,
        sample: LinuxLedSample,
    ) -> InteractionResponse {
        InteractionResponse::Custom(
            CustomInteractionResponse::new(interaction_id).with_output(sample.into_value(led_name)),
        )
    }
}

impl BoundDevice for ExampleLinuxLedBoundDevice {
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
        let output = sample.clone().into_value(self.led_name());
        Ok(Some(
            self.state_from_sample(&sample).with_last_operation(
                OperationRecord::new(LED_READ_INTERACTION, OperationStatus::Succeeded)
                    .with_output(output),
            ),
        ))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        match request {
            InteractionRequest::Custom(request) if request.id.as_str() == LED_READ_INTERACTION => {
                let sample = self.read_sample()?;
                let interaction_id = self.interactions[0].id.clone();
                let led_name = self.led_name().to_string();
                Ok(Self::response_for(interaction_id, &led_name, sample))
            }
            InteractionRequest::Custom(request) if request.id.as_str() == LED_ON_INTERACTION => {
                let max_brightness = self.io.read_u64("max_brightness")?;
                let sample = self.set_brightness(max_brightness)?;
                let interaction_id = self.interactions[1].id.clone();
                let led_name = self.led_name().to_string();
                Ok(Self::response_for(interaction_id, &led_name, sample))
            }
            InteractionRequest::Custom(request) if request.id.as_str() == LED_OFF_INTERACTION => {
                let sample = self.set_brightness(0)?;
                let interaction_id = self.interactions[2].id.clone();
                let led_name = self.led_name().to_string();
                Ok(Self::response_for(interaction_id, &led_name, sample))
            }
            InteractionRequest::Custom(request)
                if request.id.as_str() == LED_SET_BRIGHTNESS_INTERACTION =>
            {
                let brightness =
                    require_u64_input(&self.driver_id, &self.device.id, request.input.as_ref())?;
                let sample = self.set_brightness(brightness)?;
                let interaction_id = self.interactions[3].id.clone();
                let led_name = self.led_name().to_string();
                Ok(Self::response_for(interaction_id, &led_name, sample))
            }
            InteractionRequest::Custom(request)
                if request.id.as_str() == LED_SET_TRIGGER_INTERACTION =>
            {
                let trigger =
                    require_string_input(&self.driver_id, &self.device.id, request.input.as_ref())?;
                let sample = self.set_trigger(&trigger)?;
                let interaction_id = self.interactions[4].id.clone();
                let led_name = self.led_name().to_string();
                Ok(Self::response_for(interaction_id, &led_name, sample))
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
) -> DriverResult<u64> {
    input
        .and_then(Value::as_u64)
        .ok_or_else(|| DriverError::InvalidRequest {
            driver_id: driver_id.to_string(),
            device_id: device_id.clone(),
            source: CoreError::InvalidRequest {
                request: LED_SET_BRIGHTNESS_INTERACTION,
                reason: "expected a u64 custom input".into(),
            },
        })
}

fn require_string_input(
    driver_id: &str,
    device_id: &DeviceId,
    input: Option<&Value>,
) -> DriverResult<String> {
    input
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| DriverError::InvalidRequest {
            driver_id: driver_id.to_string(),
            device_id: device_id.clone(),
            source: CoreError::InvalidRequest {
                request: LED_SET_TRIGGER_INTERACTION,
                reason: "expected a string custom input".into(),
            },
        })
}

fn parse_active_trigger(trigger_text: Option<String>) -> Option<String> {
    let trigger_text = trigger_text?;
    let mut tokens = trigger_text.split_whitespace().peekable();
    let mut fallback = None;
    while let Some(token) = tokens.next() {
        if let Some(active) = token
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            return Some(active.to_string());
        }
        if fallback.is_none() && tokens.peek().is_none() {
            fallback = Some(token.to_string());
        }
    }
    fallback
}

pub fn led_device_id(lemnos: &Lemnos) -> Result<DeviceId, Box<dyn Error>> {
    lemnos
        .inventory()
        .by_kind(DeviceKind::Unspecified(InterfaceKind::Gpio))
        .into_iter()
        .find(|device| device.properties.get("linux.subsystem") == Some(&Value::from("leds")))
        .map(|device| device.id.clone())
        .ok_or_else(|| "failed to find Linux LED device".into())
}
