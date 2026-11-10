//! The statistics layout: the statistics registers as the block left them at the end of a
//! frame, copied out unread.
//!
//! A statistics buffer in [`LAYOUT_STATISTICS`] holds [`STATISTICS_REGISTERS`] little-endian
//! `u32` words, each register's value exactly as read, in the order of
//! [`access::STATISTICS`](lemnos_driver_k230isp_sys::access::STATISTICS):
//!
//! ```text
//! words  0..25   isp_exp_mean_00 .. _44   AE block means, row-major (y * 5 + x), 8 bits each
//! words 25..41   histogram bins 0 .. 15   16 integer and 4 fractional bits each
//! word  41       isp_awb_white_cnt         pixels the AWB block averaged, 26 bits
//! word  42       isp_awb_mean              RGB mode: green 23..16, blue 15..8, red 7..0
//! ```
//!
//! The driver never reads these values. [`Raw::decode`] is for a caller that wants them as
//! numbers: it applies the field masks and the AWB channel order of the register map. The raw
//! blocks measure before white balance and lens shading (the headers do not say; this is the
//! assumption, to be confirmed on a board).

use lemnos_driver_k230isp_sys::access::{STATISTICS, STATISTICS_REGISTERS as REGISTERS};
use lemnos_driver_k230isp_sys::regs::{AE_MEAN_MASK, AWB_WHITE_CNT_MASK, HIST_BIN_MASK};

/// `camera.isp/1` statistics layout: the statistics registers, as read.
pub const LAYOUT_STATISTICS: u32 = 0x0002;
/// Registers in a statistics buffer.
pub const STATISTICS_REGISTERS: usize = REGISTERS;
/// Bytes of a statistics buffer.
pub const STATISTICS_BYTES: usize = 4 * STATISTICS_REGISTERS;
/// Bins of the luma histogram.
pub const HIST_BINS: usize = 16;

/// The statistics registers' offsets, in the layout's order.
pub fn registers() -> impl Iterator<Item = u32> {
    STATISTICS
        .iter()
        .flat_map(|span| (0..span.count).map(move |i| span.first + 4 * i))
}

/// A statistics buffer as numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    /// The AE block means, row-major (`y * 5 + x`), 8 bits each.
    pub ae: [u8; 25],
    /// The histogram bins as the registers hold them: 16 integer and 4 fractional bits.
    pub hist: [u32; HIST_BINS],
    /// The AWB means of red, green and blue (RGB measurement mode), 8 bits each.
    pub awb_mean: [u8; 3],
    /// Pixels the AWB block averaged.
    pub awb_white: u32,
}

impl Raw {
    /// Decodes a statistics buffer; `None` when it is shorter than [`STATISTICS_BYTES`].
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..STATISTICS_BYTES)?;
        let mut words = [0u32; STATISTICS_REGISTERS];
        for (w, b) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *w = u32::from_le_bytes(*b);
        }
        let mut raw = Self::default();
        for (m, w) in raw.ae.iter_mut().zip(&words[..25]) {
            *m = (w & AE_MEAN_MASK) as u8;
        }
        for (b, w) in raw.hist.iter_mut().zip(&words[25..41]) {
            *b = w & HIST_BIN_MASK;
        }
        raw.awb_white = words[41] & AWB_WHITE_CNT_MASK;
        let mean = words[42];
        // RGB measurement mode: red in bits 7..0, blue in 15..8, green in 23..16.
        raw.awb_mean = [mean as u8, (mean >> 16) as u8, (mean >> 8) as u8];
        Some(raw)
    }
}
