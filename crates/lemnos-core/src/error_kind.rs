use std::io;

/// Stable, coarse classification of a Lemnos error.
///
/// Every Lemnos error type exposes `kind()` returning this enum, so callers
/// can map failures (to HTTP statuses, retry policy, resource action results)
/// without matching every variant or parsing messages. Wrapping errors
/// delegate to their source, so a bus timeout surfaced through a driver and
/// then the runtime still reports [`ErrorKind::Timeout`].
///
/// New kinds may be added in minor releases.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorKind {
    /// The device, driver, or file does not exist.
    NotFound,
    /// The device exists but cannot be used right now: disconnected, not
    /// bound, or the runtime is stopped.
    Unavailable,
    /// Another session or process holds the device.
    Busy,
    /// The OS or backend refused access.
    PermissionDenied,
    /// The operation did not finish in time.
    Timeout,
    /// The request or configuration is malformed or out of range.
    InvalidInput,
    /// No backend, driver, or action supports this operation.
    Unsupported,
    /// Drivers, manifests, or discovered descriptors conflict or are invalid.
    Configuration,
    /// The operation failed for another reason, such as a transport or
    /// internal error.
    Failed,
}

impl ErrorKind {
    /// Whether retrying the same operation later may succeed without any
    /// change in configuration.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Unavailable | Self::Busy | Self::Timeout)
    }

    /// Classifies a host I/O error.
    pub fn from_io(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
                Self::PermissionDenied
            }
            io::ErrorKind::TimedOut => Self::Timeout,
            io::ErrorKind::WouldBlock | io::ErrorKind::ResourceBusy => Self::Busy,
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => Self::InvalidInput,
            io::ErrorKind::Unsupported => Self::Unsupported,
            io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted => Self::Unavailable,
            _ => Self::Failed,
        }
    }
}

impl crate::CoreError {
    /// Every core error describes a malformed identifier, descriptor, or request.
    pub fn kind(&self) -> ErrorKind {
        ErrorKind::InvalidInput
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_io_errors() {
        assert_eq!(
            ErrorKind::from_io(io::ErrorKind::NotFound),
            ErrorKind::NotFound
        );
        assert_eq!(
            ErrorKind::from_io(io::ErrorKind::PermissionDenied),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            ErrorKind::from_io(io::ErrorKind::ResourceBusy),
            ErrorKind::Busy
        );
        assert_eq!(ErrorKind::from_io(io::ErrorKind::Other), ErrorKind::Failed);
        assert!(ErrorKind::Timeout.is_transient());
        assert!(!ErrorKind::InvalidInput.is_transient());
    }
}
