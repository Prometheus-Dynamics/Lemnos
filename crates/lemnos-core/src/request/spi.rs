#[cfg(feature = "serde")]
use crate::request_serde::*;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpiMode {
    Mode0,
    Mode1,
    Mode2,
    Mode3,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpiBitOrder {
    MsbFirst,
    LsbFirst,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiConfiguration {
    pub mode: SpiMode,
    pub max_frequency_hz: Option<u32>,
    pub bits_per_word: Option<u8>,
    pub bit_order: SpiBitOrder,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpiRequest {
    Transfer { write: Vec<u8> },
    Write { bytes: Vec<u8> },
    Configure(SpiConfiguration),
    GetConfiguration,
}

impl SpiRequest {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Transfer { .. } => "spi.transfer",
            Self::Write { .. } => "spi.write",
            Self::Configure(_) => "spi.configure",
            Self::GetConfiguration => "spi.get_configuration",
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpiResponse {
    Bytes(Vec<u8>),
    Configuration(SpiConfiguration),
    Applied,
}
