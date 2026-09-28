use lemnos_core::{CoreError, DeviceId, ErrorKind};
use thiserror::Error;

pub type DiscoveryResult<T> = Result<T, DiscoveryError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DiscoveryError {
    #[error("probe '{probe}' failed: {message}")]
    ProbeFailed { probe: String, message: String },
    #[error("watcher '{watcher}' failed: {message}")]
    WatchFailed { watcher: String, message: String },
    #[error("probe '{probe}' produced an invalid descriptor '{device_id}': {source}")]
    InvalidDescriptor {
        probe: String,
        device_id: DeviceId,
        #[source]
        source: CoreError,
    },
    #[error("discovery produced duplicate device id '{device_id}'")]
    DuplicateDeviceId { device_id: DeviceId },
}

impl DiscoveryError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::ProbeFailed { .. } | Self::WatchFailed { .. } => ErrorKind::Failed,
            Self::InvalidDescriptor { .. } | Self::DuplicateDeviceId { .. } => {
                ErrorKind::Configuration
            }
        }
    }
}
