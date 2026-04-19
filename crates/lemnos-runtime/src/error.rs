use lemnos_core::{CoreError, DeviceId};
use lemnos_discovery::DiscoveryError;
use lemnos_driver_sdk::DriverError;
use lemnos_registry::RegistryError;
use thiserror::Error;

pub type RuntimeResult<T> = Result<T, RuntimeError>;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime is not running")]
    NotRunning,
    #[error("device '{device_id}' is not present in runtime inventory")]
    UnknownDevice { device_id: DeviceId },
    #[error("device '{device_id}' is not currently bound")]
    DeviceNotBound { device_id: DeviceId },
    #[error("request for device '{device_id}' is invalid: {source}")]
    InvalidRequest {
        device_id: DeviceId,
        #[source]
        source: Box<CoreError>,
    },
    #[error("driver operation failed for device '{device_id}': {source}")]
    Driver {
        device_id: DeviceId,
        #[source]
        source: Box<DriverError>,
    },
    #[error(transparent)]
    Discovery(Box<DiscoveryError>),
    #[error(transparent)]
    Registry(Box<RegistryError>),
}

impl From<DiscoveryError> for RuntimeError {
    fn from(error: DiscoveryError) -> Self {
        Self::Discovery(Box::new(error))
    }
}

impl From<RegistryError> for RuntimeError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(Box::new(error))
    }
}
