//! The Kendryte K230 non-AI 2D engine: pixel-format conversion between dma-bufs. See the README
//! for what is covered. Written from the K230 Linux SDK driver's source, untested on board.

pub mod layout;

#[cfg(target_os = "linux")]
mod device;

#[cfg(target_os = "linux")]
pub use device::{DRIVER, Nonai2d, find};
pub use layout::{Layout, LayoutError, MAX_HEIGHT, MAX_WIDTH, PixelFormat, Plane};
