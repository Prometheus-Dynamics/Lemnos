//! A board's identity facts (`docs/identity.md`): the values the daemon's `system.identity/1`
//! serves and the bridge publishes as `hello.<fact>`. A library: it ships no program and reads no
//! files of a board. The driver that serves the facts reads them and hands them in as plain values.
//!
//! [`SerialSource`] says where a serial is read from; [`Facts`] holds each fact after its format is
//! checked. A fact that cannot be known is never set, so it is absent.

pub mod facts;
pub mod serial;

pub use facts::{Facts, MAX_FACTS, MAX_VALUE_BYTES, NAMES};
pub use serial::{ByteOrder, SerialError, SerialSource, normalise};
