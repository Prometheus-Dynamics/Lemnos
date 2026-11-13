//! A board's identity facts: the values the stack names as `hello.<fact>` (`docs/identity.md`).
//!
//! The board file says where the board's serial comes from ([`SerialSource`]); this library reads
//! it from there, normalises it, and gathers the facts Lemnos owns: `family`, `serial` and `model`
//! from the board file, and `version.slot`, `version.product` and `version.os` from the slot files
//! on the data partition. `version.lemnos` and `offers` belong to the daemon, so the caller adds
//! them with [`Facts::set`].
//!
//! Nothing here knows what a board is. A fact that cannot be known is absent, never guessed, and
//! [`Gathered::notes`] says why.

pub mod board;
pub mod facts;
pub mod json;
pub mod serial;

pub use board::{BoardFile, BoardSection, parse_board};
pub use facts::{Facts, Gathered, MAX_FACTS, MAX_VALUE_BYTES, gather};
pub use serial::{ByteOrder, SerialError, SerialSource, normalise};
