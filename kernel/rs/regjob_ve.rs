// SPDX-License-Identifier: GPL-2.0
//! The register-job profile of the A733's second video engine, its H.264 encoder and the input stage in
//! front of it (lemnos_regjob.h), Rust (stage 1i of the port).
//!
//! Facts, not code, from public sources:
//!
//! - The Radxa allwinner-bsp tree at a842bb8dc74608ba13975c97c31b9518f1976c01,
//!   drivers/ve/cedar-ve/platforms/ve_plat_sun60iw2.c and cedar_ve.c: the register blocks inside the 4 KiB
//!   window (encoder top at 0x800, input stage at 0xa00, encoder at 0xb00), the module select at 0x800 and
//!   its encoder bit (7), the encoder's interrupt enables (0xb14, bits 0..2) and status (0xb1c, bits 0..3)
//!   as its interrupt handler uses them, the top reset register at 0x804 (bits 0 and 4 set, then cleared),
//!   and the block's one reset line pulsed by IOCTL_RESET_VE.
//! - Linux, drivers/staging/media/sunxi/cedrus/cedrus_regs.h at 65429ba825723c8670d5acc672f68b325eb80dff:
//!   the encoder's register names and offsets in that layout (VE_ISP_* at 0xa00, VE_AVC_* at 0xb00), which
//!   of them take a bus address, and VE_AVC_VLE_END and VE_AVC_VLE_MAX beside the bitstream address. The
//!   interrupt registers the vendor kernel names sit at VE_AVC_CTRL and VE_AVC_STATUS of this layout.
//! - jemk/cedrus (h264enc/h264enc.c, 84c5e373d77980fbac22008a6d515318d148d297): what each address register
//!   is given in a working encoder and how large each buffer behind it is in macroblocks, the meaning of
//!   the size, stride and format fields, the put-bits and encode triggers, and the status bits.
//!
//! None of this has been seen on this engine: the sizes below are the ones the public encoder allocates,
//! not a register manual's. Untested on board (see testing/board-runs/H8a.md, which checks each with the
//! IOMMU's fault report).
//!
//! The registers a caller may set are only those the public encoder sets, with only the bits it sets.
//! Everything else in the window, the decoders' blocks and the top-level registers included, is refused.

use crate::bindings as lb;
use crate::regjob_profile::{self as rp, Ctx, Regs};
use kernel::error::{code::*, Result};

const VE_WINDOW: u32 = 0x1000;

const VE_ENC_TOP_MODE: u32 = 0x800; // module select: bit 7 the encoder, bit 6 the input stage
const VE_ENC_TOP_RESET: u32 = 0x804;

const VE_ISP_INPUT_SIZE: u32 = 0xa00; // macroblocks: width << 16 | height
const VE_ISP_INPUT_STRIDE: u32 = 0xa04; // macroblocks << 16
const VE_ISP_CTRL: u32 = 0xa08; // bits 31..29: the input format; bit 0 its interrupt
const VE_ISP_INPUT_LUMA: u32 = 0xa78;
const VE_ISP_INPUT_CHROMA: u32 = 0xa7c;

const VE_AVC_PARAM: u32 = 0xb04;
const VE_AVC_QP: u32 = 0xb08;
const VE_AVC_MOTION_EST: u32 = 0xb10;
const VE_AVC_CTRL: u32 = 0xb14; // bits 2..0: interrupt enables
const VE_AVC_TRIGGER: u32 = 0xb18;
const VE_AVC_STATUS: u32 = 0xb1c;
const VE_AVC_BASIC_BITS: u32 = 0xb20;
const VE_AVC_UNK_BUF: u32 = 0xb60;
const VE_AVC_VLE_ADDR: u32 = 0xb80;
const VE_AVC_VLE_END: u32 = 0xb84;
const VE_AVC_VLE_OFFSET: u32 = 0xb88; // bits
const VE_AVC_VLE_MAX: u32 = 0xb8c; // bits
const VE_AVC_REF_LUMA: u32 = 0xba0;
const VE_AVC_REF_CHROMA: u32 = 0xba4;
const VE_AVC_REC_LUMA: u32 = 0xbb0;
const VE_AVC_REC_CHROMA: u32 = 0xbb4;
const VE_AVC_REF_SLUMA: u32 = 0xbb8;
const VE_AVC_REC_SLUMA: u32 = 0xbbc;
const VE_AVC_MB_INFO: u32 = 0xbc0;

const VE_MODE_ENC: u32 = 1 << 7;
const VE_INT_ENABLES: u32 = 0x7;
const VE_INT_STATUS: u32 = 0xf;
const VE_TRIGGER_ENCODE: u32 = 0x8;
const VE_TRIGGER_PUT_BITS: u32 = 0x1f01; // bit 0, and the bit count in bits 12..8
const VE_FMT_SHIFT: u32 = 29;
const VE_FMT_NV12: u32 = 0;
const VE_FMT_NV16: u32 = 1;
const VE_MB_MASK: u32 = 0x3ff; // the macroblock fields the profile accepts: up to 16368 pixels

// Every address a job must give, in this order.
const S_INPUT_LUMA: usize = 0;
const S_INPUT_CHROMA: usize = 1;
const S_VLE: usize = 2;
const S_REF_LUMA: usize = 3;
const S_REF_CHROMA: usize = 4;
const S_REF_SLUMA: usize = 5;
const S_REC_LUMA: usize = 6;
const S_REC_CHROMA: usize = 7;
const S_REC_SLUMA: usize = 8;
const S_MB_INFO: usize = 9;
const S_UNK_BUF: usize = 10;
const S_COUNT: usize = 11;

/// 256 bytes: none of the public encoder's buffers is less aligned.
const VE_ALIGN: u32 = 256;

const fn slot(reg: u32) -> rp::Slot {
    rp::Slot { reg, align: VE_ALIGN }
}

static VE_SLOTS: [rp::Slot; S_COUNT] = [
    slot(VE_ISP_INPUT_LUMA),
    slot(VE_ISP_INPUT_CHROMA),
    slot(VE_AVC_VLE_ADDR),
    slot(VE_AVC_REF_LUMA),
    slot(VE_AVC_REF_CHROMA),
    slot(VE_AVC_REF_SLUMA),
    slot(VE_AVC_REC_LUMA),
    slot(VE_AVC_REC_CHROMA),
    slot(VE_AVC_REC_SLUMA),
    slot(VE_AVC_MB_INFO),
    slot(VE_AVC_UNK_BUF),
];

const fn plain(offset: u32, count: u32, mask: u32) -> rp::Plain {
    rp::Plain { offset, count, mask }
}

/// What a caller may write, and only these bits: what the public encoder writes.
static VE_PLAIN: [rp::Plain; 10] = [
    plain(VE_ENC_TOP_MODE, 1, VE_MODE_ENC),
    plain(VE_ISP_INPUT_SIZE, 1, VE_MB_MASK << 16 | VE_MB_MASK),
    plain(VE_ISP_INPUT_STRIDE, 1, VE_MB_MASK << 16),
    plain(VE_ISP_CTRL, 1, 0x7 << VE_FMT_SHIFT),
    plain(VE_AVC_PARAM, 1, 0x8000_0110), // no emulation prevention, CABAC, P slice
    plain(VE_AVC_QP, 1, 0x00ff_3f3f),
    plain(VE_AVC_MOTION_EST, 1, 0x0000_0104),
    plain(VE_AVC_TRIGGER, 1, VE_TRIGGER_PUT_BITS),
    plain(VE_AVC_BASIC_BITS, 1, 0xffff_ffff),
    plain(VE_AVC_VLE_OFFSET, 1, 0xffff_ffff),
];

fn start_ok(value: u32) -> bool {
    value == VE_TRIGGER_ENCODE
}

const fn align(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

/// The bytes the block reaches from each address, from the job's geometry. Every term is at most 16
/// bits wide, so nothing here can wrap a u64.
fn check(c: &Ctx<'_>, need: &mut [u64; rp::MAX_ADDRS]) -> Result {
    let mode = c.value(VE_ENC_TOP_MODE)?;
    let size = c.value(VE_ISP_INPUT_SIZE)?;
    let stride = c.value(VE_ISP_INPUT_STRIDE)?;
    let ctrl = c.value(VE_ISP_CTRL)?;
    let offset = c.value(VE_AVC_VLE_OFFSET)?;
    if mode & VE_MODE_ENC == 0 {
        return Err(EINVAL);
    }
    let mb_w = u64::from(size >> 16 & VE_MB_MASK);
    let mb_h = u64::from(size & VE_MB_MASK);
    let mb_stride = u64::from(stride >> 16 & VE_MB_MASK);
    if mb_w == 0 || mb_h == 0 || mb_stride < mb_w {
        return Err(EINVAL);
    }
    let fmt = ctrl >> VE_FMT_SHIFT;
    if fmt != VE_FMT_NV12 && fmt != VE_FMT_NV16 {
        return Err(EINVAL);
    }

    // The input frame: a luma plane and an interleaved chroma plane at the stride.
    need[S_INPUT_LUMA] = mb_stride * 16 * mb_h * 16;
    need[S_INPUT_CHROMA] =
        if fmt == VE_FMT_NV12 { mb_stride * 16 * mb_h * 8 } else { mb_stride * 16 * mb_h * 16 };
    // Reference and reconstruction pictures, in the encoder's own layout.
    let w32 = align(mb_w * 16, 32);
    let luma = w32 * align(mb_h * 16, 32);
    let chroma = w32 * align(mb_h * 8, 32);
    need[S_REF_LUMA] = luma;
    need[S_REC_LUMA] = luma;
    need[S_REF_CHROMA] = chroma;
    need[S_REC_CHROMA] = chroma;
    need[S_REF_SLUMA] = luma / 4;
    need[S_REC_SLUMA] = luma / 4;
    need[S_MB_INFO] = mb_w * 32;
    need[S_UNK_BUF] = align(mb_w, 4) * mb_h * 8;

    // The bitstream: its end and its size in bits are the module's, from the length.
    let vle_bits = c.bytes(S_VLE) * 8;
    if vle_bits > u64::from(u32::MAX) || u64::from(offset) >= vle_bits {
        return Err(EFAULT);
    }
    need[S_VLE] = 1;
    Ok(())
}

fn derive(r: &Regs, c: &Ctx<'_>) {
    r.wr(VE_AVC_VLE_END, (c.addr(S_VLE) + c.bytes(S_VLE) - 1) as u32);
    r.wr(VE_AVC_VLE_MAX, (c.bytes(S_VLE) * 8) as u32);
}

fn arm(r: &Regs) {
    r.wr(VE_AVC_STATUS, VE_INT_STATUS);
    r.wr(VE_AVC_CTRL, r.rd(VE_AVC_CTRL) | VE_INT_ENABLES);
}

fn irq(r: &Regs) -> Option<u32> {
    let ctrl = r.rd(VE_AVC_CTRL);
    let st = r.rd(VE_AVC_STATUS) & VE_INT_STATUS;
    if st == 0 || ctrl & VE_INT_ENABLES == 0 {
        return None;
    }
    // As the vendor handler does: the enables off, then the status cleared.
    r.wr(VE_AVC_CTRL, ctrl & !VE_INT_ENABLES);
    r.wr(VE_AVC_STATUS, st);
    Some(st)
}

fn quiet(r: &Regs) {
    r.wr(VE_AVC_CTRL, r.rd(VE_AVC_CTRL) & !VE_INT_ENABLES);
    r.wr(VE_ISP_CTRL, r.rd(VE_ISP_CTRL) & !1);
}

fn reset_regs(r: &Regs) {
    let v = r.rd(VE_ENC_TOP_RESET);
    r.wr(VE_ENC_TOP_RESET, v | 1 | 1 << 4);
    r.wr(VE_ENC_TOP_RESET, r.rd(VE_ENC_TOP_RESET) & !(1 | 1 << 4));
}

pub(crate) static PROFILE: rp::Profile = rp::Profile {
    id: lb::LEMNOS_REGJOB_PROFILE_A733_VE,
    name: c"the A733 video engine's encoder",
    window: VE_WINDOW,
    plain: &VE_PLAIN,
    keep_bits: false,
    config: &[],
    slots: &VE_SLOTS,
    start_reg: VE_AVC_TRIGGER,
    addr_limit: 1 << 32,
    continuous: false,
    irq_index: 0,
    start_ok,
    check,
    derive,
    arm,
    irq,
    latch: None,
    quiet,
    reset_regs,
    event_irq_index: 0,
    event: None,
    event_arm: None,
};
