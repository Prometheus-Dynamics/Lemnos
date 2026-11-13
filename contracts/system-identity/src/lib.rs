//! The `system.identity/1` contract: the board's identity facts (`docs/identity.md`).
//!
//! Each method writes one fact as UTF-8 into the caller's buffer and returns its length. A fact
//! the node cannot know answers `NOT_FOUND`: it is absent, never guessed. A buffer too small for
//! its text answers `BUFFER_TOO_SMALL`. [`SystemIdentity::facts`] writes every fact the driver
//! knows, one `name=value` per line, in the order `docs/identity.md` lists them.

#![no_std]

use lemnos_core::{Result, contract};

/// The most bytes one fact's value may take, and the buffer size its method needs.
pub const MAX_TEXT: usize = 256;
/// The buffer `facts` needs: every fact of the node, one line each.
pub const MAX_FACTS_TEXT: usize = 2048;

#[contract("system.identity", version = 1, kind = "system")]
pub trait SystemIdentity {
    /// The board's serial, in the normalised format (lowercase hex, 8 to 32 digits).
    #[method(1, reading = text)]
    fn serial(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The board's family: its `[board] id`.
    #[method(2, reading = text)]
    fn family(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The board's display name, with its revision after it.
    #[method(3, reading = text)]
    fn model(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Every fact the driver knows, as `name=value` lines. `into` holds [`MAX_FACTS_TEXT`] bytes.
    #[method(4)]
    fn facts(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}
