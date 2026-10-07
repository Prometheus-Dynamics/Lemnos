use lemnos_hal::{ErrorKind, HalError};
use std::fmt;

/// A board definition that cannot be read, is invalid, or names a device
/// that cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardError {
    /// The file could not be read.
    Io(String),
    /// The text is not a board definition.
    Parse(String),
    /// The definition parsed but is inconsistent; every problem found.
    Invalid(Vec<String>),
    /// A device could not be built.
    Device {
        device: String,
        kind: ErrorKind,
        reason: String,
    },
}

impl BoardError {
    pub(crate) fn device(device: &str, kind: ErrorKind, reason: impl Into<String>) -> Self {
        Self::Device {
            device: device.into(),
            kind,
            reason: reason.into(),
        }
    }
}

impl HalError for BoardError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Io(_) => ErrorKind::NotFound,
            Self::Parse(_) | Self::Invalid(_) => ErrorKind::Configuration,
            Self::Device { kind, .. } => *kind,
        }
    }
}

impl fmt::Display for BoardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(reason) => write!(f, "board definition: {reason}"),
            Self::Parse(reason) => write!(f, "board definition does not parse: {reason}"),
            Self::Invalid(problems) => {
                write!(f, "board definition is invalid: {}", problems.join("; "))
            }
            Self::Device {
                device,
                kind,
                reason,
            } => write!(f, "device {device:?} ({kind}): {reason}"),
        }
    }
}

impl std::error::Error for BoardError {}
