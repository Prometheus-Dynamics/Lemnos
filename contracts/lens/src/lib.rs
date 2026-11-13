//! The `lens/1` contract: a lens moved by an actuator. Positions are the actuator's own codes.
//!
//! Two facts about the actuator sit beside the moves, both about the hardware rather than the
//! caller. [`Lens::direction`] says which way focus moves as the position goes up, and
//! [`Lens::settle_us`] says how long after a move the lens is still. Each answer can be
//! [`FocusDirection::Unknown`] or `None`: a driver answers so when it has no figure, and never
//! guesses one.

#![no_std]

use lemnos_core::{Decode, Encode, Reader, Result, Shape, Status, Writer, contract};

/// The positions the actuator can reach.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub min: u16,
    pub max: u16,
}

/// Which way focus moves as the position goes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum FocusDirection {
    /// Neither the driver nor the board says. Never a guess.
    #[default]
    Unknown = 0,
    /// Increasing positions focus on something nearer.
    TowardNear = 1,
    /// Increasing positions focus on something farther away.
    TowardFar = 2,
}

impl Encode for FocusDirection {
    const SHAPE: Shape = Shape::U8;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        (*self as u8).encode(out)
    }
}

impl Decode<'_> for FocusDirection {
    fn decode(from: &mut Reader<'_>) -> Result<Self> {
        match u8::decode(from)? {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::TowardNear),
            2 => Ok(Self::TowardFar),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }
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
    /// Which way focus moves as the position goes up. `Unknown` until the board or the driver says.
    #[method(4)]
    fn direction(&mut self) -> Result<FocusDirection>;
    /// Microseconds after a move of `distance` codes before the lens is still. `None` when the
    /// driver and the board have no figure for it.
    #[method(5)]
    fn settle_us(&mut self, distance: u16) -> Result<Option<u32>>;
}
