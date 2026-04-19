#[cfg(feature = "serde")]
use crate::request_serde::*;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GpioLevel {
    Low,
    High,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GpioDirection {
    Input,
    Output,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GpioBias {
    Disabled,
    PullUp,
    PullDown,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GpioDrive {
    PushPull,
    OpenDrain,
    OpenSource,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GpioEdge {
    Rising,
    Falling,
    Both,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioLineConfiguration {
    pub direction: GpioDirection,
    pub active_low: bool,
    pub bias: Option<GpioBias>,
    pub drive: Option<GpioDrive>,
    pub edge: Option<GpioEdge>,
    pub debounce_us: Option<u32>,
    pub initial_level: Option<GpioLevel>,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpioRequest {
    Read,
    Write { level: GpioLevel },
    Configure(GpioLineConfiguration),
    GetConfiguration,
}

impl GpioRequest {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Read => "gpio.read",
            Self::Write { .. } => "gpio.write",
            Self::Configure(_) => "gpio.configure",
            Self::GetConfiguration => "gpio.get_configuration",
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpioResponse {
    Level(GpioLevel),
    Configuration(GpioLineConfiguration),
    Applied,
}
