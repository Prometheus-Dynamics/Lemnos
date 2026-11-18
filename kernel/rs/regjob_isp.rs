// SPDX-License-Identifier: GPL-2.0
//! The register-job profile of the K230's ISP (an ISP8000 with a version 2 memory interface), in two
//! roles (lemnos_regjob.h), Rust (stage 1i of the port; replaces lemnos_regjob_k230isp.c). The frames
//! role sets up the input, the windows and the main path and points the path at the caller's buffers (a
//! job is one frame's buffers; the path keeps running between jobs), and the configuration role writes
//! the tuning registers and takes the ISP core's interrupt, for statistics. The frame path and the
//! tuning are two drivers' work on one block; isp_ctrl is in both roles' tables, each with its own bits,
//! and is written by read-modify-write.
//!
//! Facts, not code, from public sources:
//!
//! - The vendor SDK (k230_linux_sdk at fc6cc21), buildroot-overlay/package/vvcam/isp/vvcam_isp_driver.c
//!   and vvcam_isp_hal.h: the compatible, the three interrupts in order (the ISP core, the memory
//!   interface, the front end), the one unnamed reset line, the memory interface's interrupt status and
//!   clear registers (0x16d0, 0x16d8) and the main path's frame-end bits, and the ISP core's interrupt
//!   registers (0x5bc, 0x5c4, 0x5c8) and its frame-out bit.
//! - VeriSilicon's register description of the same ISP, MIT-licensed, as published in nxp-imx/isp-vvcam
//!   (lf-6.18.20_2.0.0, e6453233e67f5d2f180d6ec277158dd54fa67a25): vvcam/isp/mrv_all_regs.h for the
//!   register offsets and fields (the memory interface's control at 0x1300, the main path's control,
//!   format, bus and picture registers from 0x1310, its Y, Cb and Cr base, size and offset registers
//!   from 0x1324, their second set from 0x1358 and their shadows from 0x1370, the raw output's base,
//!   size and offset from 0x1394 and its shadow at 0x13b4, the JPEG output's from 0x13c0, the interrupt
//!   mask, raw status and clear registers at 0x16c0, 0x16e0 and 0x16d8 with the two frame-end bits
//!   (YCbCr, raw) and the wrap bits; the ISP core's control, acquisition, output window, demosaic, image
//!   stabilisation and resizer registers, and the measurement blocks', with their fields);
//!   vvcam/isp/mrv_all_bits.h for what the base and offset enables do (a forced update copies base and
//!   size into their shadows together); and vvcam/isp/isp_miv2.c for how a working driver gives the main
//!   path a buffer (base, size, offset 0 per plane).
//!
//! The memory interface writes each plane within base .. base + size and wraps at the size (it has a
//! wrap interrupt per plane). So the module bounds the block by writing the size itself, from the
//! checked length, and the caller's frame geometry needs no check: a frame larger than its buffer wraps
//! inside it. The base and size reach the block only through their shadows, which the module makes it
//! take in one forced update: on an idle block before the start, and on a running one in the interrupt
//! handler at the end of a frame, so a frame never starts half on one buffer's addresses. Automatic
//! updates, the second address set (ping-pong) and every other path of the memory interface (the self
//! paths, JPEG, the multi-camera and read DMA) stay off: their enables are not a caller's to set. A job
//! starts the main path's YCbCr output or its raw output, one of them; every address register of the
//! main path (Y, Cb, Cr, their second set, raw, JPEG) is written from the job's checked buffers either
//! way, so none is left naming memory from before.
//!
//! The two tables are the ones the drivers in user space check against too
//! (drivers/k230-capture-sys/src/access.rs for the frames role, drivers/k230isp-sys/src/access.rs for
//! the configuration role), less the acquisition's latency FIFO bit, which selects the read port from
//! memory.
//!
//! None of this has been seen on this chip. Untested on board (see testing/board-runs/H8b.md).

use crate::bindings as lb;
use crate::regjob_profile::{self as rp, Ctx, Plain, Regs, Slot};
use kernel::error::Result;

const ISP_WINDOW: u32 = 0x9000;

const VI_DPCL: u32 = 0x0018;
const ISP_CTRL: u32 = 0x0400;
const ISP_ACQ_PROP: u32 = 0x0404;
const ISP_ACQ_H_OFFS: u32 = 0x0408; // then v_offs, h_size, v_size
const ISP_ACQ_NR_FRAMES: u32 = 0x0418;
const ISP_OUT_H_OFFS: u32 = 0x0594; // then v_offs, h_size, v_size
const ISP_DEMOSAIC: u32 = 0x05a4;
const ISP_IMSC: u32 = 0x05bc;
const ISP_MIS: u32 = 0x05c4;
const ISP_ICR: u32 = 0x05c8;
const MRSZ_CTRL: u32 = 0x0c00;
const MRSZ_FORMAT_CONV_CTRL: u32 = 0x0c6c;
const MI_CTRL: u32 = 0x1300; // path enables
const MI_MP_CTRL: u32 = 0x1310;
const MI_MP_FMT: u32 = 0x1314;
const MI_MP_BUS_CFG: u32 = 0x1318;
const MI_MP_BUS_ID: u32 = 0x131c;
const MI_MP_Y_BASE: u32 = 0x1324; // then size and offset counter
const MI_MP_Y_LLENGTH: u32 = 0x1330; // then pic width, height and size
const MI_MP_CB_BASE: u32 = 0x1340;
const MI_MP_CR_BASE: u32 = 0x134c;
const MI_MP_Y_BASE2: u32 = 0x1358; // then Cb and Cr: the ping-pong set
const MI_MP_Y_BASE_SHD: u32 = 0x1370; // then size and offset, then Cb's, then Cr's
const MI_MP_RAW_BASE: u32 = 0x1394; // then size and offset counter
const MI_MP_RAW_LLENGTH: u32 = 0x13a0; // then pic width, height and size
const MI_MP_RAW_BASE_SHD: u32 = 0x13b4;
const MI_MP_JDP_BASE: u32 = 0x13c0; // then size and offset counter
const MI_IMSC: u32 = 0x16c0;
const MI_MIS: u32 = 0x16d0;
const MI_ICR: u32 = 0x16d8;
const MI_RIS: u32 = 0x16e0;
const ISP_IS_CTRL: u32 = 0x2300;
const ISP_IS_H_OFFS: u32 = 0x2308; // then v_offs, h_size, v_size

// The configuration role's registers.
const ISP_AWB_PROP: u32 = 0x0510;
const ISP_AWB_GAIN_G: u32 = 0x0538; // then gain_rb
const ISP_CC_COEFF_0: u32 = 0x0570;
const ISP_GAMMA_OUT_MODE: u32 = 0x05f4;
const ISP_GAMMA_OUT_Y_0: u32 = 0x05f8;
const ISP_CT_OFFSET_R: u32 = 0x0648; // then g and b
const LSC_CTRL: u32 = 0x2200;
const LSC_R_TABLE_ADDR: u32 = 0x2204; // then gr, b and gb
const LSC_R_TABLE_DATA: u32 = 0x2214; // then gr, b and gb
const LSC_XGRAD_0: u32 = 0x2224; // then the x gradients 1 to 3 and y gradients 0 to 3
const LSC_XSIZE_0: u32 = 0x2244; // then the x sizes 1 to 3 and y sizes 0 to 3
const LSC_TABLE_SEL: u32 = 0x2264;
const HIST_PROP: u32 = 0x2400;
const HIST_WEIGHTS_0: u32 = 0x2454;
const AE_CTRL: u32 = 0x2600;
const BLS_CTRL: u32 = 0x2700;
const BLS_A_FIXED: u32 = 0x2728;
const AWB_MEAS_MODE: u32 = 0x2c00;

const fn bit(n: u32) -> u32 {
    1 << n
}

const CTRL_ENABLE: u32 = bit(0);
const LSC_EN: u32 = bit(0);
const LSC_ADDR_MASK: u32 = 0x0000_01ff; // mrv_all_regs.h: MRV_LSC_R_RAM_ADDR
const LSC_DATA_MASK: u32 = 0x00ff_ffff; // two 12-bit samples: 0 and 12
const LSC_GRAD_PAIR_MASK: u32 = 0x0fff_0fff; // two 12-bit gradients: 0 and 16
const LSC_SECT_PAIR_MASK: u32 = 0x03ff_03ff; // two 10-bit sector sizes: 0 and 16
const CTRL_MODE: u32 = bit(3) | bit(2) | bit(1);
const CTRL_INFORM_ENABLE: u32 = bit(4);
const CTRL_CFG_UPD: u32 = bit(9);
const CTRL_GEN_CFG_UPD: u32 = bit(10);
const CTRL_GAMMA_OUT_ENABLE: u32 = bit(11);
const ACQ_BAYER_PAT: u32 = bit(4) | bit(3);
const ACQ_INPUT_SELECTION: u32 = bit(14) | bit(13) | bit(12);
const WINDOW_MASK: u32 = 0x0000_3fff;
const ACQ_NR_FRAMES_MASK: u32 = 0x0000_03ff;
const DEMOSAIC_BYPASS: u32 = bit(10);
const MRSZ_SCALE_MASK: u32 = 0x0000_00ff;
const CONV_MASK: u32 = 0x0000_001f; // output and input formats
const MP_FMT_MASK: u32 = 0x0000_7fff;
const BUS_CFG_SWAP_RAW: u32 = 0x00f0_0000;
const BUS_ID_ENABLES: u32 = bit(25) | bit(8); // the ID's switch and write-ID enables
const VI_DPCL_MP_MASK: u32 = 0x0000_000f; // the main path's selector, the channel mode
const MIS_FRAME_OUT: u32 = bit(1);

const MI_MP_YCBCR_ENABLE: u32 = bit(0);
const MI_MP_RAW_ENABLE: u32 = bit(1);
const MP_CTRL_UPDATE: u32 = bit(5) | bit(4) | bit(3); // offset and base enables, forced update
const MI_MP_FRAME_END: u32 = bit(1) | bit(0); // raw, YCbCr
const MI_MP_WRAP: u32 = bit(10) | bit(9) | bit(8); // Cr, Cb, Y
const MI_SIZE_MASK: u32 = 0x1fff_fff0;
const MI_ALIGN: u32 = 16;

// The slots, in this order: Y, Cb (1), Cr, raw.
const P_Y: usize = 0;
const P_CR: usize = 2;
const P_RAW: usize = 3;
const P_COUNT: usize = 4;

/// Each slot's base, size and offset counter; the second address set only for Y, Cb and Cr.
const PLANE_BASE: [u32; P_COUNT] = [MI_MP_Y_BASE, MI_MP_CB_BASE, MI_MP_CR_BASE, MI_MP_RAW_BASE];

static ISP_SLOTS: [Slot; P_COUNT] = [
    Slot { reg: MI_MP_Y_BASE, align: MI_ALIGN },
    Slot { reg: MI_MP_CB_BASE, align: MI_ALIGN },
    Slot { reg: MI_MP_CR_BASE, align: MI_ALIGN },
    Slot { reg: MI_MP_RAW_BASE, align: MI_ALIGN },
];

const ALL: u32 = 0xffff_ffff;

const fn plain(offset: u32, count: u32, mask: u32) -> Plain {
    Plain { offset, count, mask }
}

/// What the frames role may write: the input, the windows, the main path's format and geometry. None of
/// these holds an address, a size of memory, a DMA start or an interrupt enable.
static ISP_FRAMES: [Plain; 16] = [
    plain(VI_DPCL, 1, VI_DPCL_MP_MASK),
    // isp_ctrl: the enable, the mode, the input formatter and the two update strobes.
    plain(ISP_CTRL, 1, CTRL_ENABLE | CTRL_MODE | CTRL_INFORM_ENABLE | CTRL_CFG_UPD | CTRL_GEN_CFG_UPD),
    // isp_acq_prop: the Bayer pattern and the input width; not the read port from memory.
    plain(ISP_ACQ_PROP, 1, ACQ_BAYER_PAT | ACQ_INPUT_SELECTION),
    plain(ISP_ACQ_H_OFFS, 4, WINDOW_MASK),
    plain(ISP_ACQ_NR_FRAMES, 1, ACQ_NR_FRAMES_MASK),
    plain(ISP_OUT_H_OFFS, 4, WINDOW_MASK),
    plain(ISP_DEMOSAIC, 1, DEMOSAIC_BYPASS),
    plain(ISP_IS_CTRL, 1, bit(0)),
    plain(ISP_IS_H_OFFS, 4, WINDOW_MASK),
    plain(MRSZ_CTRL, 1, MRSZ_SCALE_MASK),
    plain(MRSZ_FORMAT_CONV_CTRL, 1, CONV_MASK),
    plain(MI_MP_FMT, 1, MP_FMT_MASK),
    plain(MI_MP_BUS_CFG, 1, BUS_CFG_SWAP_RAW),
    plain(MI_MP_BUS_ID, 1, BUS_ID_ENABLES),
    // Line length and picture width, height and size: the YCbCr output's, then the raw's.
    plain(MI_MP_Y_LLENGTH, 4, ALL),
    plain(MI_MP_RAW_LLENGTH, 4, ALL),
];

/// What the configuration role may write: the tuning and the measurement windows. None of these holds
/// an address, a length or a DMA control.
static ISP_CONFIG: [Plain; 19] = [
    // isp_ctrl: the gamma-out enable and the two update strobes.
    plain(ISP_CTRL, 1, CTRL_GAMMA_OUT_ENABLE | CTRL_CFG_UPD | CTRL_GEN_CFG_UPD),
    plain(ISP_AWB_PROP, 1, ALL),
    plain(ISP_AWB_GAIN_G, 2, ALL),
    plain(ISP_CC_COEFF_0, 9, ALL),
    plain(ISP_GAMMA_OUT_MODE, 1, ALL),
    plain(ISP_GAMMA_OUT_Y_0, 17, ALL),
    plain(ISP_CT_OFFSET_R, 3, ALL),
    // Lens shading, each register with its field bits (mrv_all_regs.h): the enable; the four table
    // address pointers and the four sample words; the gradient and size pairs; the table select. The
    // tables are the ISP's own memory, two banks of 153 words.
    plain(LSC_CTRL, 1, LSC_EN),
    plain(LSC_R_TABLE_ADDR, 4, LSC_ADDR_MASK),
    plain(LSC_R_TABLE_DATA, 4, LSC_DATA_MASK),
    plain(LSC_XGRAD_0, 8, LSC_GRAD_PAIR_MASK),
    plain(LSC_XSIZE_0, 8, LSC_SECT_PAIR_MASK),
    plain(LSC_TABLE_SEL, 1, bit(0)),
    plain(HIST_PROP, 5, ALL),
    plain(HIST_WEIGHTS_0, 7, ALL),
    plain(AE_CTRL, 5, ALL),
    plain(BLS_CTRL, 1, ALL),
    plain(BLS_A_FIXED, 4, ALL),
    plain(AWB_MEAS_MODE, 5, ALL),
];

/// One output of the main path at a time: YCbCr or raw.
fn start_ok(value: u32) -> bool {
    value == MI_MP_YCBCR_ENABLE || value == MI_MP_RAW_ENABLE
}

/// The size the module writes for a plane: the length, to the register's grain and range.
fn size(bytes: u64) -> u32 {
    (bytes.min(u64::from(MI_SIZE_MASK)) as u32) & MI_SIZE_MASK
}

fn check(_c: &Ctx<'_>, need: &mut [u64]) -> Result {
    // The block wraps inside the size the module writes: one grain is all it needs.
    for n in need[..P_COUNT].iter_mut() {
        *n = u64::from(MI_ALIGN);
    }
    Ok(())
}

fn derive(r: &Regs, c: &Ctx<'_>) {
    for (k, &base) in PLANE_BASE.iter().enumerate() {
        r.wr(base + 4, size(c.bytes(k)));
        r.wr(base + 8, 0);
    }
    // The ping-pong set is never enabled; it holds the same addresses all the same.
    for k in P_Y..=P_CR {
        r.wr(MI_MP_Y_BASE2 + 4 * k as u32, c.addr(k) as u32);
    }
    // The JPEG output is never enabled; it is pointed at the raw output's memory.
    r.wr(MI_MP_JDP_BASE, c.addr(P_RAW) as u32);
    r.wr(MI_MP_JDP_BASE + 4, size(c.bytes(P_RAW)));
    r.wr(MI_MP_JDP_BASE + 8, 0);
}

fn latch(r: &Regs, c: &Ctx<'_>) -> u32 {
    r.wr(MI_MP_CTRL, MP_CTRL_UPDATE);
    for k in P_Y..=P_CR {
        let shd = MI_MP_Y_BASE_SHD + 12 * k as u32;
        if r.rd(shd) != c.addr(k) as u32 || r.rd(shd + 4) != size(c.bytes(k)) {
            return lb::LEMNOS_REGJOB_STATUS_UNLATCHED;
        }
    }
    if r.rd(MI_MP_RAW_BASE_SHD) != c.addr(P_RAW) as u32 {
        return lb::LEMNOS_REGJOB_STATUS_UNLATCHED;
    }
    0
}

fn arm(r: &Regs) {
    r.wr(MI_ICR, MI_MP_FRAME_END | MI_MP_WRAP);
    r.wr(MI_IMSC, MI_MP_FRAME_END);
}

fn irq(r: &Regs) -> Option<u32> {
    let mis = r.rd(MI_MIS);
    if mis & MI_MP_FRAME_END == 0 {
        return None;
    }
    // The frame's end, and whether any plane wrapped in it (a frame larger than its buffer).
    let status = (mis & MI_MP_FRAME_END) | (r.rd(MI_RIS) & MI_MP_WRAP);
    r.wr(MI_ICR, MI_MP_FRAME_END | MI_MP_WRAP);
    Some(status)
}

fn quiet(r: &Regs) {
    r.wr(MI_IMSC, 0);
    r.wr(MI_ICR, ALL);
    r.wr(MI_CTRL, 0);
    r.wr(ISP_IMSC, 0);
    r.wr(ISP_ICR, ALL);
}

fn reset_regs(r: &Regs) {
    // No automatic update: the shadows change only when the module says so.
    r.wr(MI_MP_CTRL, 0);
}

fn event(r: &Regs) -> u32 {
    let mis = r.rd(ISP_MIS);
    if mis != 0 {
        r.wr(ISP_ICR, mis);
    }
    mis
}

fn event_arm(r: &Regs, on: bool) {
    r.wr(ISP_ICR, ALL);
    r.wr(ISP_IMSC, if on { MIS_FRAME_OUT } else { 0 });
}

pub(crate) const PROFILE: rp::Profile = rp::Profile {
    id: lb::LEMNOS_REGJOB_PROFILE_K230_ISP,
    name: c"the K230 ISP's main path and configuration",
    window: ISP_WINDOW,
    plain: &ISP_FRAMES,
    keep_bits: true,
    config: &ISP_CONFIG,
    slots: &ISP_SLOTS,
    start_reg: MI_CTRL,
    addr_limit: 1 << 32,
    continuous: true,
    irq_index: 1,
    start_ok,
    check,
    derive,
    arm,
    irq,
    latch: Some(latch),
    quiet,
    reset_regs,
    event_irq_index: 0,
    event: Some(event),
    event_arm: Some(event_arm),
};

// The C engine's table (lemnos_regjob_priv.h's lrj_k230_isp).
rp::c_profile!(lrj_k230_isp, PROFILE);
