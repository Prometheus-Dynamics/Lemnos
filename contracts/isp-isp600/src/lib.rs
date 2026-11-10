//! The `isp.isp600/1` contract: what only the ISP600 has, beside its `camera.isp/1`.
//!
//! The ISP600 takes its whole configuration as one load image, the block's register image and
//! its tables, and saves its statistics as one buffer each frame. `camera.isp/1` numbers its
//! configuration layout [`LAYOUT_LOAD`] and its statistics layout [`LAYOUT_STATISTICS`]; their
//! bytes are the block's own, which the driver carries and does not read past the few words it
//! checks. Tuning and 3A stay with the caller.
//!
//! This contract tells a caller where the parts of each lie: [`IspIsp600::load_region`] and
//! [`IspIsp600::statistics_region`] answer an offset and a size for each region number below.
//! `layout_version` in [`Isp600Info`] names the arrangement; it changes when any offset or size
//! does. Offsets and sizes are bytes from the start of the buffer.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// `camera.isp/1` output number of the image the ISP and its scaler write.
pub const OUTPUT_0: u8 = 0;

/// `camera.isp/1` configuration layout: a load image.
pub const LAYOUT_LOAD: u32 = 1;
/// `camera.isp/1` statistics layout: one frame's statistics.
pub const LAYOUT_STATISTICS: u32 = 2;

/// Load image region: the register image.
pub const LOAD_REGISTERS: u8 = 0;
/// Load image region: the front end's table.
pub const LOAD_FRONT_END_TABLE: u8 = 1;
/// Load image region: the Bayer tables (lens shading, noise reduction shading, temporal
/// denoise, tone mapping).
pub const LOAD_BAYER_TABLE: u8 = 2;
/// Load image region: the RGB tables (dynamic range compression, gamma).
pub const LOAD_RGB_TABLE: u8 = 3;
/// Load image region: the YUV tables (colour enhancement).
pub const LOAD_YUV_TABLE: u8 = 4;
/// Load image regions.
pub const LOAD_REGIONS: u8 = 5;

/// Statistics region: the registers the block saves with its statistics.
pub const STATISTICS_REGISTERS: u8 = 0;
/// Statistics region: auto exposure.
pub const STATISTICS_AE: u8 = 1;
/// Statistics region: the 32 bytes after auto exposure (`RES` in the source).
pub const STATISTICS_RES: u8 = 2;
/// Statistics region: auto focus, IIR filter sums.
pub const STATISTICS_AF_IIR_SUM: u8 = 3;
/// Statistics region: auto focus, FIR filter sums.
pub const STATISTICS_AF_FIR_SUM: u8 = 4;
/// Statistics region: auto focus, IIR filter counts.
pub const STATISTICS_AF_IIR_COUNT: u8 = 5;
/// Statistics region: auto focus, FIR filter counts.
pub const STATISTICS_AF_FIR_COUNT: u8 = 6;
/// Statistics region: auto focus, highlight counts.
pub const STATISTICS_AF_HIGHLIGHT_COUNT: u8 = 7;
/// Statistics region: auto white balance.
pub const STATISTICS_AWB: u8 = 8;
/// Statistics region: flicker detection (`AFS` in the source).
pub const STATISTICS_AFS: u8 = 9;
/// Statistics region: the first histogram.
pub const STATISTICS_HISTOGRAM_0: u8 = 10;
/// Statistics region: the second histogram.
pub const STATISTICS_HISTOGRAM_1: u8 = 11;
/// Statistics region: tone mapping luminance (`PLTM_LUM` in the source).
pub const STATISTICS_TONE_LUMINANCE: u8 = 12;
/// Statistics region: tone mapping (`PLTM_PKX` in the source), from the save-load buffer.
pub const STATISTICS_TONE_PKX: u8 = 13;
/// Statistics region: temporal denoise (`D3D_K` in the source), from the save-load buffer.
pub const STATISTICS_DENOISE_K: u8 = 14;
/// Statistics regions.
pub const STATISTICS_REGIONS: u8 = 15;

/// The block.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Isp600Info {
    /// The arrangement of regions the offsets below describe.
    pub layout_version: u32,
    pub load_bytes: u32,
    pub statistics_bytes: u32,
    pub load_regions: u8,
    pub statistics_regions: u8,
    /// Frames finished after a job's load image is handed in that are still of the parameters
    /// before it; the job's statistics and image are the next ones.
    pub settle_frames: u8,
}

/// Where one region lies in its buffer.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    pub offset: u32,
    pub bytes: u32,
}

#[contract("isp.isp600", version = 1)]
pub trait IspIsp600 {
    #[method(1)]
    fn info(&mut self) -> Result<Isp600Info>;
    /// `NOT_FOUND` from [`LOAD_REGIONS`] on.
    #[method(2)]
    fn load_region(&mut self, region: u8) -> Result<Region>;
    /// `NOT_FOUND` from [`STATISTICS_REGIONS`] on.
    #[method(3)]
    fn statistics_region(&mut self, region: u8) -> Result<Region>;
}
