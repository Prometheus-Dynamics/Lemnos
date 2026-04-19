use lemnos_core::{DeviceId, InterfaceKind};
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
