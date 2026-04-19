#[cfg(feature = "serde")]
use crate::request_serde::*;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I2cOperation {
    Read { length: u32 },
    Write { bytes: Vec<u8> },
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I2cRequest {
    Read { length: u32 },
    Write { bytes: Vec<u8> },
    WriteRead { write: Vec<u8>, read_length: u32 },
    Transaction { operations: Vec<I2cOperation> },
}

impl I2cRequest {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Read { .. } => "i2c.read",
            Self::Write { .. } => "i2c.write",
            Self::WriteRead { .. } => "i2c.write_read",
            Self::Transaction { .. } => "i2c.transaction",
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I2cTransactionResult {
    Read(Vec<u8>),
    Write { bytes_written: u32 },
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I2cResponse {
    Bytes(Vec<u8>),
    Transaction(Vec<I2cTransactionResult>),
    Applied,
}
