use lemnos_core::{DeviceId, ErrorKind, InterfaceKind};
use thiserror::Error;

pub type BusResult<T> = Result<T, BusError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BusError {
    #[error("backend '{backend}' does not support interface '{interface}'")]
    UnsupportedInterface {
        backend: String,
        interface: InterfaceKind,
    },
    #[error("backend '{backend}' does not support device '{device_id}'")]
    UnsupportedDevice {
        backend: String,
        device_id: DeviceId,
    },
    #[error("device '{device_id}' access conflict: {reason}")]
    AccessConflict { device_id: DeviceId, reason: String },
    #[error("device '{device_id}' session is not available: {reason}")]
    SessionUnavailable { device_id: DeviceId, reason: String },
    #[error("device '{device_id}' transport failure during '{operation}': {reason}")]
    TransportFailure {
        device_id: DeviceId,
        operation: &'static str,
        reason: String,
    },
    #[error("device '{device_id}' request '{operation}' timed out")]
    Timeout {
        device_id: DeviceId,
        operation: &'static str,
    },
    #[error("device '{device_id}' is disconnected")]
    Disconnected { device_id: DeviceId },
    #[error("device '{device_id}' request '{operation}' is invalid: {reason}")]
    InvalidRequest {
        device_id: DeviceId,
        operation: &'static str,
        reason: String,
    },
    #[error("device '{device_id}' configuration is invalid: {reason}")]
    InvalidConfiguration { device_id: DeviceId, reason: String },
    #[error("device '{device_id}' operation '{operation}' was denied: {reason}")]
    PermissionDenied {
        device_id: DeviceId,
        operation: &'static str,
        reason: String,
    },
}

impl BusError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::UnsupportedInterface { .. } | Self::UnsupportedDevice { .. } => {
                ErrorKind::Unsupported
            }
            Self::AccessConflict { .. } => ErrorKind::Busy,
            Self::SessionUnavailable { .. } | Self::Disconnected { .. } => ErrorKind::Unavailable,
            Self::TransportFailure { .. } => ErrorKind::Failed,
            Self::Timeout { .. } => ErrorKind::Timeout,
            Self::InvalidRequest { .. } | Self::InvalidConfiguration { .. } => {
                ErrorKind::InvalidInput
            }
            Self::PermissionDenied { .. } => ErrorKind::PermissionDenied,
        }
    }
}

impl embedded_hal::i2c::Error for BusError {
    fn kind(&self) -> embedded_hal::i2c::ErrorKind {
        BusError::kind(self).to_i2c()
    }
}

impl embedded_hal::spi::Error for BusError {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        BusError::kind(self).to_spi()
    }
}

impl embedded_hal::digital::Error for BusError {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}

impl embedded_hal::pwm::Error for BusError {
    fn kind(&self) -> embedded_hal::pwm::ErrorKind {
        embedded_hal::pwm::ErrorKind::Other
    }
}

impl lemnos_hal::HalError for BusError {
    fn kind(&self) -> ErrorKind {
        BusError::kind(self)
    }
}
