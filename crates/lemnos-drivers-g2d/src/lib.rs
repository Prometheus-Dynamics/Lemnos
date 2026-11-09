//! The Allwinner G2D 2D engine through the BSP kernel's `/dev/g2d`. See the README for what is
//! covered. Written from the BSP driver's source, untested on board.

#[cfg(target_os = "linux")]
mod device;

#[cfg(target_os = "linux")]
pub use device::G2d;
#[cfg(target_os = "linux")]
pub use lemnos_linux_sys::g2d::{
    Blend, ColorRange, Gamut, HardwareVersion, Job, PixelFormat, Rect, Surface, Turn,
};
