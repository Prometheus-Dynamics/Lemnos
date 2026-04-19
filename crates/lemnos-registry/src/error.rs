use crate::DriverId;
use lemnos_core::DeviceId;
use lemnos_driver_manifest::ManifestError;
use thiserror::Error;

pub type RegistryResult<T> = Result<T, RegistryError>;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("driver '{driver_id}' is already registered")]
    DuplicateDriverId { driver_id: DriverId },
    #[error("driver '{driver_id}' manifest is invalid: {source}")]
    InvalidManifest {
        driver_id: DriverId,
        #[source]
        source: Box<ManifestError>,
    },
    #[error("preferred driver '{driver_id}' is not registered")]
    UnknownPreferredDriver { driver_id: DriverId },
    #[error("device '{device_id}' has no matching drivers")]
    NoMatchingDriver { device_id: DeviceId },
    #[error("preferred driver '{driver_id}' does not match device '{device_id}'")]
    PreferredDriverDidNotMatch {
        device_id: DeviceId,
        driver_id: DriverId,
    },
    #[error("device '{device_id}' has conflicting top matches: {driver_ids:?}")]
    ConflictingMatches {
        device_id: DeviceId,
        driver_ids: Vec<DriverId>,
    },
}
