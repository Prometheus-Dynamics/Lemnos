#[cfg(feature = "serde")]
use crate::request_serde::*;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PwmPolarity {
    Normal,
    Inversed,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PwmConfiguration {
    pub period_ns: u64,
    pub duty_cycle_ns: u64,
    pub enabled: bool,
    pub polarity: PwmPolarity,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PwmRequest {
    Enable { enabled: bool },
    Configure(PwmConfiguration),
    SetPeriod { period_ns: u64 },
    SetDutyCycle { duty_cycle_ns: u64 },
    GetConfiguration,
}

impl PwmRequest {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Enable { .. } => "pwm.enable",
            Self::Configure(_) => "pwm.configure",
            Self::SetPeriod { .. } => "pwm.set_period",
            Self::SetDutyCycle { .. } => "pwm.set_duty_cycle",
            Self::GetConfiguration => "pwm.get_configuration",
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PwmResponse {
    Configuration(PwmConfiguration),
    Applied,
}
