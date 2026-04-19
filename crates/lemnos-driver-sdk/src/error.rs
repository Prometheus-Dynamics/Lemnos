use lemnos_bus::BusError;
use lemnos_core::{CoreError, DeviceId, InterfaceKind};
use std::io;
use thiserror::Error;

pub type DriverResult<T> = Result<T, DriverError>;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("driver '{driver_id}' requires a backend for interface '{interface}'")]
    MissingBackend {
        driver_id: String,
        interface: InterfaceKind,
    },
    #[error("driver '{driver_id}' rejected device '{device_id}': {reason}")]
    BindRejected {
        driver_id: String,
        device_id: DeviceId,
        reason: String,
    },
    #[error("driver '{driver_id}' failed to bind device '{device_id}': {reason}")]
    BindFailed {
        driver_id: String,
        device_id: DeviceId,
        reason: String,
    },
    #[error("driver '{driver_id}' received an invalid request for device '{device_id}': {source}")]
    InvalidRequest {
        driver_id: String,
        device_id: DeviceId,
        #[source]
        source: CoreError,
    },
    #[error("driver '{driver_id}' transport failure for device '{device_id}': {source}")]
    Transport {
        driver_id: String,
        device_id: DeviceId,
        #[source]
        source: BusError,
    },
    #[error(
        "driver '{driver_id}' host I/O failure for device '{device_id}' while {action}: {source}"
    )]
    HostIo {
        driver_id: String,
        device_id: DeviceId,
        action: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "driver '{driver_id}' reached an invalid internal state for device '{device_id}': {reason}"
    )]
    InvariantViolation {
        driver_id: String,
        device_id: DeviceId,
        reason: String,
    },
    #[error("driver '{driver_id}' does not support action '{action}' on device '{device_id}'")]
    UnsupportedAction {
        driver_id: String,
        device_id: DeviceId,
        action: String,
    },
    #[error("driver '{driver_id}' does not implement '{action}'")]
    NotImplemented { driver_id: String, action: String },
}
