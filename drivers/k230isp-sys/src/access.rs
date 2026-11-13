//! Which registers a caller may write, and which may be read: the whole of what reaches the
//! vendor driver's register requests.
//!
//! The vendor driver writes `base + addr` for any `addr` (no range check, see [`crate::regs`]),
//! and the window holds the memory interface and the front end, whose registers take bus
//! addresses, start DMA or run a command list from memory. A write there would let a caller make
//! the block reach memory it does not hold. So writes are allowed only to the registers listed in
//! [`WRITABLE`]: the measurement windows, gains, colour matrix, gamma curve, lens shading tables
//! and black levels of the ISP core and its measurement blocks, none of which holds an address,
//! a length or a DMA control. Everything else in the window, and everything past it, is refused.
//! Reads are allowed only of the writable registers and of the [`STATISTICS`] registers.
//!
//! The driver programs no register that takes a buffer address, because it delivers no frames
//! (no buffer of the caller's ever reaches the block): every such register is left out.

use crate::regs::{
    AE_BASE, AE_MEAN_0, AWB_BASE, BLS_A_FIXED, BLS_BASE, CTRL_CFG_UPD, CTRL_GAMMA_OUT_ENABLE,
    CTRL_GEN_CFG_UPD, HIST_BASE, HIST_BIN_0, HIST_WEIGHTS_0, ISP_AWB_GAIN_G, ISP_AWB_PROP,
    ISP_AWB_WHITE_CNT, ISP_CC_COEFF_0, ISP_CT_OFFSET_R, ISP_CTRL, ISP_GAMMA_OUT_MODE,
    ISP_GAMMA_OUT_Y_0, LSC_ADDR_MASK, LSC_CTRL, LSC_DATA_MASK, LSC_EN, LSC_GRAD_PAIR_MASK,
    LSC_R_TABLE_ADDR, LSC_R_TABLE_DATA, LSC_SECT_PAIR_MASK, LSC_TABLE_SEL, LSC_XGRAD_0,
    LSC_XSIZE_0, WINDOW_BYTES,
};

/// A run of `count` consecutive registers from `first`, each taking the bits in `mask`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The first register's offset in the window.
    pub first: u32,
    /// Registers in the run.
    pub count: u32,
    /// The bits a write may set or clear. A register whose mask is not `u32::MAX` is written by
    /// read-modify-write: its other bits keep their value.
    pub mask: u32,
}

impl Span {
    const fn new(first: u32, count: u32, mask: u32) -> Self {
        Self { first, count, mask }
    }

    /// Whether `addr` is one of this run's registers.
    #[must_use]
    pub const fn holds(&self, addr: u32) -> bool {
        addr.is_multiple_of(4) && addr >= self.first && (addr - self.first) / 4 < self.count
    }

    /// One past the run's last byte, as a `u64` so it cannot overflow.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.first as u64 + 4 * self.count as u64
    }
}

const ALL: u32 = u32::MAX;

/// The registers a caller may write: offsets and masks from [`crate::regs`].
pub const WRITABLE: [Span; 19] = [
    // isp_ctrl: only the gamma-out enable and the two shadow-update strobes. Its other bits
    // (the ISP's enable, the input selection) are kept as they are.
    Span::new(
        ISP_CTRL,
        1,
        CTRL_GAMMA_OUT_ENABLE | CTRL_CFG_UPD | CTRL_GEN_CFG_UPD,
    ),
    Span::new(ISP_AWB_PROP, 1, ALL),
    // isp_awb_gain_g, isp_awb_gain_rb.
    Span::new(ISP_AWB_GAIN_G, 2, ALL),
    // isp_cc_coeff_0 to _8.
    Span::new(ISP_CC_COEFF_0, 9, ALL),
    Span::new(ISP_GAMMA_OUT_MODE, 1, ALL),
    // gamma_out_y_block_arr: 17 points.
    Span::new(ISP_GAMMA_OUT_Y_0, 17, ALL),
    // isp_ct_offset_r, _g, _b.
    Span::new(ISP_CT_OFFSET_R, 3, ALL),
    // Lens shading (`LSC_BASE` to `LSC_TABLE_SEL`), each register with its field bits only: the
    // enable; the four table address pointers (9 bits); the four sample words (two 12-bit samples
    // each); the X and Y gradient pairs (8 words) and sector size pairs (8 words); the table select.
    Span::new(LSC_CTRL, 1, LSC_EN),
    Span::new(LSC_R_TABLE_ADDR, 4, LSC_ADDR_MASK),
    Span::new(LSC_R_TABLE_DATA, 4, LSC_DATA_MASK),
    Span::new(LSC_XGRAD_0, 8, LSC_GRAD_PAIR_MASK),
    Span::new(LSC_XSIZE_0, 8, LSC_SECT_PAIR_MASK),
    Span::new(LSC_TABLE_SEL, 1, 1),
    // Histogram: properties and the first sub-window (`HIST_PROP` to `HIST_V_SIZE`).
    Span::new(HIST_BASE, 5, ALL),
    // Histogram weights (`hist_weight_00to30` to `hist_weight_44`).
    Span::new(HIST_WEIGHTS_0, 7, ALL),
    // Exposure measurement: control and the block window (`AE_CTRL` to `AE_V_SIZE`).
    Span::new(AE_BASE, 5, ALL),
    // Black level: control.
    Span::new(BLS_BASE, 1, ALL),
    // Black level: the four fixed levels.
    Span::new(BLS_A_FIXED, 4, ALL),
    // AWB measurement: mode and window (`AWB_MEAS_MODE` to `AWB_V_SIZE`).
    Span::new(AWB_BASE, 5, ALL),
];

/// The statistics registers, in the order the statistics layout holds them: the 25 AE block
/// means (row-major), the 16 histogram bins, then the AWB white count and the AWB means.
pub const STATISTICS: [Span; 3] = [
    Span::new(AE_MEAN_0, 25, ALL),
    Span::new(HIST_BIN_0, 16, ALL),
    // isp_awb_white_cnt, isp_awb_mean.
    Span::new(ISP_AWB_WHITE_CNT, 2, ALL),
];

/// Registers in [`STATISTICS`].
pub const STATISTICS_REGISTERS: usize = 25 + 16 + 2;

/// The bits a caller may write at `addr`; `None` when the register may not be written.
#[must_use]
pub const fn writable(addr: u32) -> Option<u32> {
    let mut i = 0;
    while i < WRITABLE.len() {
        if WRITABLE[i].holds(addr) {
            return Some(WRITABLE[i].mask);
        }
        i += 1;
    }
    None
}

/// Whether `addr` may be read: a writable register or a statistics register.
#[must_use]
pub const fn readable(addr: u32) -> bool {
    if writable(addr).is_some() {
        return true;
    }
    let mut i = 0;
    while i < STATISTICS.len() {
        if STATISTICS[i].holds(addr) {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether `value` may be written to `addr`: the register is writable and `value` sets no bit
/// outside its mask.
#[must_use]
pub const fn allowed(addr: u32, value: u32) -> bool {
    match writable(addr) {
        Some(mask) => value & !mask == 0,
        None => false,
    }
}

const _: () = {
    let mut i = 0;
    while i < WRITABLE.len() {
        assert!(WRITABLE[i].end() <= WINDOW_BYTES as u64);
        i += 1;
    }
    let mut i = 0;
    let mut n = 0;
    while i < STATISTICS.len() {
        assert!(STATISTICS[i].end() <= WINDOW_BYTES as u64);
        n += STATISTICS[i].count as usize;
        i += 1;
    }
    assert!(n == STATISTICS_REGISTERS);
};
