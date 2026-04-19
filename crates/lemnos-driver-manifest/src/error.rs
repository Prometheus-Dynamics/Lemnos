use lemnos_core::CoreError;
use thiserror::Error;

pub type ManifestResult<T> = Result<T, ManifestError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestError {
    #[error("driver manifest id cannot be empty")]
    EmptyDriverId,
    #[error("driver manifest id '{id}' contains invalid character '{invalid}'")]
    InvalidDriverId { id: String, invalid: char },
    #[error("driver manifest '{id}' must declare at least one interface")]
    MissingInterfaces { id: String },
    #[error("driver manifest '{id}' standard interaction '{interaction}' is invalid: {source}")]
    InvalidStandardInteraction {
        id: String,
        interaction: String,
        #[source]
        source: CoreError,
    },
    #[error("driver manifest '{id}' custom interaction '{interaction}' is invalid: {source}")]
    InvalidCustomInteraction {
        id: String,
        interaction: String,
        #[source]
        source: CoreError,
    },
    #[error("driver manifest '{id}' capability '{capability}' is invalid: {source}")]
    InvalidCapability {
        id: String,
        capability: String,
        #[source]
        source: CoreError,
    },
}
