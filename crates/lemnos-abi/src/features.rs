//! A surface's optional features, beside its version: a bit set the caller
//! asks for once, after the version handshake, through the surface's own
//! `<prefix>_features()` call returning a `u64`. Kept out of [`AbiVersion`]
//! so the handshake's layout, and every surface's layout hash, stay as they
//! are.
//!
//! [`AbiVersion`]: crate::AbiVersion

use crate::Status;

/// A set of up to 64 feature bits. Each surface names its own bits; a bit the
/// caller does not know is kept, never an error.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Features(pub u64);

impl Features {
    pub const NONE: Self = Self(0);

    /// The set with `bit` (0 to 63) added.
    pub const fn with(self, bit: u32) -> Self {
        Self(self.0 | (1 << bit))
    }

    /// Whether every bit of `wanted` is in this set.
    pub const fn contains(self, wanted: Self) -> bool {
        self.0 & wanted.0 == wanted.0
    }

    /// [`Status::OK`] when this set has every bit a caller requires, else
    /// [`Status::NOT_SUPPORTED`].
    pub const fn require(self, wanted: Self) -> Status {
        if self.contains(wanted) {
            Status::OK
        } else {
            Status::NOT_SUPPORTED
        }
    }
}
