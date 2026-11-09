//! The `lens/1` contract: a lens moved by an actuator. Positions are the actuator's own codes.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// The positions the actuator can reach.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub min: u16,
    pub max: u16,
}

#[contract("lens", version = 1)]
pub trait Lens {
    #[method(1)]
    fn range(&mut self) -> Result<Range>;
    /// Starts a move and returns at once.
    #[method(2)]
    fn set_position(&mut self, position: u16) -> Result<()>;
    /// The last position asked for.
    #[method(3)]
    fn position(&mut self) -> Result<u16>;
}
