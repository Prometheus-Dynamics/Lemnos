//! The rkvpss offline scaler: dma-buf to dma-buf crop, scale and format conversion on up to six
//! output channels, through the vendor kernel's `rkvpss-offline` video node. No vendor library:
//! the caller says what each output shows, and the driver carries that to the block without
//! reading a pixel.
//!
//! - [`Job`]: one input [`Surface`] and up to six [`Output`]s, checked against the block's rules
//!   and against the size of every dma-buf it names, so it cannot reach memory the caller does not
//!   hold (see `lemnos-driver-rkvpss-sys`).
//! - [`Scaler`]: opens the node, hands its channels to offline mode, and runs jobs. A device
//!   failure closes the node and backs off (250 ms doubling to an 8 s cap, cleared by a job that
//!   runs); it never gives up and never turns itself off.
//! - [`find`]: the node's path, by its name in sysfs.
//!
//! The device side lives in `lemnos-driver-rkvpss-sys`, which holds all the `unsafe`; this crate
//! has none. A job run on an open node allocates nothing.
//!
//! Left out: tiled and compressed (FBC) formats, privacy masks, background fill, the encoder ring,
//! mirroring, quarter turns, the average scaler, and the driver's two-pass path for images wider
//! than 4096.
//!
//! Hardware status: untested on board. Written from the vendor driver's source; the check is
//! `testing/board-runs/E7.md`.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-rkvpss`).

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod device;
mod scaler;

#[cfg(test)]
mod tests;

pub use device::{Device, find};
pub use lemnos_driver_rkvpss_sys::{
    CHANNELS, Channel, Crop, Job, MAX_HEIGHT, MAX_RATIO, MAX_SCALED_HEIGHT, MAX_SCALED_WIDTH,
    MAX_STRIDE, MAX_WIDTH, MIN_SIZE, Node, OUTPUT_STRIDE_ALIGN, Output, PixelFormat, Surface,
};
pub use scaler::{RunError, Scaler, State, backoff_ns, open_at};
