//! The Lemnos socket protocol. `docs/wire.md` is its specification.
//!
//! A frame names a device, a contract position and a method number, and its
//! arguments are bytes this crate never reads. Nothing here names a kind of
//! device.
//!
//! Encoding and decoding allocate nothing on the paths used in steady state:
//! a `Frame` borrows from the bytes it was read from.
//!
//! With no features, only [`ring`]: the shared ring of a frame stream, with no part of the
//! engine behind it, for a program that maps a stream's memory itself. The `protocol` feature
//! (on by default) adds the rest.

/// Declares constants that are part of the wire layout, and a function that folds each name and
/// value into a layout hash. A constant that is not declared through this macro is not in the
/// hash, so every layout constant goes through it.
macro_rules! constants {
    ($($(#[$doc:meta])* pub const $name:ident: $ty:ty = $value:expr;)*) => {
        $($(#[$doc])* pub const $name: $ty = $value;)*

        /// Folds every constant of this module, by name and value, into `hash`.
        #[allow(clippy::cast_lossless)]
        #[cfg_attr(not(feature = "protocol"), allow(dead_code))]
        pub(crate) const fn fold_constants(mut hash: u64) -> u64 {
            $(
                hash = $crate::hash::text(hash, stringify!($name));
                hash = $crate::hash::word(hash, $name as u64);
            )*
            hash
        }
    };
}

// Without the protocol only the ring's constants are folded, and nothing reads the hash.
#[cfg_attr(not(feature = "protocol"), allow(dead_code))]
mod hash;
#[cfg(feature = "protocol")]
mod protocol;
pub mod ring;
#[cfg(feature = "protocol")]
pub mod text;

#[cfg(feature = "protocol")]
pub use protocol::*;
