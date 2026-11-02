//! The error type every USB operation returns.

use std::fmt;
#[cfg(target_os = "linux")]
use std::io;

/// What went wrong, in terms a caller can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The user or process may not open the device, claim the interface or
    /// perform the request (a device-node permission, or a driver rule).
    PermissionDenied,
    /// The device, interface or endpoint is in use: another claim, another
    /// handle or a kernel driver.
    Busy,
    /// The device is gone: unplugged, reset away, or cut off mid-transfer.
    Disconnected,
    /// The platform, the device's driver or this request does not support the
    /// operation (isochronous transfers off Linux, for example).
    Unsupported,
    /// The device, interface, endpoint, configuration or location is not there.
    NotFound,
    /// The transfer's timeout passed.
    Timeout,
    /// The device stalled the endpoint: it refused the request.
    Stall,
    /// The transfer was cancelled.
    Cancelled,
    /// The request is malformed: an endpoint of zero, an empty payload, a
    /// packet layout that does not match the buffer.
    InvalidInput,
    /// Anything else. [`Error::os_code`] carries the OS error when there is one.
    Other,
}

impl ErrorKind {
    fn label(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission denied",
            Self::Busy => "busy",
            Self::Disconnected => "disconnected",
            Self::Unsupported => "unsupported",
            Self::NotFound => "not found",
            Self::Timeout => "timed out",
            Self::Stall => "stalled",
            Self::Cancelled => "cancelled",
            Self::InvalidInput => "invalid input",
            Self::Other => "failed",
        }
    }
}

/// A USB failure: a kind, a message that names the operation, and the OS
/// error code when the platform gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    os_code: Option<i32>,
}

/// The result type of this crate.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// A new error of `kind`, with `message` saying what was being done.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            os_code: None,
        }
    }

    /// The error's kind.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// What was being done when the error happened.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The OS error code (an errno on Linux and macOS, a Win32 error on
    /// Windows), when the platform reported one.
    pub fn os_code(&self) -> Option<i32> {
        self.os_code
    }

    /// Classifies an OS error raised while doing `context` (usbdevfs errnos).
    #[cfg(target_os = "linux")]
    pub(crate) fn from_io(context: &str, error: &io::Error) -> Self {
        let kind = match error.raw_os_error() {
            Some(libc_code) => kind_for_errno(libc_code),
            None => match error.kind() {
                io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
                io::ErrorKind::NotFound => ErrorKind::NotFound,
                io::ErrorKind::TimedOut => ErrorKind::Timeout,
                io::ErrorKind::InvalidInput => ErrorKind::InvalidInput,
                io::ErrorKind::Unsupported => ErrorKind::Unsupported,
                _ => ErrorKind::Other,
            },
        };
        Self {
            kind,
            message: format!("{context}: {error}"),
            os_code: error.raw_os_error(),
        }
    }

    /// The error a negative URB status (an errno) stands for.
    #[cfg(target_os = "linux")]
    pub(crate) fn from_status(context: &str, status: i32) -> Self {
        let error = io::Error::from_raw_os_error(-status);
        let mut classified = Self::from_io(context, &error);
        // The kernel completes a discarded URB with ENOENT (or ECONNRESET);
        // those are cancellations, not missing endpoints.
        if matches!(-status, libc::ECANCELED | libc::ENOENT | libc::ECONNRESET) {
            classified.kind = ErrorKind::Cancelled;
        }
        classified
    }

    /// An error from nusb (the Windows and macOS backend).
    pub(crate) fn from_nusb(context: &str, error: &nusb::Error) -> Self {
        let kind = match error.kind() {
            nusb::ErrorKind::Disconnected => ErrorKind::Disconnected,
            nusb::ErrorKind::Busy => ErrorKind::Busy,
            nusb::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            nusb::ErrorKind::NotFound => ErrorKind::NotFound,
            nusb::ErrorKind::Unsupported => ErrorKind::Unsupported,
            _ => ErrorKind::Other,
        };
        Self {
            kind,
            message: format!("{context}: {error}"),
            os_code: error.os_error().map(|code| code as i32),
        }
    }

    /// An error from a nusb transfer (the Windows and macOS backend).
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn from_transfer(context: &str, error: nusb::transfer::TransferError) -> Self {
        use nusb::transfer::TransferError;
        let kind = match error {
            TransferError::Cancelled => ErrorKind::Cancelled,
            TransferError::Stall => ErrorKind::Stall,
            TransferError::Disconnected => ErrorKind::Disconnected,
            TransferError::InvalidArgument => ErrorKind::InvalidInput,
            _ => ErrorKind::Other,
        };
        Self {
            kind,
            message: format!("{context}: {error}"),
            os_code: None,
        }
    }
}

/// The kind an errno stands for.
#[cfg(target_os = "linux")]
fn kind_for_errno(code: i32) -> ErrorKind {
    match code {
        libc::EACCES | libc::EPERM => ErrorKind::PermissionDenied,
        libc::EBUSY => ErrorKind::Busy,
        libc::ENODEV | libc::ESHUTDOWN | libc::ENOTCONN | libc::ENXIO => ErrorKind::Disconnected,
        libc::ENOENT | libc::ENOTTY => ErrorKind::NotFound,
        libc::ETIMEDOUT => ErrorKind::Timeout,
        libc::EPIPE => ErrorKind::Stall,
        libc::ECANCELED => ErrorKind::Cancelled,
        libc::EINVAL => ErrorKind::InvalidInput,
        // ENOTSUP and EOPNOTSUPP are one value on Linux.
        libc::ENOSYS | libc::EOPNOTSUPP => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.label(), self.message)
    }
}

impl std::error::Error for Error {}
