//! The `light/1` contract: a strip or ring of lights. Whoever calls it sends a whole look in each
//! call. Lemnos keeps no catalogue of named looks.
//!
//! A look is shown on a layer. Among the layers with something on them, the highest priority is
//! what's shown, and clearing it reveals the next. The bytes of a look are defined by the driver
//! that draws it.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// How many pixels there are, and how many layers a look can be shown on.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layout {
    pub pixels: u16,
    pub layers: u8,
}

#[contract("light", version = 1)]
pub trait Light {
    #[method(1)]
    fn layout(&mut self) -> Result<Layout>;
    /// Shows `look` on `layer` at `priority`, replacing what the layer had.
    #[method(2)]
    fn show(&mut self, layer: u8, priority: u8, look: &[u8]) -> Result<()>;
    /// Empties `layer`.
    #[method(3)]
    fn clear(&mut self, layer: u8) -> Result<()>;
    /// Empties every layer.
    #[method(4)]
    fn clear_all(&mut self) -> Result<()>;
    /// Scales everything shown, 0 to 255.
    #[method(5)]
    fn set_brightness(&mut self, level: u8) -> Result<()>;
}
