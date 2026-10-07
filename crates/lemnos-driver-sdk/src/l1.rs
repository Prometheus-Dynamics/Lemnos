//! The generic adapter from the compact device model (`lemnos-device`) to
//! the runtime: any L1 device becomes a [`BoundDevice`].
//!
//! - Channels become state telemetry keyed by the channel name
//!   (`acceleration.x`), as `f64` in the quantity's canonical unit
//!   (`Value::Null` for a channel without a reading).
//! - Controls become custom interactions `<control>.set` (input: a number in
//!   the unit) and `<control>.get`; [`READ_INTERACTION`] returns every
//!   channel.
//! - [`describe`] adds the device class, model and channel units to a
//!   descriptor; [`L1Driver`] binds descriptors through a factory that
//!   returns a [`BoxedDevice`].
//!
//! Drivers with their own interactions (the hwmon fan driver) reuse
//! [`telemetry`], [`channel_value`] and [`control_value`].

use crate::{BoundDevice, CustomInteraction, Driver, DriverBindContext, DriverError, DriverResult};
use lemnos_core::{
    CoreError, CustomInteractionResponse, DeviceDescriptor, DeviceDescriptorBuilder, DeviceHealth,
    DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest, InteractionResponse,
    InterfaceKind, Value, ValueMap,
};
use lemnos_device::{BoxedDevice, Channel, ControlInfo, DeviceInfo, MAX_CHANNELS, NO_VALUE};
use lemnos_driver_manifest::{DriverManifest, DriverPriority, MatchCondition, MatchRule};
use lemnos_hal::ErrorKind;
use std::borrow::Cow;
use std::sync::Mutex;
use std::time::Duration;

/// Reads every channel. Output: a map of channel name to value.
pub const READ_INTERACTION: &str = "device.read";
/// Descriptor property: the device class (`imu`, `fan`, ...).
pub const PROPERTY_CLASS: &str = "device.class";
/// Descriptor property: the model (`BMI088`).
pub const PROPERTY_MODEL: &str = "device.model";
/// Descriptor property naming the [`L1Driver`] that binds the device.
pub const PROPERTY_DRIVER: &str = "lemnos.driver";

/// `raw × 10^exponent` as the nearest `f64` (dividing by an exact power of
/// ten, so 702 at -3 is 0.702, not 0.7020000000000001).
pub fn scaled(raw: i32, exponent: i8) -> f64 {
    let scale = 10f64.powi(i32::from(exponent.unsigned_abs()));
    if exponent < 0 {
        f64::from(raw) / scale
    } else {
        f64::from(raw) * scale
    }
}

/// A raw channel reading as a runtime value: `f64` in the unit, or
/// `Value::Null` for [`NO_VALUE`].
pub fn channel_value(channel: &Channel, raw: i32) -> Value {
    if raw == NO_VALUE {
        Value::Null
    } else {
        Value::from(scaled(raw, channel.exponent))
    }
}

/// A raw control value as a runtime value (`f64` in the unit).
pub fn control_value(control: &ControlInfo, raw: i32) -> Value {
    Value::from(scaled(raw, control.exponent))
}

/// A runtime value in a control's unit as its raw count: numbers (`f64`,
/// `i64`, `u64`) in the unit, rounded, within the control's range.
pub fn control_raw(control: &ControlInfo, value: &Value) -> Option<i32> {
    let unit = value
        .as_f64()
        .or_else(|| value.as_i64().map(|v| v as f64))
        .or_else(|| value.as_u64().map(|v| v as f64))?;
    let raw = (unit * 10f64.powi(-i32::from(control.exponent))).round();
    (raw.is_finite() && raw >= f64::from(control.min) && raw <= f64::from(control.max))
        .then_some(raw as i32)
}

/// Adds one telemetry entry per channel to `state`.
pub fn telemetry(
    mut state: DeviceStateSnapshot,
    info: &DeviceInfo,
    values: &[i32],
) -> DeviceStateSnapshot {
    for (channel, raw) in info.channels.iter().zip(values) {
        state = state.with_telemetry(channel.name, channel_value(channel, *raw));
    }
    state
}

/// Adds the class, model, and each channel's and control's quantity and
/// unit (`channel.<name>.unit`, `control.<name>.unit`) to a descriptor.
pub fn describe(
    mut builder: DeviceDescriptorBuilder,
    info: &DeviceInfo,
) -> DeviceDescriptorBuilder {
    builder = builder
        .label(PROPERTY_CLASS, info.class.name())
        .property(PROPERTY_CLASS, info.class.name())
        .property(PROPERTY_MODEL, info.model);
    for channel in info.channels {
        builder = builder
            .property(
                format!("channel.{}.quantity", channel.name),
                channel.quantity.name(),
            )
            .property(
                format!("channel.{}.unit", channel.name),
                channel.unit().symbol(),
            );
    }
    for control in info.controls {
        builder = builder
            .property(
                format!("control.{}.quantity", control.name),
                control.quantity.name(),
            )
            .property(
                format!("control.{}.unit", control.name),
                control.unit().symbol(),
            )
            .property(
                format!("control.{}.min", control.name),
                control_value(control, control.min),
            )
            .property(
                format!("control.{}.max", control.name),
                control_value(control, control.max),
            );
    }
    builder
}

/// The custom interactions an L1 device offers: [`READ_INTERACTION`] for a
/// sensor, `<control>.set` and `<control>.get` per control.
pub fn interactions(info: &DeviceInfo) -> Result<Vec<CustomInteraction>, CoreError> {
    let mut interactions = Vec::new();
    if !info.channels.is_empty() {
        interactions.push(CustomInteraction::new(
            READ_INTERACTION,
            "Read every channel in its unit",
        )?);
    }
    for control in info.controls {
        interactions.push(CustomInteraction::new(
            format!("{}.set", control.name),
            format!("Set {} ({})", control.name, control.unit().symbol()),
        )?);
        interactions.push(CustomInteraction::new(
            format!("{}.get", control.name),
            format!("Read {}", control.name),
        )?);
    }
    Ok(interactions)
}

/// `embedded_hal::delay::DelayNs` over `std::thread::sleep`, for `init`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SleepDelay;

impl embedded_hal::delay::DelayNs for SleepDelay {
    fn delay_ns(&mut self, ns: u32) {
        std::thread::sleep(Duration::from_nanos(u64::from(ns)));
    }
}

/// Any L1 device bound into the runtime.
pub struct L1BoundDevice {
    descriptor: DeviceDescriptor,
    driver_id: String,
    device: Mutex<BoxedDevice>,
    interactions: Vec<CustomInteraction>,
}

impl L1BoundDevice {
    /// Wraps `device` (already initialized, or call [`init`](Self::init)).
    pub fn new(
        driver_id: impl Into<String>,
        descriptor: DeviceDescriptor,
        device: BoxedDevice,
    ) -> DriverResult<Self> {
        let driver_id = driver_id.into();
        let interactions =
            interactions(device.info()).map_err(|source| DriverError::BindFailed {
                driver_id: driver_id.clone(),
                device_id: descriptor.id.clone(),
                reason: source.to_string(),
            })?;
        Ok(Self {
            descriptor,
            driver_id,
            device: Mutex::new(device),
            interactions,
        })
    }

    /// Runs the device's `init`.
    pub fn init(&mut self) -> DriverResult<()> {
        let result = self.device_mut().init(&mut SleepDelay);
        result.map_err(|kind| self.error("init", kind))
    }

    pub fn info(&mut self) -> &'static DeviceInfo {
        self.device_mut().info()
    }

    fn device_mut(&mut self) -> &mut BoxedDevice {
        // `&mut self` already excludes other users; the mutex only makes
        // the boxed device `Sync`.
        self.device
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn error(&self, action: &str, kind: ErrorKind) -> DriverError {
        DriverError::Device {
            driver_id: self.driver_id.clone(),
            device_id: self.descriptor.id.clone(),
            action: action.into(),
            kind,
        }
    }

    fn read(&mut self) -> DriverResult<(&'static DeviceInfo, [i32; MAX_CHANNELS])> {
        let mut values = [NO_VALUE; MAX_CHANNELS];
        let device = self.device_mut();
        let info = device.info();
        let result = device.read(&mut values);
        result.map_err(|kind| self.error("read", kind))?;
        Ok((info, values))
    }

    fn invalid(&self, interaction: &'static str, reason: impl Into<String>) -> DriverError {
        DriverError::InvalidRequest {
            driver_id: self.driver_id.clone(),
            device_id: self.descriptor.id.clone(),
            source: CoreError::InvalidRequest {
                request: interaction,
                reason: reason.into(),
            },
        }
    }

    fn respond(&self, id: &str, output: Value) -> DriverResult<InteractionResponse> {
        let interaction = self
            .interactions
            .iter()
            .find(|i| i.id.as_str() == id)
            .expect("interaction ids come from the table");
        Ok(InteractionResponse::Custom(
            CustomInteractionResponse::new(interaction.id.clone()).with_output(output),
        ))
    }
}

impl BoundDevice for L1BoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.descriptor
    }

    fn driver_id(&self) -> &str {
        &self.driver_id
    }

    fn custom_interactions(&self) -> &[CustomInteraction] {
        &self.interactions
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        let id = self.descriptor.id.clone();
        let device = self.device_mut();
        let info = device.info();
        let is_sensor = device.is_sensor();
        let mut state = DeviceStateSnapshot::new(id)
            .with_lifecycle(DeviceLifecycleState::Idle)
            .with_config(PROPERTY_CLASS, info.class.name())
            .with_config(PROPERTY_MODEL, info.model);
        if is_sensor {
            let (info, values) = self.read()?;
            state = telemetry(state, info, &values);
        }
        let device = self.device_mut();
        for (index, control) in info.controls.iter().enumerate() {
            // Controls a device cannot report (a VCM before its first move)
            // are left out.
            if let Ok(raw) = device.get(index) {
                state = state.with_config(control.name, control_value(control, raw));
            }
        }
        Ok(Some(state.with_health(DeviceHealth::Healthy)))
    }

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        let InteractionRequest::Custom(custom) = request else {
            return Err(crate::unsupported_action_error(
                &self.driver_id,
                &self.descriptor,
                request,
            ));
        };
        let id = custom.id.as_str();
        if id == READ_INTERACTION {
            let (info, values) = self.read()?;
            let map: ValueMap = info
                .channels
                .iter()
                .zip(values)
                .map(|(channel, raw)| (channel.name.to_string(), channel_value(channel, raw)))
                .collect();
            return self.respond(id, Value::from(map));
        }
        let info = self.device_mut().info();
        let found = info
            .controls
            .iter()
            .enumerate()
            .find_map(|(index, control)| {
                let name = id
                    .strip_suffix(".set")
                    .or_else(|| id.strip_suffix(".get"))?;
                (name == control.name).then_some((index, control))
            });
        let Some((index, control)) = found else {
            return Err(crate::unsupported_action_error(
                &self.driver_id,
                &self.descriptor,
                request,
            ));
        };
        let raw = if id.ends_with(".set") {
            let raw = custom
                .input
                .as_ref()
                .and_then(|value| control_raw(control, value))
                .ok_or_else(|| {
                    self.invalid(
                        "set",
                        format!(
                            "expected a number from {} to {} {}",
                            control_value(control, control.min)
                                .as_f64()
                                .unwrap_or_default(),
                            control_value(control, control.max)
                                .as_f64()
                                .unwrap_or_default(),
                            control.unit().symbol()
                        ),
                    )
                })?;
            let result = self.device_mut().set(index, raw);
            result.map_err(|kind| self.error("set", kind))?
        } else {
            let result = self.device_mut().get(index);
            result.map_err(|kind| self.error("get", kind))?
        };
        self.respond(id, control_value(control, raw))
    }
}

/// Builds the device for a descriptor at bind time.
pub type L1Factory =
    dyn Fn(&DeviceDescriptor, &DriverBindContext<'_>) -> DriverResult<BoxedDevice> + Send + Sync;

/// A runtime driver for L1 devices: it matches descriptors whose
/// [`PROPERTY_DRIVER`] is its id, builds the device with its factory, initializes it, and binds
/// it as an [`L1BoundDevice`].
pub struct L1Driver {
    id: String,
    interface: InterfaceKind,
    manifest: DriverManifest,
    factory: Box<L1Factory>,
}

impl L1Driver {
    pub fn new(
        id: impl Into<String>,
        summary: impl Into<String>,
        interface: InterfaceKind,
        factory: impl Fn(&DeviceDescriptor, &DriverBindContext<'_>) -> DriverResult<BoxedDevice>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let id = id.into();
        let manifest = DriverManifest::new(id.clone(), summary, vec![interface])
            .with_priority(DriverPriority::Exact)
            .with_custom_interaction(READ_INTERACTION, "Read every channel in its unit")
            .with_rule(
                MatchRule::new(1000)
                    .described("device configured for this driver")
                    .require(MatchCondition::PropertyEq {
                        key: PROPERTY_DRIVER.into(),
                        value: Value::from(id.clone()),
                    }),
            )
            .with_tag("lemnos-device");
        Self {
            id,
            interface,
            manifest,
            factory: Box::new(factory),
        }
    }
}

impl L1Driver {
    /// Matches devices by `rule` instead of by [`PROPERTY_DRIVER`], for
    /// devices a backend discovers (thermal zones).
    pub fn matching(mut self, rule: MatchRule) -> Self {
        let mut manifest = DriverManifest::new(
            self.manifest.id.clone(),
            self.manifest.summary.clone(),
            vec![self.interface],
        )
        .with_priority(DriverPriority::Exact)
        .with_custom_interaction(READ_INTERACTION, "Read every channel in its unit")
        .with_tag("lemnos-device");
        manifest = manifest.with_rule(rule);
        self.manifest = manifest;
        self
    }
}

impl Driver for L1Driver {
    fn id(&self) -> &str {
        &self.id
    }

    fn interface(&self) -> InterfaceKind {
        self.interface
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(self.manifest.clone())
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        let boxed = (self.factory)(device, context)?;
        let mut bound = L1BoundDevice::new(self.id.clone(), device.clone(), boxed)?;
        bound.init()?;
        Ok(Box::new(bound))
    }
}
