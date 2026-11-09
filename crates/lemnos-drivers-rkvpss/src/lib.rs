//! Rockchip `rkvpss` offline scaler: dma-buf to dma-buf crop, scale and format conversion on up
//! to six output channels, through the vendor kernel's `rkvpss-offline` video node.
//!
//! Written from the driver's source for the RV1126B's block, untested on board.

#![cfg(target_os = "linux")]

mod device;

pub use device::{Rkvpss, find};
pub use lemnos_linux_sys::rkvpss::{Channel, Crop, Job, Output, PixelFormat, Surface};
