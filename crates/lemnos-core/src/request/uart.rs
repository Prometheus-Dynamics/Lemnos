#[cfg(feature = "serde")]
use crate::request_serde::*;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UartParity {
    None,
    Even,
    Odd,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UartStopBits {
    One,
    Two,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UartDataBits {
    Five,
    Six,
    Seven,
    Eight,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UartFlowControl {
    None,
    Software,
    Hardware,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UartConfiguration {
    pub baud_rate: u32,
    pub data_bits: UartDataBits,
    pub parity: UartParity,
    pub stop_bits: UartStopBits,
    pub flow_control: UartFlowControl,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UartRequest {
    Read { max_bytes: u32 },
    Write { bytes: Vec<u8> },
    Configure(UartConfiguration),
    Flush,
    GetConfiguration,
}

impl UartRequest {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Read { .. } => "uart.read",
            Self::Write { .. } => "uart.write",
            Self::Configure(_) => "uart.configure",
            Self::Flush => "uart.flush",
            Self::GetConfiguration => "uart.get_configuration",
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UartResponse {
    Bytes(Vec<u8>),
    Configuration(UartConfiguration),
    Applied,
}
