//! The `light/1` contract: a strip or ring of lights. Whoever calls it sends a whole look, or a
//! frame of single LEDs, with each call. Lemnos keeps no catalogue of named looks.
//!
//! Each caller that shows something is an owner, and what it shows sits on a layer: a slot the
//! caller picks, `0` to `layers - 1`. A slot holds one look or one frame. Every LED shows the
//! value of the highest-priority owner that set it: a look sets every LED, a frame sets the LEDs
//! it names. On equal priorities the higher layer wins. Where no owner set an LED, the board's
//! default look shows there. Nothing is blended: an owner's value is what the LED shows.
//!
//! The bytes of a look are defined by the driver that draws it. A frame's bytes are one entry per
//! LED: its index as a `u16`, then red, green, blue and white.
//!
#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// How many pixels there are, and how many layers (owner slots) a caller can show on.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layout {
    pub pixels: u16,
    pub layers: u8,
}

/// Bytes per LED in a frame's entries: a `u16` index, then red, green, blue and white.
pub const FRAME_ENTRY: usize = 6;

#[contract("light", version = 1, kind = "light")]
pub trait Light {
    #[method(1)]
    fn layout(&mut self) -> Result<Layout>;
    /// Shows `look` on `layer` at `priority`, replacing what the layer had. The layer is then the
    /// caller's, and the look covers every LED.
    #[method(2)]
    fn show(&mut self, layer: u8, priority: u8, look: &[u8]) -> Result<()>;
    /// Empties `layer`, but only when the caller owns it: another caller's look or frame is left
    /// as it is.
    #[method(3)]
    fn clear(&mut self, layer: u8) -> Result<()>;
    /// Empties every layer the caller owns. Another caller's layers are left as they are.
    #[method(4)]
    fn clear_all(&mut self) -> Result<()>;
    /// Scales everything shown, 0 to 255.
    #[method(5, reading = range)]
    fn set_brightness(&mut self, level: u8) -> Result<()>;
    /// Shows a frame on `layer` at `priority`, replacing what the layer had. `leds` is a run of
    /// [`FRAME_ENTRY`]-byte entries (see the module's docs); the LEDs it names show its colours,
    /// and the rest of the ring is left to other owners. An entry for an LED past the ring is
    /// ignored. Marked `#[added]`.
    #[method(6)]
    #[added]
    fn show_frame(&mut self, layer: u8, priority: u8, leds: &[u8]) -> Result<()>;
}
