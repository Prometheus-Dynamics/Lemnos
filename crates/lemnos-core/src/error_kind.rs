//! [`ErrorKind`] lives in `lemnos-hal` (it is shared with the no_std drivers)
//! and is re-exported from this crate under its old path.

pub use lemnos_hal::ErrorKind;

impl crate::CoreError {
    /// Every core error describes a malformed identifier, descriptor, or request.
    pub fn kind(&self) -> ErrorKind {
        ErrorKind::InvalidInput
    }
}
