//! The `pixels/1` contract: a frame of colours for a strip or ring of LEDs. Each LED is four bytes,
//! red, green, blue and white, in logical order. The driver that draws the frame decides what each
//! byte becomes on its wire.
//!
//! A frame is built with `set` calls and sent with one `show`. A local caller has only
//! [`LOCAL_ARGS`] bytes of arguments, so a frame larger than [`MAX_LOCAL_LEDS`] is split across
//! `set` calls of at most that many LEDs each, and then `show` sends the whole stored frame.

#![no_std]

use lemnos_core::{LOCAL_ARGS, Result, contract};

/// How many LEDs fit in one local `set` call. Its arguments are a 2-byte offset, a 4-byte length
/// and 4 bytes per LED, within [`LOCAL_ARGS`]: `(64 - 2 - 4) / 4` = 14.
pub const MAX_LOCAL_LEDS: usize = (LOCAL_ARGS - 2 - 4) / 4;

#[contract("pixels", version = 1, kind = "light")]
pub trait Pixels {
    /// How many LEDs the strip has.
    #[method(1, reading = number)]
    fn pixel_count(&mut self) -> Result<u16>;
    /// Stores LEDs starting at LED `offset` (an LED index, not a byte offset), 4 bytes each (r, g,
    /// b, w), first LED first. LEDs past the end of the strip are ignored, and a partial LED at the
    /// end is dropped. Other stored LEDs keep their values. Nothing reaches the strip until `show`.
    #[method(2, reading = colour_array)]
    fn set(&mut self, offset: u16, pixels: &[u8]) -> Result<()>;
    /// Sends the stored frame to the strip as one transaction.
    #[method(3)]
    fn show(&mut self) -> Result<()>;
}
