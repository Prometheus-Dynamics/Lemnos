/// The result of a call across the boundary, as an `i32`.
///
/// - 0: success ([`Status::OK`]).
/// - -1 to -4095: a Linux errno, negated, as the kernel returns them
///   ([`Status::from_errno`]): a surface passes the kernel's error on as is.
/// - [`Status::LEMNOS_BASE`] and below: this crate's codes, which have no
///   errno.
/// - Positive: success with a count, where a call documents one.
///
/// Unknown values are valid: a peer built against a newer crate may return
/// codes this one does not name.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Status(pub i32);

impl Status {
    pub const OK: Self = Self(0);

    // The errnos a surface returns most, by name (Linux values).
    pub const NOT_PERMITTED: Self = Self(-1);
    pub const NOT_FOUND: Self = Self(-2);
    pub const INTERRUPTED: Self = Self(-4);
    pub const IO: Self = Self(-5);
    pub const AGAIN: Self = Self(-11);
    pub const NO_MEMORY: Self = Self(-12);
    pub const ACCESS: Self = Self(-13);
    pub const BUSY: Self = Self(-16);
    pub const NO_DEVICE: Self = Self(-19);
    pub const INVALID_ARGUMENT: Self = Self(-22);
    pub const NOT_SUPPORTED: Self = Self(-95);
    pub const TIMED_OUT: Self = Self(-110);

    /// The first of this crate's own codes; errnos stay above it.
    pub const LEMNOS_BASE: i32 = -4096;
    /// A handle that is null, was never issued, or was released.
    pub const STALE_HANDLE: Self = Self(Self::LEMNOS_BASE);
    /// The peer's [`AbiVersion`](crate::AbiVersion) does not match.
    pub const VERSION_MISMATCH: Self = Self(Self::LEMNOS_BASE - 1);
    /// An output buffer is too small; the call says where the size it needs is.
    pub const BUFFER_TOO_SMALL: Self = Self(Self::LEMNOS_BASE - 2);
    /// The entry point panicked ([`guard`](crate::guard)); its state may be
    /// half-updated, so the handle it was called on should be released.
    pub const PANIC: Self = Self(Self::LEMNOS_BASE - 3);

    /// Success, or success with a count.
    pub const fn is_ok(self) -> bool {
        self.0 >= 0
    }

    /// A negated Linux errno (`errno` positive, as `errno(3)` gives it).
    /// 0 and values out of the errno range map to [`Status::IO`].
    pub const fn from_errno(errno: i32) -> Self {
        if errno > 0 && errno < -Self::LEMNOS_BASE {
            Self(-errno)
        } else {
            Self::IO
        }
    }

    /// The errno, when this is one.
    pub const fn errno(self) -> Option<i32> {
        if self.0 < 0 && self.0 > Self::LEMNOS_BASE {
            Some(-self.0)
        } else {
            None
        }
    }

    /// The errno of an I/O error, or [`Status::IO`] when it carries none.
    #[cfg(feature = "std")]
    pub fn from_io_error(error: &std::io::Error) -> Self {
        error.raw_os_error().map_or(Self::IO, Self::from_errno)
    }

    /// `Ok(count)` for success, the status itself as the error.
    pub const fn into_result(self) -> Result<u32, Status> {
        if self.0 >= 0 {
            Ok(self.0 as u32)
        } else {
            Err(self)
        }
    }
}
