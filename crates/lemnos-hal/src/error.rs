use core::fmt;

/// Stable, coarse classification of a Lemnos error.
///
/// Every Lemnos error type exposes `kind()` returning this enum, so callers
/// can map failures (to HTTP statuses, retry policy, resource action results)
/// without matching every variant or parsing messages. Wrapping errors
/// delegate to their source, so a bus timeout surfaced through a driver and
/// then the runtime still reports [`ErrorKind::Timeout`].
///
/// `lemnos_core::ErrorKind` is this type. New kinds may be added in minor
/// releases.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorKind {
    /// The device, driver, role, or file does not exist.
    NotFound,
    /// The device exists but cannot be used right now: disconnected, not
    /// bound, or the runtime is stopped.
    Unavailable,
    /// Another session, process, or bus master holds the device.
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
    /// An I2C address or data byte was not acknowledged.
    Nack,
    /// Data was lost because a buffer or FIFO overflowed.
    Overrun,
    /// The operation failed for another reason, such as a transport or
    /// internal error.
    Failed,
}

impl ErrorKind {
    /// Whether retrying the same operation later may succeed without any
    /// change in configuration.
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Busy | Self::Timeout | Self::Nack | Self::Overrun
        )
    }

    /// Classifies an embedded-hal I2C error kind.
    pub fn from_i2c(kind: embedded_hal::i2c::ErrorKind) -> Self {
        use embedded_hal::i2c::ErrorKind as K;
        match kind {
            K::NoAcknowledge(_) => Self::Nack,
            K::ArbitrationLoss => Self::Busy,
            K::Overrun => Self::Overrun,
            K::Bus | K::Other => Self::Failed,
            _ => Self::Failed,
        }
    }

    /// Classifies an embedded-hal SPI error kind.
    pub fn from_spi(kind: embedded_hal::spi::ErrorKind) -> Self {
        use embedded_hal::spi::ErrorKind as K;
        match kind {
            K::Overrun => Self::Overrun,
            K::ModeFault | K::FrameFormat => Self::Configuration,
            K::ChipSelectFault | K::Other => Self::Failed,
            _ => Self::Failed,
        }
    }

    /// Classifies an embedded-hal digital (pin) error kind.
    pub fn from_digital(_kind: embedded_hal::digital::ErrorKind) -> Self {
        Self::Failed
    }

    /// The embedded-hal I2C error kind closest to this one.
    pub fn to_i2c(self) -> embedded_hal::i2c::ErrorKind {
        use embedded_hal::i2c::{ErrorKind as K, NoAcknowledgeSource};
        match self {
            Self::Nack => K::NoAcknowledge(NoAcknowledgeSource::Unknown),
            Self::Busy => K::ArbitrationLoss,
            Self::Overrun => K::Overrun,
            _ => K::Other,
        }
    }

    /// The embedded-hal SPI error kind closest to this one.
    pub fn to_spi(self) -> embedded_hal::spi::ErrorKind {
        use embedded_hal::spi::ErrorKind as K;
        match self {
            Self::Overrun => K::Overrun,
            _ => K::Other,
        }
    }

    /// A short lowercase name (`"not found"`, `"timeout"`, ...).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not found",
            Self::Unavailable => "unavailable",
            Self::Busy => "busy",
            Self::PermissionDenied => "permission denied",
            Self::Timeout => "timeout",
            Self::InvalidInput => "invalid input",
            Self::Unsupported => "unsupported",
            Self::Configuration => "configuration error",
            Self::Nack => "not acknowledged",
            Self::Overrun => "overrun",
            Self::Failed => "failed",
        }
    }
}

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "std")]
impl ErrorKind {
    /// Classifies a host I/O error.
    pub fn from_io(kind: std::io::ErrorKind) -> Self {
        use std::io::ErrorKind as K;
        match kind {
            K::NotFound => Self::NotFound,
            K::PermissionDenied | K::ReadOnlyFilesystem => Self::PermissionDenied,
            K::TimedOut => Self::Timeout,
            K::WouldBlock | K::ResourceBusy => Self::Busy,
            K::InvalidInput | K::InvalidData => Self::InvalidInput,
            K::Unsupported => Self::Unsupported,
            K::NotConnected | K::BrokenPipe | K::ConnectionReset | K::ConnectionAborted => {
                Self::Unavailable
            }
            _ => Self::Failed,
        }
    }

    /// Classifies a Linux `errno` (the kernel's I2C, SPI and GPIO error codes).
    pub fn from_errno(errno: i32) -> Self {
        match errno {
            // ENXIO, EREMOTEIO: the target did not acknowledge (i2c-dev reports both).
            6 | 121 => Self::Nack,
            // ENODEV, ESHUTDOWN: the device went away.
            19 | 108 => Self::Unavailable,
            // EAGAIN with I2C means arbitration lost.
            11 => Self::Busy,
            // EOVERFLOW.
            75 => Self::Overrun,
            // EOPNOTSUPP, ENOTTY, ENOSYS.
            95 | 25 | 38 => Self::Unsupported,
            _ => Self::from_io(std::io::Error::from_raw_os_error(errno).kind()),
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::ErrorKind> for ErrorKind {
    fn from(kind: std::io::ErrorKind) -> Self {
        Self::from_io(kind)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl core::error::Error for ErrorKind {}

impl embedded_hal::i2c::Error for ErrorKind {
    fn kind(&self) -> embedded_hal::i2c::ErrorKind {
        self.to_i2c()
    }
}

impl embedded_hal::spi::Error for ErrorKind {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        self.to_spi()
    }
}

impl embedded_hal::digital::Error for ErrorKind {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}

/// An error that knows its [`ErrorKind`].
///
/// Implemented by every Lemnos HAL error. Traits whose default methods must
/// produce an error (for example [`Regulator::set_voltage_uv`]) also require
/// `From<ErrorKind>`.
///
/// [`Regulator::set_voltage_uv`]: crate::power::Regulator::set_voltage_uv
pub trait HalError: fmt::Debug {
    /// The coarse classification of this error.
    fn kind(&self) -> ErrorKind;

    /// Writes a human-readable account of the error, for hosts that show
    /// why a device failed. Defaults to the `Debug` form; errors with a
    /// `Display` form should write that.
    fn describe(&self, f: &mut dyn fmt::Write) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl HalError for ErrorKind {
    fn kind(&self) -> ErrorKind {
        *self
    }
}

impl HalError for core::convert::Infallible {
    fn kind(&self) -> ErrorKind {
        match *self {}
    }
}

impl<E: HalError + ?Sized> HalError for &E {
    fn kind(&self) -> ErrorKind {
        (**self).kind()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_embedded_hal_kinds() {
        use embedded_hal::i2c::{ErrorKind as I, NoAcknowledgeSource};
        assert_eq!(
            ErrorKind::from_i2c(I::NoAcknowledge(NoAcknowledgeSource::Address)),
            ErrorKind::Nack
        );
        assert_eq!(ErrorKind::from_i2c(I::ArbitrationLoss), ErrorKind::Busy);
        assert_eq!(
            ErrorKind::from_i2c(ErrorKind::Nack.to_i2c()),
            ErrorKind::Nack
        );
        assert_eq!(
            ErrorKind::from_spi(embedded_hal::spi::ErrorKind::Overrun),
            ErrorKind::Overrun
        );
        assert!(ErrorKind::Nack.is_transient());
        assert!(!ErrorKind::InvalidInput.is_transient());
    }

    #[cfg(feature = "std")]
    #[test]
    fn classifies_io_errors_and_errnos() {
        use std::io;
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
        assert_eq!(ErrorKind::from_errno(121), ErrorKind::Nack);
        assert_eq!(ErrorKind::from_errno(16), ErrorKind::Busy);
        assert_eq!(ErrorKind::from_errno(19), ErrorKind::Unavailable);
        assert!(ErrorKind::Timeout.is_transient());
    }
}
