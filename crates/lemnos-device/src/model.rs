//! The plain-data description of a device: classes, quantities, units,
//! channels, controls and status. Everything here is `Copy` and `'static`, so
//! a driver describes itself with read-only tables and no code.

use core::fmt;
use lemnos_hal::ErrorKind;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// The value a channel reads when the device has no valid measurement for it,
/// such as a magnetometer axis that overflowed or a die temperature the model
/// does not measure.
pub const NO_VALUE: i32 = i32::MIN;

/// What a device is.
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceClass {
    /// Accelerometer and gyroscope together.
    Imu,
    Accelerometer,
    Gyroscope,
    Magnetometer,
    /// Voltage, current and power.
    PowerMonitor,
    /// One or more temperatures (thermal zones, die sensors).
    Temperature,
    Fan,
    /// A camera focus lens actuator.
    Lens,
    /// LEDs: a single LED or an addressable strip.
    Light,
    /// A digital input or output line.
    Gpio,
    /// An orientation estimate (typically computed from other devices).
    Orientation,
    Other,
}

impl DeviceClass {
    /// The kebab-case name used in telemetry and configuration.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Imu => "imu",
            Self::Accelerometer => "accelerometer",
            Self::Gyroscope => "gyroscope",
            Self::Magnetometer => "magnetometer",
            Self::PowerMonitor => "power-monitor",
            Self::Temperature => "temperature",
            Self::Fan => "fan",
            Self::Lens => "lens",
            Self::Light => "light",
            Self::Gpio => "gpio",
            Self::Orientation => "orientation",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for DeviceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a channel measures or a control sets. Each quantity has one
/// canonical [`Unit`], so a reading means the same thing whichever driver or
/// backend produced it.
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Quantity {
    /// m/s².
    Acceleration,
    /// rad/s.
    AngularRate,
    /// T.
    MagneticField,
    /// V.
    Voltage,
    /// A.
    Current,
    /// W.
    Power,
    /// °C.
    Temperature,
    /// rpm.
    RotationalSpeed,
    /// A dimensionless fraction (duty cycle, brightness): 1 is 100 %.
    Ratio,
    /// A position in device steps (a lens actuator code).
    Position,
    /// A digital level: 0 or 1.
    Level,
    /// A device-specific mode number.
    Mode,
    /// A packed `0xRRGGBB` colour.
    Color,
    /// rad.
    Angle,
    /// Pa.
    Pressure,
    /// Hz.
    Frequency,
}

impl Quantity {
    /// The kebab-case name used in telemetry and configuration.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Acceleration => "acceleration",
            Self::AngularRate => "angular-rate",
            Self::MagneticField => "magnetic-field",
            Self::Voltage => "voltage",
            Self::Current => "current",
            Self::Power => "power",
            Self::Temperature => "temperature",
            Self::RotationalSpeed => "rotational-speed",
            Self::Ratio => "ratio",
            Self::Position => "position",
            Self::Level => "level",
            Self::Mode => "mode",
            Self::Color => "color",
            Self::Angle => "angle",
            Self::Pressure => "pressure",
            Self::Frequency => "frequency",
        }
    }

    /// The canonical unit of this quantity.
    pub const fn unit(self) -> Unit {
        match self {
            Self::Acceleration => Unit::MetrePerSecondSquared,
            Self::AngularRate => Unit::RadianPerSecond,
            Self::MagneticField => Unit::Tesla,
            Self::Voltage => Unit::Volt,
            Self::Current => Unit::Ampere,
            Self::Power => Unit::Watt,
            Self::Temperature => Unit::DegreeCelsius,
            Self::RotationalSpeed => Unit::RevolutionPerMinute,
            Self::Angle => Unit::Radian,
            Self::Pressure => Unit::Pascal,
            Self::Frequency => Unit::Hertz,
            Self::Ratio | Self::Position | Self::Level | Self::Mode | Self::Color => Unit::One,
        }
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The unit of a value before its decimal exponent.
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Unit {
    MetrePerSecondSquared,
    RadianPerSecond,
    Tesla,
    Volt,
    Ampere,
    Watt,
    DegreeCelsius,
    RevolutionPerMinute,
    Radian,
    Pascal,
    Hertz,
    /// Dimensionless: ratios, steps, levels, modes, colours.
    One,
}

impl Unit {
    /// The unit's symbol (empty for [`Unit::One`]).
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::MetrePerSecondSquared => "m/s²",
            Self::RadianPerSecond => "rad/s",
            Self::Tesla => "T",
            Self::Volt => "V",
            Self::Ampere => "A",
            Self::Watt => "W",
            Self::DegreeCelsius => "°C",
            Self::RevolutionPerMinute => "rpm",
            Self::Radian => "rad",
            Self::Pascal => "Pa",
            Self::Hertz => "Hz",
            Self::One => "",
        }
    }
}

/// The axis a channel belongs to.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Axis {
    #[default]
    None,
    X,
    Y,
    Z,
}

impl Axis {
    /// `"x"`, `"y"`, `"z"`, or `None`.
    pub const fn name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::X => Some("x"),
            Self::Y => Some("y"),
            Self::Z => Some("z"),
        }
    }
}

/// One value a device reads: `raw × 10^exponent` in the quantity's unit.
///
/// `name` is the stable key telemetry uses (`"acceleration.x"`,
/// `"bus_voltage"`); it is unique within a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Channel {
    pub name: &'static str,
    pub quantity: Quantity,
    pub axis: Axis,
    pub exponent: i8,
}

impl Channel {
    pub const fn new(name: &'static str, quantity: Quantity, exponent: i8) -> Self {
        Self {
            name,
            quantity,
            axis: Axis::None,
            exponent,
        }
    }

    /// The same channel on `axis`.
    pub const fn on(mut self, axis: Axis) -> Self {
        self.axis = axis;
        self
    }

    /// The canonical unit of the channel's quantity.
    pub const fn unit(&self) -> Unit {
        self.quantity.unit()
    }

    /// Converts a raw reading to the unit, or `None` for [`NO_VALUE`].
    #[cfg(feature = "float")]
    pub fn to_f32(&self, raw: i32) -> Option<f32> {
        (raw != NO_VALUE).then(|| crate::fixed::to_f32(raw, self.exponent))
    }
}

/// One value a device accepts: `raw × 10^exponent` in the quantity's unit,
/// within `min..=max` raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlInfo {
    pub name: &'static str,
    pub quantity: Quantity,
    pub exponent: i8,
    pub min: i32,
    pub max: i32,
}

impl ControlInfo {
    pub const fn new(
        name: &'static str,
        quantity: Quantity,
        exponent: i8,
        min: i32,
        max: i32,
    ) -> Self {
        Self {
            name,
            quantity,
            exponent,
            min,
            max,
        }
    }

    /// The canonical unit of the control's quantity.
    pub const fn unit(&self) -> Unit {
        self.quantity.unit()
    }

    /// Whether `value` is within `min..=max`.
    pub const fn accepts(&self, value: i32) -> bool {
        value >= self.min && value <= self.max
    }

    /// Converts a value in the unit to raw, rounded; `None` when it falls
    /// outside `min..=max` or is not finite.
    #[cfg(feature = "float")]
    pub fn from_f32(&self, value: f32) -> Option<i32> {
        let raw = crate::fixed::from_f32(value, self.exponent)?;
        self.accepts(raw).then_some(raw)
    }
}

/// A static description of a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    pub class: DeviceClass,
    /// The part, such as `"BMI088"`.
    pub model: &'static str,
    /// What `Sensor::read` fills, in order.
    pub channels: &'static [Channel],
    /// What `Control::set` accepts, by index.
    pub controls: &'static [ControlInfo],
}

impl DeviceInfo {
    pub const fn new(
        class: DeviceClass,
        model: &'static str,
        channels: &'static [Channel],
        controls: &'static [ControlInfo],
    ) -> Self {
        Self {
            class,
            model,
            channels,
            controls,
        }
    }

    /// The index of the channel called `name`.
    pub fn channel_index(&self, name: &str) -> Option<usize> {
        self.channels.iter().position(|c| c.name == name)
    }

    /// The index of the control called `name`.
    pub fn control_index(&self, name: &str) -> Option<usize> {
        self.controls.iter().position(|c| c.name == name)
    }
}

/// Coarse single-value summary of a device's condition, shared by every
/// Lemnos layer (`lemnos_core::DeviceStatus` is this type). Variants are
/// ordered from best to worst, so `max` picks the worse of two statuses.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DeviceStatus {
    /// Present and usable.
    #[default]
    Available,
    /// Present but running with warnings or incomplete information.
    Degraded,
    /// Present but its driver or last operation failed.
    Faulted,
    /// Removed, offline, or otherwise not reachable.
    Missing,
}

impl DeviceStatus {
    /// The status a failed operation with `kind` leaves a device in:
    /// [`Missing`](Self::Missing) when it is gone or unreachable,
    /// [`Degraded`](Self::Degraded) for transient faults, and
    /// [`Faulted`](Self::Faulted) otherwise.
    pub fn after_error(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::NotFound | ErrorKind::Nack => Self::Missing,
            kind if kind.is_transient() => Self::Degraded,
            _ => Self::Faulted,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Degraded => "degraded",
            Self::Faulted => "faulted",
            Self::Missing => "missing",
        }
    }
}
