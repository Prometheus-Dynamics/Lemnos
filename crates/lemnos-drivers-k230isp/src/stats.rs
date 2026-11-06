//! The statistics the ISP measured, read from its registers ([`read_raw`]): the AE block's 5x5
//! grid of luma means, the histogram's 16 bins, and the AWB block's means and pixel count. The
//! conversion into a 3A input is the camera stack's; the reads are here. The raw blocks measure
//! before white balance and lens shading (the header does not say; this is the assumption, to be
//! confirmed on a board). Written, untested on board.

use crate::RegisterAccess;
use crate::regs::{
    AE_MEAN_0, AE_MEAN_MASK, AWB_WHITE_CNT_MASK, HIST_BIN_0, HIST_BIN_MASK, ISP_AWB_MEAN,
    ISP_AWB_WHITE_CNT,
};

/// Bins of the luma histogram.
pub const HIST_BINS: usize = 16;

/// A statistics snapshot as the registers hold it.
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

/// Reads a statistics snapshot: the 25 AE means, the 16 histogram bins and the AWB means and
/// count.
pub fn read_raw<R: RegisterAccess>(regs: &mut R) -> Result<Raw, R::Error> {
    let mut raw = Raw::default();
    for (i, m) in raw.ae.iter_mut().enumerate() {
        *m = (regs.read(AE_MEAN_0 + 4 * i as u32)? & AE_MEAN_MASK) as u8;
    }
    for (i, b) in raw.hist.iter_mut().enumerate() {
        *b = regs.read(HIST_BIN_0 + 4 * i as u32)? & HIST_BIN_MASK;
    }
    let mean = regs.read(ISP_AWB_MEAN)?;
    // RGB measurement mode: red in bits 7..0, blue in 15..8, green in 23..16.
    raw.awb_mean = [mean as u8, (mean >> 16) as u8, (mean >> 8) as u8];
    raw.awb_white = regs.read(ISP_AWB_WHITE_CNT)? & AWB_WHITE_CNT_MASK;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::convert::Infallible;

    /// A register file where only the AWB registers hold values.
    struct Fake;

    impl RegisterAccess for Fake {
        type Error = Infallible;

        fn read(&mut self, addr: u32) -> Result<u32, Infallible> {
            Ok(match addr {
                ISP_AWB_MEAN => 0x00AA_BBCC,
                ISP_AWB_WHITE_CNT => 1234,
                _ => 0,
            })
        }

        fn write(&mut self, _: u32, _: u32) -> Result<(), Infallible> {
            Ok(())
        }
    }

    #[test]
    fn the_awb_mean_is_read_in_red_green_blue_order() {
        let raw = read_raw(&mut Fake).unwrap();
        assert_eq!(raw.awb_mean, [0xCC, 0xAA, 0xBB]);
        assert_eq!(raw.awb_white, 1234);
        assert_eq!(raw.ae, [0; 25]);
    }
}
