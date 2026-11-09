//! Back end configuration builder.
//!
//! The back end reads a raw (Bayer or mono) or RGB/YUV image from memory and runs:
//! decompress → DPC → GEQ → TDN → SDN → BLC → stitch → LSC → WBG → CDN → CAC → debin →
//! tonemap → demosaic → CCM → saturation → YCbCr → sharpen/false colour → YCbCr inverse →
//! gamma, then per output branch: crop → CSC → downscale → resample → output format. It works
//! in tiles of at most 640 pixels wide; the tiles are part of the config buffer.
//!
//! The finalisation and "smart resize" rules follow libpisp
//! `src/libpisp/backend/backend.cpp` and `backend_prepare.cpp` (BSD-2-Clause, Copyright (C)
//! 2021 - 2023, Raspberry Pi Ltd), ported to Rust; unlike libpisp, everything is finalised
//! on every [`BackEnd::prepare`] (no dirty tracking) and the tiling is recomputed each time.
//!
//! Tuning stays with the caller: this crate has no gamma curve, resampling filter, sharpening,
//! demosaic or colour encoding of its own, and [`BackEnd::prepare`] refuses a block that is
//! enabled with an empty table rather than run it.

pub mod curve;
pub mod lsc;
pub mod ops;
mod prepare;
mod slices;
pub mod tiling;

use crate::math;
use crate::uapi::*;

/// Offset of the compressed temporal-denoise average (libcamera's PiSP pipeline uses 2048 for
/// its mode-1 TDN buffers). Part of the buffer format, shared by the compressor and the
/// decompressor; not an image-quality setting.
const TDN_COMPRESS_OFFSET: u16 = 2048;

pub use prepare::PrepareError;
pub use slices::{SplitJobs, split_tiles, split_tiles_into};

/// Maximum tile width of the BCM2712 back end (libpisp `BackEndMaxTileWidth`).
pub const MAX_TILE_WIDTH: u16 = 640;
/// libpisp's stripe height limit.
pub const MAX_STRIPE_HEIGHT: u16 = 3072;

/// The identity colour matrix: the hardware no-op, not a tuning value.
pub const IDENTITY_CCM: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

/// Signed 4.10 fixed point (1.0 = 1024), clamped to the 14-bit field.
pub fn coeff_s4_10(v: f64) -> i16 {
    math::round(v * 1024.0).clamp(-8192.0, 8191.0) as i16
}

/// Builds `pisp_be_tiles_config` buffers.
#[derive(Clone, Debug)]
pub struct BackEnd {
    cfg: BeConfig,
    crop: [BeCropConfig; BE_NUM_OUTPUTS],
    downscale_extra: [BeDownscaleExtra; BE_NUM_OUTPUTS],
    resample_extra: [BeResampleExtra; BE_NUM_OUTPUTS],
    smart_resize: [(u16, u16); BE_NUM_OUTPUTS],
    /// Fixed resampler steps (4.12) in place of the corner-aligned ones.
    resample_step: [Option<(u16, u16)>; BE_NUM_OUTPUTS],
    lsc_extra: BeLscExtra,
    max_tile_width: u16,
    max_stripe_height: u16,
}

impl Default for BackEnd {
    fn default() -> Self {
        Self::new()
    }
}

impl BackEnd {
    /// A back end with every block zeroed and nothing enabled: the caller supplies the tables
    /// of the blocks it enables.
    pub fn new() -> Self {
        Self {
            cfg: BeConfig::default(),
            crop: Default::default(),
            downscale_extra: Default::default(),
            resample_extra: Default::default(),
            smart_resize: Default::default(),
            resample_step: Default::default(),
            lsc_extra: BeLscExtra::default(),
            max_tile_width: MAX_TILE_WIDTH,
            max_stripe_height: MAX_STRIPE_HEIGHT,
        }
    }

    /// The configuration as it stands.
    pub fn config(&self) -> &BeConfig {
        &self.cfg
    }

    /// Mutable access to every block, for settings without a dedicated setter.
    pub fn config_mut(&mut self) -> &mut BeConfig {
        &mut self.cfg
    }

    /// Enables and Bayer order.
    pub fn set_global(&mut self, bayer_enables: u32, rgb_enables: u32, bayer: BayerOrder) {
        self.cfg.global = BeGlobalConfig {
            bayer_enables,
            rgb_enables,
            bayer_order: bayer as u8,
            pad: [0; 3],
        };
    }

    /// Input image format.
    pub fn set_input_format(&mut self, f: ImageFormatConfig) {
        self.cfg.input_format = f;
    }

    /// Black level correction (16-bit scale, one level for all channels).
    pub fn set_black_level(&mut self, level: u16) {
        self.set_black_levels([level; 4]);
    }

    /// Black level correction with one level per CFA channel, in sensor order: red, green on
    /// the red row, green on the blue row, blue (16-bit scale). The spatial and temporal
    /// denoise, which take one level, use their mean.
    pub fn set_black_levels(&mut self, levels: [u16; 4]) {
        self.cfg.blc = BlaConfig {
            black_level_r: levels[0],
            black_level_gr: levels[1],
            black_level_gb: levels[2],
            black_level_b: levels[3],
            output_black_level: 0,
            pad: [0; 2],
        };
        let mean = self.denoise_black_level();
        self.cfg.sdn.black_level = mean;
        self.cfg.tdn.black_level = mean;
    }

    /// The one black level the denoise blocks use: the mean of the four channels, rounded.
    fn denoise_black_level(&self) -> u16 {
        let b = &self.cfg.blc;
        let sum = u32::from(b.black_level_r)
            + u32::from(b.black_level_gr)
            + u32::from(b.black_level_gb)
            + u32::from(b.black_level_b);
        ((sum + 2) / 4) as u16
    }

    /// White balance (and digital) gains.
    pub fn set_wb_gains(&mut self, r: f64, g: f64, b: f64) {
        self.cfg.wbg = WbgConfig {
            gain_r: crate::fe::gain_4_10(r),
            gain_g: crate::fe::gain_4_10(g),
            gain_b: crate::fe::gain_4_10(b),
            pad: [0; 2],
        };
    }

    /// Colour correction matrix (row-major, applied to linear RGB).
    pub fn set_ccm(&mut self, m: [f64; 9]) {
        self.cfg.ccm = BeCcmConfig {
            coeffs: m.map(coeff_s4_10),
            ..Default::default()
        };
    }

    /// Lens shading: the gain table (see [`lsc::pack_lut`]) and where the image starts in it;
    /// enables the block. Grid steps left at zero spread the table over the input image.
    pub fn set_lsc(&mut self, lsc: BeLscConfig, extra: BeLscExtra) {
        self.cfg.lsc = lsc;
        self.lsc_extra = extra;
        self.cfg.global.bayer_enables |= bayer_enable::LSC;
    }

    /// Defective pixel correction (`None`: off).
    pub fn set_dpc(&mut self, dpc: Option<BeDpcConfig>) {
        self.cfg.dpc = dpc.unwrap_or_default();
        self.enable_bayer(bayer_enable::DPC, dpc.is_some());
    }

    /// Green equalisation (`None`: off).
    pub fn set_geq(&mut self, geq: Option<BeGeqConfig>) {
        self.cfg.geq = geq.unwrap_or_default();
        self.enable_bayer(bayer_enable::GEQ, geq.is_some());
    }

    /// Spatial denoise (`None`: off); its black level follows [`Self::set_black_level`].
    pub fn set_sdn(&mut self, sdn: Option<BeSdnConfig>) {
        self.cfg.sdn = sdn.unwrap_or_default();
        self.cfg.sdn.black_level = self.denoise_black_level();
        self.enable_bayer(bayer_enable::SDN, sdn.is_some());
    }

    /// Colour denoise (`None`: off).
    pub fn set_cdn(&mut self, cdn: Option<BeCdnConfig>) {
        self.cfg.cdn = cdn.unwrap_or_default();
        self.enable_bayer(bayer_enable::CDN, cdn.is_some());
    }

    /// The format of the temporal denoise buffers (the input's: 16-bit Bayer, same size and
    /// stride), needed before [`Self::set_tdn`].
    pub fn set_tdn_format(&mut self, f: ImageFormatConfig) {
        self.cfg.tdn_input_format = f;
        self.cfg.tdn_output_format = f;
        // Compressed (8-bit mode 1, as libcamera keeps its average): half the memory traffic.
        let comp = image_format::is_compressed(f.format);
        let (offset, mode) = if comp {
            (TDN_COMPRESS_OFFSET, 1)
        } else {
            (0, 0)
        };
        self.cfg.tdn_compress = CompressConfig {
            offset,
            pad: 0,
            mode,
        };
        self.cfg.tdn_decompress = DecompressConfig {
            offset,
            pad: 0,
            mode,
        };
    }

    /// Temporal denoise (`None`: off). `input`: read the long-term average written by the
    /// previous job (false after a reset, when there is none). Its black level follows
    /// [`Self::set_black_level`].
    pub fn set_tdn(&mut self, tdn: Option<BeTdnConfig>, input: bool) {
        self.cfg.tdn = tdn.unwrap_or_default();
        self.cfg.tdn.black_level = self.denoise_black_level();
        let on = tdn.is_some();
        let comp = image_format::is_compressed(self.cfg.tdn_output_format.format);
        self.enable_bayer(bayer_enable::TDN | bayer_enable::TDN_OUTPUT, on);
        self.enable_bayer(bayer_enable::TDN_INPUT, on && input);
        self.enable_bayer(bayer_enable::TDN_COMPRESS, on && comp);
        self.enable_bayer(bayer_enable::TDN_DECOMPRESS, on && input && comp);
    }

    fn enable_bayer(&mut self, bits: u32, on: bool) {
        if on {
            self.cfg.global.bayer_enables |= bits;
        } else {
            self.cfg.global.bayer_enables &= !bits;
        }
    }

    /// Gamma from a curve of `(x, y)` points on 16-bit scales.
    pub fn set_gamma_curve(&mut self, points: &[(u32, u32)]) {
        self.cfg.gamma = curve::gamma_from_curve(points);
    }

    /// The YCbCr matrices used inside the pipeline (for sharpening/false colour): `forward` is
    /// RGB to YCbCr, `inverse` the way back. The caller chooses the encoding.
    pub fn set_ycbcr(&mut self, forward: BeCcmConfig, inverse: BeCcmConfig) {
        self.cfg.ycbcr = forward;
        self.cfg.ycbcr_inverse = inverse;
    }

    /// Output `i` format; width, height and strides left at zero are computed.
    pub fn set_output_format(&mut self, i: Branch, f: BeOutputFormatConfig) {
        let i = i.index();
        self.cfg.output_format[i] = f;
    }

    /// Output `i` crop of the input (zero width = no crop).
    pub fn set_crop(&mut self, i: Branch, c: BeCropConfig) {
        let i = i.index();
        self.crop[i] = c;
    }

    /// Output `i` colour space conversion.
    pub fn set_csc(&mut self, i: Branch, c: BeCcmConfig) {
        let i = i.index();
        self.cfg.csc[i] = c;
    }

    /// Output `i` size, choosing downscaler and resampler automatically ("smart resize").
    pub fn set_smart_resize(&mut self, i: Branch, width: u16, height: u16) {
        let i = i.index();
        self.smart_resize[i] = (width, height);
    }

    /// Output `i` downscaler target size.
    pub fn set_downscale(&mut self, i: Branch, e: BeDownscaleExtra) {
        let i = i.index();
        self.downscale_extra[i] = e;
    }

    /// Output `i` resampler target size and phases.
    pub fn set_resample(&mut self, i: Branch, e: BeResampleExtra) {
        let i = i.index();
        self.resample_extra[i] = e;
    }

    /// Output `i` resampler filter: 16 phases of 6 taps (s.10). Required for a resampler the
    /// caller or smart resize turns on, unless smart resize computes its own trapezoid.
    pub fn set_resample_filter(&mut self, i: Branch, coef: &[i16; BE_RESAMPLE_FILTER_SIZE]) {
        self.cfg.resample[i.index()].coef = *coef;
    }

    /// Fixes output `i`'s resampler step (4.12, horizontal and vertical) instead of the
    /// corner-aligned `(in - 1) / (out - 1)`: `Some((2 << 12, 2 << 12))` puts output pixel
    /// `x` at input `2x` exactly, every pixel on phase 0. `None` goes back to corner-aligned.
    pub fn set_resample_step(&mut self, i: Branch, step: Option<(u16, u16)>) {
        let i = i.index();
        self.resample_step[i] = step;
    }

    /// Limits the tile width (for testing tiling; at most [`MAX_TILE_WIDTH`]).
    pub fn set_max_tile_width(&mut self, w: u16) {
        self.max_tile_width = w.clamp(BE_MIN_TILE_WIDTH as u16, MAX_TILE_WIDTH);
    }

    /// Splits tiles into horizontal stripes of at most `h` output rows (libpisp's
    /// `max_stripe_height`; default [`MAX_STRIPE_HEIGHT`], one stripe for any frame the caller
    /// makes). With stripes a frame's tiles divide into the rows above a line and the rest,
    /// which can run as separate jobs into the same output buffers.
    pub fn set_max_stripe_height(&mut self, h: u16) {
        self.max_stripe_height = h.clamp(BE_MIN_TILE_HEIGHT as u16, MAX_STRIPE_HEIGHT);
    }

    /// A fixed Bayer-to-output pipeline: black level, white balance gains, demosaic (colour
    /// orders only), CCM ([`IDENTITY_CCM`] when `None`), and output 0 in `format` (a
    /// [`crate::format::formats`] value) at the input size. `csc`, when given, is the output 0
    /// colour space conversion a YUV `format` needs; the matrix is the caller's. Gamma,
    /// sharpening, denoise and the rest are off until the caller sets and enables them.
    pub fn simple_bayer(
        input: ImageFormatConfig,
        bayer: BayerOrder,
        black_level: u16,
        wb: (f64, f64, f64),
        ccm: Option<[f64; 9]>,
        format: u32,
        csc: Option<BeCcmConfig>,
    ) -> Self {
        let mut be = Self::new();
        be.set_input_format(input);
        be.set_black_level(black_level);
        be.set_wb_gains(wb.0, wb.1, wb.2);
        be.set_ccm(ccm.unwrap_or(IDENTITY_CCM));
        let mut bayer_en = bayer_enable::INPUT | bayer_enable::BLC | bayer_enable::WBG;
        let mut rgb_en = rgb_enable::CCM | rgb_enable::OUTPUT0;
        if bayer != BayerOrder::Greyscale {
            bayer_en |= bayer_enable::DEMOSAIC;
        }
        if let Some(csc) = csc {
            be.set_csc(Branch::Zero, csc);
            rgb_en |= rgb_enable::CSC0;
        }
        be.set_global(bayer_en, rgb_en, bayer);
        be.set_output_format(
            Branch::Zero,
            BeOutputFormatConfig {
                image: ImageFormatConfig {
                    format,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        be
    }
}

#[cfg(test)]
mod tests;
