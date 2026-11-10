//! Which frame-path registers a caller may write and read: the whole of what reaches the vendor
//! driver's register requests from [`Capture::write`](crate::Capture::write) and
//! [`Capture::read`](crate::Capture::read).
//!
//! The vendor driver writes `base + addr` for any `addr` (no range check), and the window holds
//! registers that take bus addresses, start DMA or run a command list from memory. So a write is
//! allowed only to a register in [`WRITABLE`], and only to the bits in its mask; the other bits
//! keep their value (read, merged, written back). Nothing in the table holds an address, a size
//! the memory interface wraps at, an offset counter, or a path enable: those are written by
//! [`Capture`](crate::Capture) alone, from memory it holds (see [`crate::regs::ADDRESS_REGISTERS`],
//! [`crate::regs::MIV2_CTRL`] and [`crate::regs::MP_CTRL`]).
//!
//! The line lengths and picture sizes are in the table. They set how the main path walks a plane,
//! not where the plane is: every byte the path writes is at the plane's base plus an offset that
//! wraps at the plane's size (`*_size_init`, "the size of the buffer; the offset counter wraps
//! around"), and [`Capture`](crate::Capture) sets base and size from memory it holds. That wrap is
//! the one fact about the block the bound rests on; the board run checks it.

use crate::regs::{
    ACQ_BAYER_PAT_MASK, ACQ_INPUT_SELECTION_MASK, ACQ_LATENCY_FIFO_MASK, ACQ_NR_FRAMES_MASK,
    ADDRESS_REGISTERS, BUS_CFG_SWAP_RAW_MASK, BUS_ID_SW_EN, BUS_ID_WR_ID_EN, CONV_INPUT_MASK,
    CONV_OUTPUT_MASK, DEMOSAIC_BYPASS, ISP_ACQ_H_OFFS, ISP_ACQ_NR_FRAMES, ISP_ACQ_PROP, ISP_CTRL,
    ISP_CTRL_CFG_UPD, ISP_CTRL_ENABLE, ISP_CTRL_GEN_CFG_UPD, ISP_CTRL_INFORM_ENABLE,
    ISP_CTRL_MODE_MASK, ISP_DEMOSAIC, ISP_IS_CTRL, ISP_IS_H_OFFS, ISP_MIS, ISP_OUT_H_OFFS,
    MIS_MP_FRAME_END, MIV2_CTRL, MIV2_IMSC, MIV2_MIS, MP_BUS_CFG, MP_BUS_ID, MP_CTRL, MP_FMT,
    MP_FMT_MASK, MP_RAW_BASE_AD_SHD, MP_RAW_LLENGTH, MP_Y_BASE_AD_SHD, MP_Y_LLENGTH, MRSZ_CTRL,
    MRSZ_FORMAT_CONV_CTRL, MRSZ_SCALE_MASK, VI_DPCL, VI_DPCL_CHAN_MODE_MASK, VI_DPCL_MP_MUX_MASK,
    WINDOW_BYTES, WINDOW_MASK,
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

/// The registers a caller may write.
pub const WRITABLE: [Span; 17] = [
    // vi_dpcl: the main path's selector and the channel mode. Its other bits (the self paths'
    // selectors, the DMA read switch) keep their value.
    Span::new(VI_DPCL, 1, VI_DPCL_MP_MUX_MASK | VI_DPCL_CHAN_MODE_MASK),
    // isp_ctrl: enable, mode, input formatter, and the two update strobes. Its other bits (the
    // gamma-out and digital gain enables, the k230isp driver's) keep their value.
    Span::new(
        ISP_CTRL,
        1,
        ISP_CTRL_ENABLE
            | ISP_CTRL_MODE_MASK
            | ISP_CTRL_INFORM_ENABLE
            | ISP_CTRL_CFG_UPD
            | ISP_CTRL_GEN_CFG_UPD,
    ),
    Span::new(
        ISP_ACQ_PROP,
        1,
        ACQ_BAYER_PAT_MASK | ACQ_INPUT_SELECTION_MASK | ACQ_LATENCY_FIFO_MASK,
    ),
    // isp_acq_h_offs, _v_offs, _h_size, _v_size.
    Span::new(ISP_ACQ_H_OFFS, 4, WINDOW_MASK),
    Span::new(ISP_ACQ_NR_FRAMES, 1, ACQ_NR_FRAMES_MASK),
    // isp_out_h_offs, _v_offs, _h_size, _v_size.
    Span::new(ISP_OUT_H_OFFS, 4, WINDOW_MASK),
    Span::new(ISP_DEMOSAIC, 1, DEMOSAIC_BYPASS),
    // isp_is_ctrl: bit 0, the enable.
    Span::new(ISP_IS_CTRL, 1, 1),
    // isp_is_h_offs, _v_offs, _h_size, _v_size.
    Span::new(ISP_IS_H_OFFS, 4, WINDOW_MASK),
    Span::new(MRSZ_CTRL, 1, MRSZ_SCALE_MASK),
    Span::new(MRSZ_FORMAT_CONV_CTRL, 1, CONV_OUTPUT_MASK | CONV_INPUT_MASK),
    Span::new(MP_FMT, 1, MP_FMT_MASK),
    // miv2_mp_bus_cfg: the raw swap only. The burst length keeps its value.
    Span::new(MP_BUS_CFG, 1, BUS_CFG_SWAP_RAW_MASK),
    Span::new(MP_BUS_ID, 1, BUS_ID_WR_ID_EN | BUS_ID_SW_EN),
    // miv2_mp_y_llength, _y_pic_width, _y_pic_height, _y_pic_size.
    Span::new(MP_Y_LLENGTH, 4, ALL),
    // miv2_mp_raw_llength, _raw_pic_width, _raw_pic_height, _raw_pic_size.
    Span::new(MP_RAW_LLENGTH, 4, ALL),
    // miv2_imsc: the main path's two frame ends. The other paths' bits keep their value.
    Span::new(MIV2_IMSC, 1, MIS_MP_FRAME_END),
];

/// Registers that may be read beyond the writable ones: the controls [`Capture`](crate::Capture)
/// writes, the interrupt status, the address registers and the shadows saying which memory the
/// path writes now. Reading takes nothing from the caller's memory and starts nothing.
pub const READ_ONLY: [u32; 6] = [
    MIV2_CTRL,
    MP_CTRL,
    ISP_MIS,
    MIV2_MIS,
    MP_Y_BASE_AD_SHD,
    MP_RAW_BASE_AD_SHD,
];

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

/// Whether `addr` may be read.
#[must_use]
pub const fn readable(addr: u32) -> bool {
    if writable(addr).is_some() {
        return true;
    }
    let mut i = 0;
    while i < READ_ONLY.len() {
        if READ_ONLY[i] == addr {
            return true;
        }
        i += 1;
    }
    let mut i = 0;
    while i < ADDRESS_REGISTERS.len() {
        if ADDRESS_REGISTERS[i] == addr {
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
};
