// SPDX-License-Identifier: GPL-2.0-only
//! rp1-csi2: one DMA channel of the camera front-end block's CSI-2 receiver, feeding a frame stream
//! (stage 1m of the port; replaces lemnos_rp1_csi2.c). The front end proper is rs/pisp_fe.rs's.
//!
//! Bound only through driver_override and only for compatibles in the owned= parameter (see
//! rs/lists.rs); it never replaces or patches a stock driver.
//!
//! The D-PHY bring-up, the CSI-2 DMA channel's registers and the frame start and end accounting are
//! ported from the stock driver, drivers/media/platform/raspberrypi/rp1_cfe/{dphy,csi2,cfe}.c:
//!   Copyright (C) 2021-2024 Raspberry Pi Ltd.
//! by way of the receiver on the dev branch (kernel-modules/lemnos/lemnos_rp1_csi2.c). Its buffer
//! queue is gone: this file has no address of its own. The rest:
//!   Copyright (c) 2026 the Lemnos authors.
//!
//! Everything the block can name or be told comes from somewhere the kernel holds:
//!  - Only device addresses from lem_stream_next() reach the DMA registers: memory this module pinned
//!    and mapped (the stream's pool, or the buffer's own dma-buf), the end checked against the buffer.
//!    No ioctl carries an address. There is no register passthrough.
//!  - The register offsets here are constants, inside windows checked at probe.
//!  - Interrupt status is only tested for known bits. No count or length from the block reaches a
//!    frame or a publish: a frame's size is the one fixed at START, already checked against the
//!    buffer. The channel's line count only decides whether a frame is published or counted as gone
//!    wrong.
//!  - A buffer is the block's from lem_stream_next() until the channel is known not to be using it.
//!    Stopping runs FORCE and gives the buffers back once the stop is confirmed: by the frame end
//!    that follows FORCE, or by the channel being between frames when FORCE landed with no frame
//!    start within the stop's bound. Unconfirmed, the buffers stay pinned and the receiver is wedged;
//!    it keeps looking for the late frame end on a backoff capped at 8 s, and releases them when it
//!    comes (`lrc_unwedge`). Closing the stream, STOP, closing this node and unbinding all come
//!    through here. docs/streams.md, "Stopping a receiver", has the why.
//!
//! The receiver's state is `LrcDev`, below: the front end on the same block (rs/pisp_fe.rs) reads a
//! few fields of it.
//!
//! Locking: `ctl` orders START, STOP, close and unbind. `lock` is the frame path's, taken from the
//! handler, and nests the stream's lock. The stream's ctl nests `ctl` through the stop callback, so
//! `ctl` is never held across a call that takes the stream's ctl. `going` keeps stream creation out
//! once unbinding has started, without taking `ctl`, because unbinding waits for the stream code
//! while holding its list lock.

use crate::bindings as lb;
use crate::pisp_fe::{
    FE_WINDOW, LemPispFe, lem_pisp_fe_create, lem_pisp_fe_csi_fs, lem_pisp_fe_destroy,
    lem_pisp_fe_dma, lem_pisp_fe_ioctl, lem_pisp_fe_isr, lem_pisp_fe_stop, lem_pisp_fe_streaming,
};
use crate::stream::{
    Handle, Producer, Slot, Stream, lem_stream_attach, lem_stream_complete_at, lem_stream_create,
    lem_stream_detach, lem_stream_from_file, lem_stream_next, lem_stream_unquarantine,
    lem_streams_block_gone, msecs_to_jiffies, usleep_range,
};
use core::{
    cell::UnsafeCell,
    ffi::{c_int, c_long, c_uint, c_void},
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};
use kernel::{
    alloc::{
        KBox,
        flags::{__GFP_ZERO, GFP_KERNEL},
    },
    bindings,
    device::Device,
    error::{Error, Result, code::*, from_err_ptr, to_result},
    ffi::{c_char, c_ulong},
    ioctl::{_IO, _IOR, _IOW, _IOWR},
    prelude::*,
    static_lock_class,
    transmute::{AsBytes, FromBytes},
    uaccess::{UserPtr, UserSlice},
};

const LRC_NAME: &core::ffi::CStr = c"lemnos-rp1-csi2";
const LRC_CH: usize = 0;
/// The embedded data channel: the same block's second DMA channel (stock: CSI2_CH1_EMBEDDED).
const LRC_EMB_CH: usize = 1;

// DW CSI-2 host (resource 1), from dphy.c.
const HOST_VERSION: usize = 0x000;
const HOST_N_LANES: usize = 0x004;
const HOST_RESETN: usize = 0x008;
const HOST_PHY_SHUTDOWNZ: usize = 0x040;
const HOST_PHY_RSTZ: usize = 0x044;
const HOST_PHY_TST_CTRL0: usize = 0x050;
const HOST_PHY_TST_CTRL1: usize = 0x054;
const DPHY_HS_RX_CTRL_LANE0: u8 = 0x44;
const DPHY_CLK_PN_SWAP: u8 = 0x35;
const DPHY_D0_PN_SWAP: u8 = 0x45;
const DPHY_D1_PN_SWAP: u8 = 0x55;
const DPHY_D2_PN_SWAP: u8 = 0x85;
const DPHY_D3_PN_SWAP: u8 = 0x95;

// CSI-2 DMA (resource 0), from csi2.c.
const CSI2_STATUS: usize = 0x000;
const CSI2_IRQ_MASK: usize = 0x020;
const CSI2_CTRL: usize = 0x024;
const fn csi2_ch_ctrl(x: usize) -> usize {
    x * 0x40 + 0x28
}
const fn csi2_ch_addr0(x: usize) -> usize {
    x * 0x40 + 0x2c
}
const fn csi2_ch_addr1(x: usize) -> usize {
    x * 0x40 + 0x3c
}
const fn csi2_ch_stride(x: usize) -> usize {
    x * 0x40 + 0x30
}
const fn csi2_ch_length(x: usize) -> usize {
    x * 0x40 + 0x34
}
const fn csi2_ch_debug(x: usize) -> usize {
    x * 0x40 + 0x38
}
const fn csi2_ch_frame_size(x: usize) -> usize {
    x * 0x40 + 0x40
}
const fn irq_fs(x: usize) -> u32 {
    (1 << 0) << x
}
const fn irq_fe(x: usize) -> u32 {
    (1 << 4) << x
}
const fn irq_fe_ack(x: usize) -> u32 {
    (1 << 8) << x
}
const fn irq_le(x: usize) -> u32 {
    (1 << 12) << x
}
const fn irq_le_ack(x: usize) -> u32 {
    (1 << 16) << x
}
const fn irq_ch_mask(x: usize) -> u32 {
    irq_fs(x) | irq_fe(x) | irq_fe_ack(x) | irq_le(x) | irq_le_ack(x)
}
const EOP_IS_EOL: u32 = 1 << 0;
const DMA_EN: u32 = 1 << 0;
const FORCE: u32 = 1 << 3;
const AUTO_ARM: u32 = 1 << 4;
const CSI2_MODE_FE_STREAMING: u32 = 3;
const IRQ_EN_FS: u32 = 1 << 13;
const IRQ_EN_FE_ACK: u32 = 1 << 15;
// stop_armed: which channel's FE_ACK after its FORCE is the stop's acknowledgement.
const STOP_ARM_IMAGE: u8 = 1 << 0;
const STOP_ARM_EMB: u8 = 1 << 1;
const PACK_LINE: u32 = 1 << 29;
const PACK_BYTES: u32 = 1 << 30;
const CH_MODE_SHIFT: u32 = 1;
const VC_SHIFT: u32 = 5;
const DT_SHIFT: u32 = 7;

// MIPI config (resource 2), from cfe.c.
const MIPICFG_CFG: usize = 0x004;
const MIPICFG_INTE: usize = 0x02c;
const MIPICFG_INTS: usize = 0x034;
const MIPICFG_CFG_SEL_CSI: u32 = 1 << 0;
const MIPICFG_INT_CSI_DMA: u32 = 1 << 0;
const MIPICFG_INT_PISP_FE: u32 = 1 << 4;

// The windows must cover what's used here, so a short region can't be mistaken for one.
const LRC_DMA_WINDOW: u64 = (csi2_ch_frame_size(LRC_EMB_CH) + 4) as u64;
const LRC_HOST_WINDOW: u64 = (HOST_PHY_TST_CTRL1 + 4) as u64;
const LRC_CFG_WINDOW: u64 = (MIPICFG_INTS + 4) as u64;

// Limits the hardware has; dphy.c and csi2.c are the source.
const LRC_MIN_MBPS: u32 = 80;
const LRC_MAX_MBPS: u32 = 1500;
const LRC_MAX_VC: u32 = 3;
const LRC_MAX_DT: u32 = 63;
const LRC_MAX_DIMENSION: u32 = 0xffff;

/// How often a channel with nothing armed looks for a free buffer again.
const LRC_RETRY_MS: u32 = 1;

/// The stop's bound is never below this (docs/streams.md), in us.
const LRC_STOP_MIN_US: u64 = 250_000;

/// While wedged, the first look for the late frame end comes this long after the stop gave up; each
/// look after doubles it, up to LRC_UNWEDGE_MAX_MS, for as long as it takes (the failure rule).
const LRC_UNWEDGE_FIRST_MS: u32 = 10;
const LRC_UNWEDGE_MAX_MS: u32 = 8000;

/// `wedged`: whose stop wasn't confirmed. Either refuses START on the block.
pub(crate) const WEDGED_CSI2: u8 = 1 << 0;
pub(crate) const WEDGED_FE: u8 = 1 << 1;

const DMA_IDLE: u32 = lb::LEMNOS_RP1_CSI2_DMA_IDLE;
const DMA_STREAMING: u32 = lb::LEMNOS_RP1_CSI2_DMA_STREAMING;
const DMA_WEDGED: u32 = lb::LEMNOS_RP1_CSI2_DMA_WEDGED;
pub(crate) const STOP_ACKED: u32 = lb::LEMNOS_RP1_CSI2_STOP_ACKED;
const STOP_IDLE: u32 = lb::LEMNOS_RP1_CSI2_STOP_IDLE;
pub(crate) const STOP_UNCONFIRMED: u32 = lb::LEMNOS_RP1_CSI2_STOP_UNCONFIRMED;
pub(crate) const STOP_LATE: u32 = lb::LEMNOS_RP1_CSI2_STOP_LATE;

// fwnode_graph_get_endpoint_by_id's flags (linux/property.h): the next endpoint up when the one named
// is absent, and an endpoint whose far end is not an enabled device.
const FWNODE_GRAPH_ENDPOINT_NEXT: c_ulong = 1 << 0;
const FWNODE_GRAPH_DEVICE_DISABLED: c_ulong = 1 << 1;

const PART_CSI2: u32 = lb::LEMNOS_RP1_CSI2_PART_CSI2;
const PART_FE: u32 = lb::LEMNOS_RP1_CSI2_PART_FE;
/// The parts a block can have: the index of `owner` is the part's bit.
const PARTS: usize = 2;

const IOC_INFO: u32 = _IOR::<lb::lemnos_rp1_csi2_info>(b'L' as u32, 0xc0);
const IOC_STREAM_CREATE: u32 = _IOWR::<lb::lemnos_stream_create>(b'L' as u32, 0xc1);
const IOC_START: u32 = _IOW::<lb::lemnos_rp1_csi2_start>(b'L' as u32, 0xc2);
const IOC_STOP: u32 = _IO(b'L' as u32, 0xc3);
const IOC_FRAME_STARTS: u32 = _IOR::<lb::lemnos_rp1_csi2_frame_starts>(b'L' as u32, 0xc4);
const IOC_EMBEDDED: u32 = _IOW::<lb::lemnos_rp1_csi2_embedded>(b'L' as u32, 0xc5);
const IOC_CLAIM: u32 = _IOW::<lb::lemnos_rp1_csi2_claim>(b'L' as u32, 0xc6);
const IOC_DMA: u32 = _IOR::<lb::lemnos_rp1_csi2_dma>(b'L' as u32, 0xc7);
/// LEMNOS_PISP_FE_IOC_INFO (lemnos_pisp_fe.h): the one call of the front end's that needs no claim.
const FE_IOC_INFO: u32 = _IOR::<lb::lemnos_pisp_fe_info>(b'L' as u32, 0xc8);

const _: () = assert!(size_of::<lb::lemnos_rp1_csi2_info>() == 56);
const _: () = assert!(size_of::<lb::lemnos_rp1_csi2_start>() == 48);
const _: () = assert!(size_of::<lb::lemnos_rp1_csi2_frame_starts>() == 24);
const _: () = assert!(size_of::<lb::lemnos_rp1_csi2_embedded>() == 16);
const _: () = assert!(size_of::<lb::lemnos_rp1_csi2_claim>() == 8);
const _: () = assert!(size_of::<lb::lemnos_rp1_csi2_dma_part>() == 32);
const _: () = assert!(size_of::<lb::lemnos_rp1_csi2_dma>() == 64);
const _: () = assert!(size_of::<lb::lemnos_stream_create>() == 56);
const _: () =
    assert!(lb::LEMNOS_RP1_CSI2_PART_CSI2 == 1 << 0 && lb::LEMNOS_RP1_CSI2_PART_FE == 1 << 1);

// SAFETY: the uAPI structs are integers only, with no padding (the sizes above are the sums of their
// fields): every byte is initialised, and any bytes are a value.
unsafe impl AsBytes for lb::lemnos_rp1_csi2_info {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_rp1_csi2_start {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_rp1_csi2_frame_starts {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_rp1_csi2_embedded {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_rp1_csi2_claim {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_rp1_csi2_dma {}

/// `struct lemnos_stream_create` as this file copies it in and out: the stream's own type, wrapped so
/// the copy's traits are declared here and nowhere else.
#[repr(transparent)]
struct StreamCreate(lb::lemnos_stream_create);
// SAFETY: integers only (ten 32-bit words and two 64-bit ones), no padding: 56 bytes, asserted above.
unsafe impl FromBytes for StreamCreate {}
// SAFETY: as above.
unsafe impl AsBytes for StreamCreate {}

// The owned= list (rs/lists.rs).
use crate::lists::lem_block_owned;

/// Time for one line of `bytes` on `lanes` lanes at `mbps` each, rounded up, in us. Lanes and rate
/// are at least 1 (START checked them).
fn lrc_line_us(bytes: u64, lanes: u32, mbps: u32) -> u64 {
    let rate = u64::from(lanes) * u64::from(mbps);
    (bytes * 8 + rate - 1) / rate
}

fn rd(base: *mut c_void, off: usize) -> u32 {
    // SAFETY: `base` is one of the block's three windows, mapped at probe and checked to cover every
    // offset this file names; the mapping lasts until the device is unbound, after which nothing here
    // runs (`gone`, and the handler is freed).
    unsafe { bindings::readl(base.cast::<u8>().wrapping_add(off).cast()) }
}

fn wr(base: *mut c_void, off: usize, val: u32) {
    // SAFETY: as rd.
    unsafe { bindings::writel(val, base.cast::<u8>().wrapping_add(off).cast()) };
}

/// ktime_get_ns(): CLOCK_MONOTONIC, the clock every frame's time is on.
fn now_ns() -> u64 {
    // SAFETY: the monotonic clock may be read from any context.
    (unsafe { bindings::ktime_get() }) as u64
}

/// One frame's buffer on a channel: the stream's slot, and whether it is held.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct LrcFrame {
    pub(crate) slot: Slot,
    pub(crate) valid: bool,
}

/// The embedded data channel (CSI-2 data type 0x12 on the image's virtual channel): its own stream,
/// its own buffers, its own frame accounting. See START, and the emb_ functions of the frame path.
#[repr(C)]
pub(crate) struct LrcEmb {
    /// Asked for before START; -1: none.
    pub(crate) req_fd: c_int,
    pub(crate) req_bytes: u32,
    /// Non-null while streaming.
    pub(crate) stream: *mut Stream,
    /// The line's length, from START.
    pub(crate) bytes: u32,
    /// cur: a line being written; next: armed.
    pub(crate) cur: LrcFrame,
    pub(crate) next: LrcFrame,
    /// cur's frame, and its start.
    pub(crate) seq: u64,
    pub(crate) fs_ns: u64,
    /// The channel's line count at the last frame end.
    pub(crate) fe_line: u16,
}

/// A stop's outcome (LEMNOS_RP1_CSI2_STOP_*), how long it took or waited, and its bound, in us.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct LrcStop {
    pub(crate) outcome: u32,
    pub(crate) us: u32,
    pub(crate) bound_us: u32,
}

/// One open file of the node: the parts it has claimed (LEMNOS_RP1_CSI2_PART_*). `parts` is written
/// only by this file's own CLAIM, under the receiver's `ctl`; the file's own calls read it.
#[repr(C)]
pub(crate) struct LrcOpen {
    pub(crate) l: *mut LrcDev,
    pub(crate) parts: u32,
}

/// The receiver's state. The front end on the same block (rs/pisp_fe.rs) reads `ctl`, `gone`,
/// `wedged`, `stream`, `max_lanes`, `irq` and `dev` of it. Zeroed memory is a valid start: probe
/// initialises the locks, the completions and the work in place.
#[repr(C)]
pub(crate) struct LrcDev {
    /// The block, each open file, each attached stream.
    pub(crate) ref_: bindings::kref,
    /// Held until freed.
    pub(crate) dev: *mut bindings::device,
    pub(crate) dma: *mut c_void,
    pub(crate) host: *mut c_void,
    pub(crate) cfg: *mut c_void,
    pub(crate) irq: c_int,
    pub(crate) host_version: u32,
    pub(crate) max_lanes: u32,
    /// Clock, then data lanes 0-3.
    pub(crate) polarity: [u8; 5],
    pub(crate) misc: bindings::miscdevice,
    pub(crate) name: [c_char; 64],
    /// The file that claims each part (index: the part's bit, LEMNOS_RP1_CSI2_PART_*); under ctl.
    pub(crate) owner: [*mut LrcOpen; 2],

    /// Stream creation against unbinding.
    pub(crate) going: bindings::rw_semaphore,
    /// START, STOP, close, unbind.
    pub(crate) ctl: bindings::mutex,
    /// Unbound: every register access is off.
    pub(crate) gone: bool,
    /// WEDGED_*: whose stop couldn't be confirmed, and hasn't been seen to stop since. The front end
    /// sets and clears its bit too, holding `ctl` as well.
    pub(crate) wedged: u8,
    /// STOP_ARM_*: the FE_ACKs a stop waits for, set under lock.
    pub(crate) stop_armed: u8,
    /// STOP_ARM_*: a frame start the handler saw while the stop was armed (under lock).
    pub(crate) stop_fs: u8,
    /// STOP_ARM_*: the channels a wedge waits on for the late frame end, and those it has seen (under lock).
    pub(crate) wedge_armed: u8,
    pub(crate) wedge_seen: u8,
    /// The stop's FE_ACK after its FORCE: image, then embedded.
    pub(crate) stop_done: [bindings::completion; 2],
    /// The streams an unconfirmed stop kept (image, embedded): the receiver's references, from
    /// Handle::quarantine, given back when the DMA is seen to stop. Under `ctl`.
    pub(crate) held: [*mut Stream; 2],
    /// The last stop, for LEMNOS_RP1_CSI2_IOC_DMA. Under `ctl`.
    pub(crate) last_stop: LrcStop,
    /// When the receiver's wedge began, and the looks since. Under `ctl`.
    pub(crate) wedged_ns: u64,
    pub(crate) unwedge_attempt: u32,
    /// Looks for the late frame end while wedged: `lrc_unwedge`.
    pub(crate) unwedge: bindings::delayed_work,
    /// The front end's: its last stop, the stream its unconfirmed stop kept, when that began. Under
    /// `ctl`; the front end's own (rs/pisp_fe.rs).
    pub(crate) fe_stop: LrcStop,
    pub(crate) fe_held: *mut Stream,
    pub(crate) fe_wedged_ns: u64,

    /// The front end, when the block has one; null otherwise.
    pub(crate) fe: *mut LemPispFe,
    /// The frame path, everything below.
    pub(crate) lock: bindings::spinlock_t,
    /// Non-null while streaming.
    pub(crate) stream: *mut Stream,
    pub(crate) streaming: bool,
    pub(crate) fs_int: bool,
    pub(crate) fe_int: bool,
    /// Being written; in the channel's shadow registers.
    pub(crate) cur: LrcFrame,
    pub(crate) next: LrcFrame,
    /// The latest frame start, armed or not (0 before one).
    pub(crate) fs_ns: u64,
    /// The channel's line count at the last frame end.
    pub(crate) fe_line: u16,
    pub(crate) start: lb::lemnos_rp1_csi2_start,
    pub(crate) emb: LrcEmb,
    pub(crate) frames: u64,
    pub(crate) written: u64,
    pub(crate) dropped: u64,
    pub(crate) errors: u64,
    /// Arms again while nothing is armed and no buffer was free; see Frame::arm.
    pub(crate) retry: bindings::delayed_work,
}

/// The receiver, by address. One is only made from a pointer whose holder has a
/// reference (the block's, an open file's, an attached stream's) or from the handler, which is freed
/// before the block's reference is given up; so the receiver is alive wherever one is used.
#[derive(Clone, Copy)]
struct Lrc(*mut LrcDev);

/// `ctl`, held: START, STOP, close and unbind. Unlocked when dropped.
struct Ctl {
    l: Lrc,
}

/// `going`, held for reading: stream creation against unbinding. Released when dropped.
struct Going {
    l: Lrc,
}

/// `lock`, held: the frame path. Released when dropped, with interrupts as they were.
struct Frame {
    l: Lrc,
    /// Taken outside the handler, with interrupts turned off.
    irq_off: bool,
}

impl Lrc {
    // Set by probe before the handler is requested and the node registered, and never written again.

    fn dma(self) -> *mut c_void {
        // SAFETY: the receiver is alive (see Lrc); the field is written only by probe.
        unsafe { (*self.0).dma }
    }

    fn host(self) -> *mut c_void {
        // SAFETY: as dma.
        unsafe { (*self.0).host }
    }

    fn cfg(self) -> *mut c_void {
        // SAFETY: as dma.
        unsafe { (*self.0).cfg }
    }

    fn dev(self) -> *mut bindings::device {
        // SAFETY: as dma.
        unsafe { (*self.0).dev }
    }

    fn irq(self) -> c_int {
        // SAFETY: as dma.
        unsafe { (*self.0).irq }
    }

    fn max_lanes(self) -> u32 {
        // SAFETY: as dma.
        unsafe { (*self.0).max_lanes }
    }

    fn host_version(self) -> u32 {
        // SAFETY: as dma.
        unsafe { (*self.0).host_version }
    }

    fn polarity(self, lane: usize) -> u8 {
        // SAFETY: as dma; `lane` is 0 to 4, the array's length.
        unsafe { (*self.0).polarity[lane] }
    }

    /// The front end, when the block has one; null otherwise.
    fn fe(self) -> *mut LemPispFe {
        // SAFETY: as dma.
        unsafe { (*self.0).fe }
    }

    /// The stop's completion for the image channel (0) or the embedded one (1).
    fn stop_done(self, which: usize) -> *mut bindings::completion {
        // SAFETY: the receiver is alive; only the address is taken, of an element inside the array.
        unsafe { &raw mut (*self.0).stop_done[which] }
    }

    fn retry(self) -> *mut bindings::delayed_work {
        // SAFETY: the receiver is alive; only the field's address is taken.
        unsafe { &raw mut (*self.0).retry }
    }

    fn unwedge(self) -> *mut bindings::delayed_work {
        // SAFETY: the receiver is alive; only the field's address is taken.
        unsafe { &raw mut (*self.0).unwedge }
    }

    /// The next look for the late frame end, `ms` from now.
    fn queue_unwedge(self, ms: u32) {
        // SAFETY: the kernel's own queue, set before any module runs and never changed.
        let wq = unsafe { bindings::system_wq };
        // SAFETY: the work was initialised at probe and lives as long as the receiver; unbinding
        // cancels it before the receiver can go.
        unsafe {
            bindings::queue_delayed_work_on(
                bindings::wq_misc_consts_WORK_CPU_UNBOUND as c_int,
                wq,
                self.unwedge(),
                msecs_to_jiffies(ms),
            )
        };
    }

    fn device(&self) -> &Device {
        // SAFETY: the receiver holds a reference to its device until it is freed.
        unsafe { Device::from_raw(self.dev()) }
    }

    fn ref_get(self) {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let r = unsafe { &raw mut (*self.0).ref_.refcount };
        // SAFETY: kref_get: the caller holds a reference, so the count is not zero.
        unsafe { bindings::refcount_inc(r) };
    }

    /// kref_put(&l->ref, lrc_free): the receiver is freed with its last reference.
    fn ref_put(self) {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let r = unsafe { &raw mut (*self.0).ref_.refcount };
        // SAFETY: the caller gives up a reference it holds.
        if unsafe { bindings::refcount_dec_and_test(r) } {
            self.free();
        }
    }

    /// The last reference is gone: nothing else can reach the receiver.
    fn free(self) {
        let fe = self.fe();
        if !fe.is_null() {
            // SAFETY: the front end probe made, destroyed once, with its receiver.
            unsafe { lem_pisp_fe_destroy(fe.cast()) };
        }
        // SAFETY: the reference probe took with get_device.
        unsafe { bindings::put_device(self.dev()) };
        // SAFETY: only the field's address is taken.
        let ctl = unsafe { &raw mut (*self.0).ctl };
        // SAFETY: initialised at probe, and unlocked: no reference is left to hold it.
        unsafe { bindings::mutex_destroy(ctl) };
        // SAFETY: the allocation probe made with KBox, freed once.
        drop(unsafe { KBox::from_raw(self.0) });
    }

    fn ctl(self) -> Ctl {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let m = unsafe { &raw mut (*self.0).ctl };
        // SAFETY: the mutex is initialised at probe; process context.
        unsafe { bindings::mutex_lock(m) };
        Ctl { l: self }
    }

    fn going(self) -> Going {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let sem = unsafe { &raw mut (*self.0).going };
        // SAFETY: the semaphore is initialised at probe; process context.
        unsafe { bindings::down_read(sem) };
        Going { l: self }
    }

    /// spin_lock_irqsave(&l->lock): the frame path from anywhere but the handler.
    fn frame(self) -> Frame {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let lock = unsafe { &raw mut (*self.0).lock };
        // SAFETY: the lock is initialised at probe; released by Frame's drop.
        unsafe { bindings::spin_lock_irq_disable(lock) };
        Frame {
            l: self,
            irq_off: true,
        }
    }

    /// spin_lock(&l->lock): the frame path from the handler, where interrupts are already off.
    fn frame_in_handler(self) -> Frame {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let lock = unsafe { &raw mut (*self.0).lock };
        // SAFETY: the lock is initialised at probe; released by Frame's drop.
        unsafe { bindings::spin_lock(lock) };
        Frame {
            l: self,
            irq_off: false,
        }
    }

    /// The frame stream, or null: read under the frame lock by a caller that holds `ctl`, so it stays
    /// as read until `ctl` is released.
    fn stream_now(self) -> *mut Stream {
        self.frame().stream()
    }

    /// queue_delayed_work(system_highpri_wq, &l->retry, LRC_RETRY_MS): on the high-priority queue so a
    /// busy CPU delays it least. Unless already queued.
    fn queue_retry(self) {
        // SAFETY: the kernel's own queue, set before any module runs and never changed.
        let wq = unsafe { bindings::system_highpri_wq };
        // SAFETY: the work was initialised at probe and lives as long as the receiver.
        unsafe {
            bindings::queue_delayed_work_on(
                bindings::wq_misc_consts_WORK_CPU_UNBOUND as c_int,
                wq,
                self.retry(),
                msecs_to_jiffies(LRC_RETRY_MS),
            )
        };
    }

    // The D-PHY test interface (dphy.c).

    fn tst_ctrl0(self, mask: u32, val: u32) {
        let host = self.host();
        wr(
            host,
            HOST_PHY_TST_CTRL0,
            (rd(host, HOST_PHY_TST_CTRL0) & !mask) | val,
        );
    }

    fn tst_ctrl1(self, mask: u32, val: u32) {
        let host = self.host();
        wr(
            host,
            HOST_PHY_TST_CTRL1,
            (rd(host, HOST_PHY_TST_CTRL1) & !mask) | val,
        );
    }

    fn dphy_transaction(self, code: u8, data: u8) {
        self.tst_ctrl0(2, 2); // tstclk 1
        self.tst_ctrl1(1 << 16, 0); // testen 0
        self.tst_ctrl1(0xff, u32::from(code));
        self.tst_ctrl1(1 << 16, 1 << 16);
        self.tst_ctrl0(2, 0);
        self.tst_ctrl1(1 << 16, 0);
        self.tst_ctrl1(0xff, u32::from(data));
        self.tst_ctrl0(2, 2);
    }

    /// dphy_start: the host in reset, the lanes, the PHY's rate and polarities.
    fn dphy_start(self, lanes: u32, mbps: u32) {
        let host = self.host();
        wr(host, HOST_RESETN, 0);
        wr(host, HOST_N_LANES, lanes - 1);
        wr(host, HOST_PHY_RSTZ, 0);
        wr(host, HOST_PHY_SHUTDOWNZ, 0);
        self.tst_ctrl0(2, 2);
        self.tst_ctrl1(1 << 16, 0);
        self.tst_ctrl0(1, 1); // tstclr
        usleep_range(15, 20);
        self.tst_ctrl0(1, 0);
        usleep_range(15, 20);
        self.dphy_transaction(DPHY_HS_RX_CTRL_LANE0, hsfreqrange(mbps) << 1);
        self.dphy_transaction(DPHY_CLK_PN_SWAP, self.polarity(0));
        self.dphy_transaction(DPHY_D0_PN_SWAP, self.polarity(1));
        self.dphy_transaction(DPHY_D1_PN_SWAP, self.polarity(2));
        self.dphy_transaction(DPHY_D2_PN_SWAP, self.polarity(3));
        self.dphy_transaction(DPHY_D3_PN_SWAP, self.polarity(4));
        usleep_range(5, 10);
        wr(host, HOST_PHY_SHUTDOWNZ, 1);
        usleep_range(5, 10);
        wr(host, HOST_PHY_RSTZ, 1);
        wr(host, HOST_RESETN, 0xffff_ffff);
        usleep_range(10, 50);
    }

    /// Programs a channel's buffer: ADDR0 last, it latches the others for the next frame. Only ever
    /// a device address and size from lem_stream_next(). Frame lock held.
    fn program(self, ch: usize, addr: u64, bytes: u32, stride: u32) {
        let dma = self.dma();
        let addr = addr >> 4;
        wr(dma, csi2_ch_length(ch), bytes >> 4);
        wr(dma, csi2_ch_stride(ch), stride >> 4);
        wr(dma, csi2_ch_addr1(ch), (addr >> 32) as u32);
        wr(dma, csi2_ch_addr0(ch), (addr & 0xffff_ffff) as u32);
    }
}

fn hsfreqrange(mbps: u32) -> u8 {
    // The D-PHY databook's table 5-1, as dphy.c has it.
    const TABLE: [(u16, u8); 39] = [
        (89, 0x00),
        (99, 0x10),
        (109, 0x20),
        (129, 0x01),
        (139, 0x11),
        (149, 0x21),
        (169, 0x02),
        (179, 0x12),
        (199, 0x22),
        (219, 0x03),
        (239, 0x13),
        (249, 0x23),
        (269, 0x04),
        (299, 0x14),
        (329, 0x05),
        (359, 0x15),
        (399, 0x25),
        (449, 0x06),
        (499, 0x16),
        (549, 0x07),
        (599, 0x17),
        (649, 0x08),
        (699, 0x18),
        (749, 0x09),
        (799, 0x19),
        (849, 0x29),
        (899, 0x39),
        (949, 0x0a),
        (999, 0x1a),
        (1049, 0x2a),
        (1099, 0x3a),
        (1149, 0x0b),
        (1199, 0x1b),
        (1249, 0x2b),
        (1299, 0x3b),
        (1349, 0x0c),
        (1399, 0x1c),
        (1449, 0x2c),
        (1500, 0x3c),
    ];
    // The first row the rate fits, or the last row.
    let last = TABLE[TABLE.len() - 1].1;
    TABLE[..TABLE.len() - 1]
        .iter()
        .find(|row| mbps <= u32::from(row.0))
        .map_or(last, |row| row.1)
}

impl Drop for Ctl {
    fn drop(&mut self) {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let m = unsafe { &raw mut (*self.l.0).ctl };
        // SAFETY: locked by Lrc::ctl, on this task.
        unsafe { bindings::mutex_unlock(m) };
    }
}

impl Ctl {
    /// Unbound: every register access is off. Written under `going` and `ctl` both, so either reads it.
    fn gone(&self) -> bool {
        // SAFETY: `ctl` is held (`self`), and the one write is made holding it.
        unsafe { (*self.l.0).gone }
    }

    /// WEDGED_*: whose stop couldn't be confirmed. The front end sets its bit too, holding `ctl` as well.
    fn wedged(&mut self) -> &mut u8 {
        // SAFETY: `ctl` is held (`self`), and it guards the field.
        unsafe { &mut (*self.l.0).wedged }
    }

    /// The streams an unconfirmed stop kept.
    fn held(&mut self) -> &mut [*mut Stream; 2] {
        // SAFETY: `ctl` is held (`self`), and it guards the field.
        unsafe { &mut (*self.l.0).held }
    }

    fn last_stop(&mut self) -> &mut LrcStop {
        // SAFETY: as held.
        unsafe { &mut (*self.l.0).last_stop }
    }

    fn wedged_ns(&mut self) -> &mut u64 {
        // SAFETY: as held.
        unsafe { &mut (*self.l.0).wedged_ns }
    }

    fn unwedge_attempt(&mut self) -> &mut u32 {
        // SAFETY: as held.
        unsafe { &mut (*self.l.0).unwedge_attempt }
    }

    /// The file that claims the part whose bit is `part`.
    fn owner(&mut self, part: usize) -> &mut *mut LrcOpen {
        // SAFETY: `ctl` is held (`self`), and it guards the array; `part` is below PARTS, its length.
        unsafe { &mut (*self.l.0).owner[part] }
    }

    /// The embedded stream asked for before START: its fd, or -1 for none.
    fn emb_req_fd(&mut self) -> &mut c_int {
        // SAFETY: `ctl` is held (`self`), and it guards the field.
        unsafe { &mut (*self.l.0).emb.req_fd }
    }

    fn emb_req_bytes(&mut self) -> &mut u32 {
        // SAFETY: `ctl` is held (`self`), and it guards the field.
        unsafe { &mut (*self.l.0).emb.req_bytes }
    }
}

impl Drop for Going {
    fn drop(&mut self) {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let sem = unsafe { &raw mut (*self.l.0).going };
        // SAFETY: taken for reading by Lrc::going, on this task.
        unsafe { bindings::up_read(sem) };
    }
}

impl Going {
    fn gone(&self) -> bool {
        // SAFETY: `going` is held for reading (`self`), and the one write is made holding it for writing.
        unsafe { (*self.l.0).gone }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: the receiver is alive; only the field's address is taken.
        let lock = unsafe { &raw mut (*self.l.0).lock };
        if self.irq_off {
            // SAFETY: taken by Lrc::frame, on this CPU.
            unsafe { bindings::spin_unlock_irq_enable(lock) };
        } else {
            // SAFETY: taken by Lrc::frame_in_handler, on this CPU.
            unsafe { bindings::spin_unlock(lock) };
        }
    }
}

/// The fields the frame lock alone guards: each is reached only through a held `Frame`.
macro_rules! frame_fields {
    ($($name:ident: $ty:ty => $($field:ident).+;)*) => {
        impl Frame {
            $(
                fn $name(&mut self) -> &mut $ty {
                    // SAFETY: the frame lock is held (`self`) and guards this field, which nothing
                    // touches without it; the borrow ends before the lock is released.
                    unsafe { &mut (*self.l.0).$($field).+ }
                }
            )*
        }
    };
}

frame_fields! {
    stop_armed: u8 => stop_armed;
    stop_fs: u8 => stop_fs;
    wedge_armed: u8 => wedge_armed;
    wedge_seen: u8 => wedge_seen;
    fs_int: bool => fs_int;
    fe_int: bool => fe_int;
    cur: LrcFrame => cur;
    next: LrcFrame => next;
    fs_ns: u64 => fs_ns;
    fe_line: u16 => fe_line;
    frames: u64 => frames;
    written: u64 => written;
    dropped: u64 => dropped;
    errors: u64 => errors;
    emb_bytes: u32 => emb.bytes;
    emb_cur: LrcFrame => emb.cur;
    emb_next: LrcFrame => emb.next;
    emb_seq: u64 => emb.seq;
    emb_fs_ns: u64 => emb.fs_ns;
    emb_fe_line: u16 => emb.fe_line;
}

/// A frame's slot as the stream's calls take it.
fn slot_of(frame: &LrcFrame) -> &Slot {
    &frame.slot
}

impl Frame {
    // Written holding `ctl` and the frame lock both (START and the stop), so either reads them.

    /// The frame stream: non-null while streaming.
    fn stream(&self) -> *mut Stream {
        // SAFETY: the frame lock is held (`self`); every write is made holding it.
        (unsafe { (*self.l.0).stream }).cast()
    }

    /// The embedded stream: non-null while streaming with one.
    fn emb_stream(&self) -> *mut Stream {
        // SAFETY: as stream.
        (unsafe { (*self.l.0).emb.stream }).cast()
    }

    fn streaming(&self) -> bool {
        // SAFETY: as stream.
        unsafe { (*self.l.0).streaming }
    }

    /// What START was given.
    fn start(&self) -> &lb::lemnos_rp1_csi2_start {
        // SAFETY: as stream; the borrow ends before the lock is released.
        unsafe { &(*self.l.0).start }
    }

    /// `ctl` held as well.
    fn set_streams(&mut self, stream: *mut Stream, emb: *mut Stream) {
        // SAFETY: `ctl` and the frame lock are both held, which is what every reader holds one of.
        unsafe { (*self.l.0).stream = stream.cast() };
        // SAFETY: as above.
        unsafe { (*self.l.0).emb.stream = emb.cast() };
    }

    /// `ctl` held as well.
    fn set_streaming(&mut self, on: bool) {
        // SAFETY: as set_streams.
        unsafe { (*self.l.0).streaming = on };
    }

    /// `ctl` held as well.
    fn set_start(&mut self, start: &lb::lemnos_rp1_csi2_start) {
        // SAFETY: as set_streams.
        unsafe { (*self.l.0).start = *start };
    }

    /// Gives the channel the stream's next buffer when it has none waiting.
    ///
    /// With nothing armed the channel makes no frame start, so no interrupt would ever come to try
    /// again: a stream started before its consumer gave memory, or a consumer holding every buffer,
    /// would stop the camera for good. So when no buffer is free, this tries again every LRC_RETRY_MS
    /// until one is or the channel stops. A stream's own error is not retried: the stream is over.
    ///
    /// A frame start is counted when the channel raises its interrupt for it (lrc_isr, sof), whether
    /// or not a buffer is armed for it: with none armed it is a drop. A channel with no buffer address
    /// raises no interrupt, so a start then is in no count and has no sequence number; the board
    /// showed that (testing/board-runs/D5.md, "Nothing armed, nothing seen"), and the stock driver is
    /// the same. The count is therefore the starts the channel showed.
    fn arm(&mut self) {
        if !self.streaming() || self.next().valid {
            return;
        }
        let stream = self.stream();
        let slot = (&raw mut self.next().slot).cast::<Slot>();
        // SAFETY: streaming, so the stream is attached and alive; the slot is the receiver's own and
        // is written whole.
        let ret = unsafe { lem_stream_next(stream, slot) };
        if ret != 0 {
            // Nothing to arm: ADDR0 goes to zero, as emb_arm does. Left alone it would still hold the
            // buffer the last frame end gave away, and the next frame start would latch it, so the
            // frame after would be written into a buffer the client already holds.
            wr(self.l.dma(), csi2_ch_addr0(LRC_CH), 0);
            if ret == ENOBUFS.to_errno() {
                self.l.queue_retry();
            }
            return;
        }
        self.next().valid = true;
        let (addr, bytes) = (self.next().slot.addr, self.next().slot.bytes);
        let stride = self.start().stride;
        self.l.program(LRC_CH, addr, bytes, stride);
    }

    /// `arm` for the embedded channel. Its lines are PACK_BYTES, so the stride is zero, as the stock
    /// driver's meta node. The rule: the channel's ADDR0 is the next buffer's address, or zero when
    /// none is armed, so the channel latches nothing it could write into a buffer that has been given
    /// away (and a channel with no buffer address makes no frame start, as the board showed).
    fn emb_arm(&mut self) {
        let stream = self.emb_stream();
        if !self.streaming() || stream.is_null() || self.emb_next().valid {
            return;
        }
        let slot = (&raw mut self.emb_next().slot).cast::<Slot>();
        // SAFETY: as arm, for the embedded stream.
        let ret = unsafe { lem_stream_next(stream, slot) };
        if ret != 0 {
            if ret == ENOBUFS.to_errno() {
                self.l.queue_retry();
            }
            wr(self.l.dma(), csi2_ch_addr0(LRC_EMB_CH), 0);
            return;
        }
        self.emb_next().valid = true;
        let (addr, bytes) = (self.emb_next().slot.addr, self.emb_next().slot.bytes);
        self.l.program(LRC_EMB_CH, addr, bytes, 0);
    }

    /// The embedded channel's frame before the one starting now is over, and the frame count is still
    /// the last one's. A line that started and never ended is given back empty (the stream drops it).
    /// A frame with no embedded line publishes nothing: the buffer armed for the next line is still
    /// the next frame's, the one the channel latched at its last start, so it is not touched. The
    /// sequence numbers show the gap.
    fn emb_frame_end(&mut self) {
        let stream = self.emb_stream();
        if stream.is_null() || *self.frames() == 0 {
            return;
        }
        if self.emb_cur().valid {
            // A line started and never ended: cut short.
            let (seq, fs_ns) = (*self.emb_seq(), *self.emb_fs_ns());
            Handle(stream).complete(slot_of(self.emb_cur()), 0, seq, fs_ns);
            self.emb_cur().valid = false;
        }
    }

    /// cfe_sof_isr_handler.
    fn sof(&mut self, now: u64) {
        self.emb_frame_end();
        if self.cur().valid {
            // A start before the last frame's end. The cut frame's start was counted at `frames`: its
            // sequence is one below.
            let (seq, fs_ns) = (self.frames().wrapping_sub(1), *self.fs_ns());
            Handle(self.stream()).complete(slot_of(self.cur()), 0, seq, fs_ns);
            *self.errors() += 1;
            self.cur().valid = false;
        }
        let (next, cur): (*const LrcFrame, *mut LrcFrame) = (self.next(), self.cur());
        // SAFETY: two fields of the receiver, both the frame lock's (held); plain data, so the copy
        // is `l->cur = l->next`.
        unsafe { ptr::copy_nonoverlapping(next, cur, 1) };
        self.next().valid = false;
        // The latest start, armed or not: LEMNOS_RP1_CSI2_IOC_FRAME_STARTS names it.
        *self.fs_ns() = now;
        if !self.cur().valid {
            *self.dropped() += 1;
        }
        *self.frames() += 1;
        *self.fs_int() = true;
        *self.fe_int() = false;
        self.arm();
    }

    /// The embedded channel's frame start. Its line goes to the buffer armed for it, and that
    /// buffer's frame is the one starting now.
    fn emb_sof(&mut self, now: u64) {
        if self.emb_stream().is_null() {
            return;
        }
        if !self.emb_next().valid {
            // None armed: the line has nowhere to go.
            return;
        }
        let (next, cur): (*const LrcFrame, *mut LrcFrame) = (self.emb_next(), self.emb_cur());
        // SAFETY: as sof's copy: `l->emb.cur = l->emb.next`.
        unsafe { ptr::copy_nonoverlapping(next, cur, 1) };
        self.emb_next().valid = false;
        let seq = self.frames().wrapping_sub(1);
        *self.emb_seq() = seq;
        *self.emb_fs_ns() = now;
        self.emb_arm();
    }

    /// cfe_eof_isr_handler. `line` is the channel's line count now (the low half of CSI2_CH_DEBUG): it
    /// counts the lines the channel wrote, and none while nothing is armed, so the lines since the
    /// last frame end are this frame's. A frame is published only when that is the height START set:
    /// a frame cut short (one caught mid-transmission at a start, measured on the board as a single
    /// line) is a frame gone wrong. Read at the frame end, which the next frame's lines can't reach
    /// before its vertical blanking is over.
    fn eof(&mut self, line: u16, now: u64) {
        let lines = line.wrapping_sub(*self.fe_line());
        *self.fe_line() = line;
        if self.cur().valid {
            let stream = Handle(self.stream());
            // frames is the FS count: this frame's number is one below it.
            let (seq, fs_ns) = (self.frames().wrapping_sub(1), *self.fs_ns());
            if lines == self.start().height as u16 {
                *self.written() += 1;
                let bytes = self.cur().slot.bytes;
                let slot: *const Slot = slot_of(self.cur());
                // SAFETY: streaming, so the stream is attached and alive; the slot is the one it gave.
                unsafe { lem_stream_complete_at(stream.0, slot, bytes, seq, fs_ns, now) };
            } else {
                stream.complete(slot_of(self.cur()), 0, seq, fs_ns);
                *self.errors() += 1;
            }
            self.cur().valid = false;
        }
        *self.fe_int() = true;
        *self.fs_int() = false;
        self.arm();
    }

    /// The embedded channel's frame end: its one line is in, when the channel counted exactly one line
    /// since the last end (stock: the meta node's frame height is 1). Otherwise the buffer goes out
    /// empty.
    fn emb_eof(&mut self, line: u16, now: u64) {
        let lines = line.wrapping_sub(*self.emb_fe_line());
        *self.emb_fe_line() = line;
        if !self.emb_cur().valid {
            return;
        }
        let stream = self.emb_stream();
        let bytes = if lines == 1 { *self.emb_bytes() } else { 0 };
        let (seq, fs_ns) = (*self.emb_seq(), *self.emb_fs_ns());
        let slot: *const Slot = slot_of(self.emb_cur());
        // SAFETY: a line is being written, so the embedded stream is attached and alive; the slot is
        // the one it gave.
        unsafe { lem_stream_complete_at(stream, slot, bytes, seq, fs_ns, now) };
        self.emb_cur().valid = false;
        self.emb_arm();
    }
}

/// The retry work: arms again while nothing is armed and no buffer was free; see `Frame::arm`.
unsafe extern "C" fn lrc_retry(work: *mut bindings::work_struct) {
    // The work is the delayed work's, which is the receiver's `retry`; it is cancelled before the
    // receiver stops streaming for good, so the receiver is alive.
    let off = offset_of!(bindings::delayed_work, work) + offset_of!(LrcDev, retry);
    let l = Lrc(work.wrapping_byte_sub(off).cast());
    let mut f = l.frame();
    f.arm();
    f.emb_arm();
}

/// The block's interrupt: the DMA channels' and, when the block has one, the front end's.
unsafe extern "C" fn lrc_isr(_irq: c_int, data: *mut c_void) -> bindings::irqreturn_t {
    // `data` is the receiver, as given to devm_request_irq; the handler is freed before it is.
    let l = Lrc(data.cast());
    let now = now_ns();
    let mut handled = false;
    let mut status = 0;

    let sts = rd(l.cfg(), MIPICFG_INTS);
    if sts & MIPICFG_INT_CSI_DMA != 0 {
        // Read and cleared under the frame lock, with the stop's arming: a frame end this reads was
        // latched after the stop cleared the status, so it follows the stop's FORCE or it isn't
        // counted at all.
        let mut f = l.frame_in_handler();
        status = rd(l.dma(), CSI2_STATUS);
        if status != 0 {
            wr(l.dma(), CSI2_STATUS, status); // write back to clear
            handled = true;
        }
        if status & irq_fe_ack(LRC_CH) != 0 && *f.stop_armed() & STOP_ARM_IMAGE != 0 {
            // SAFETY: the completion is initialised at probe and lives as long as the receiver.
            unsafe { bindings::complete(l.stop_done(0)) };
        }
        if status & irq_fe_ack(LRC_EMB_CH) != 0 && *f.stop_armed() & STOP_ARM_EMB != 0 {
            // SAFETY: as above.
            unsafe { bindings::complete(l.stop_done(1)) };
        }
        // A frame start while the stop is armed: a frame began with an address the channel latched
        // before its ADDR0 went to zero, so only the frame end after it confirms the stop.
        if status & irq_fs(LRC_CH) != 0 {
            *f.stop_fs() |= *f.stop_armed() & STOP_ARM_IMAGE;
        }
        if status & irq_fs(LRC_EMB_CH) != 0 {
            *f.stop_fs() |= *f.stop_armed() & STOP_ARM_EMB;
        }
    }
    let mut sof = status & irq_fs(LRC_CH) != 0;
    let eof = status & irq_fe_ack(LRC_CH) != 0;
    let emb_sof = status & irq_fs(LRC_EMB_CH) != 0;
    let emb_eof = status & irq_fe_ack(LRC_EMB_CH) != 0;
    // The channel feeding the front end: only its frame starts matter, and before the front end's
    // interrupt, which comes after them.
    let fe = l.fe();
    if sof && !fe.is_null() {
        // SAFETY: the front end lives as long as its receiver; this is its frame-start hook.
        unsafe { lem_pisp_fe_csi_fs(fe.cast()) };
    }
    if !fe.is_null() && sts & MIPICFG_INT_PISP_FE != 0 {
        // SAFETY: as above; this is its interrupt.
        unsafe { lem_pisp_fe_isr(fe.cast(), now) };
        handled = true;
    }
    if status == 0 {
        return if handled {
            bindings::irqreturn::IRQ_HANDLED
        } else {
            bindings::irqreturn::IRQ_NONE
        };
    }
    let line = if eof {
        (rd(l.dma(), csi2_ch_debug(LRC_CH)) & 0xffff) as u16
    } else {
        0
    };
    let emb_line = if emb_eof {
        (rd(l.dma(), csi2_ch_debug(LRC_EMB_CH)) & 0xffff) as u16
    } else {
        0
    };

    let mut f = l.frame_in_handler();
    if f.streaming() {
        // The embedded line of the frame ending is in before the next frame's start.
        if emb_eof {
            f.emb_eof(emb_line, now);
        }
        // The orders cfe_isr handles: FE then FS of the next frame, FS and FE of one frame, and an FS
        // with no FE before it. The last, unlike cfe_isr, is a frame this can't call complete, so sof
        // flags it rather than publishing it.
        if eof {
            if sof && !*f.fs_int() {
                f.sof(now);
                sof = false;
            }
            f.eof(line, now);
        }
        if sof {
            f.sof(now);
        }
        // After the image's start, which closes the frame before it.
        if emb_sof {
            f.emb_sof(now);
        }
    }
    bindings::irqreturn::IRQ_HANDLED
}

/// The stop's bound, in us: max(4 frame periods, 250 ms) (docs/streams.md). The frame period is the
/// START layout's, height x line time: the contract period. DEBT-C's measured period replaces it when
/// it lands (B7).
fn stop_bound_us(st: &lb::lemnos_rp1_csi2_start) -> u32 {
    let frame_us = lrc_line_us(u64::from(st.stride), st.lanes, st.mbps) * u64::from(st.height);
    (4 * frame_us).max(LRC_STOP_MIN_US).min(u64::from(u32::MAX)) as u32
}

/// Microseconds since `t0` (a now_ns()), saturated.
fn us_since(t0: u64) -> u32 {
    (now_ns().saturating_sub(t0) / 1000).min(u64::from(u32::MAX)) as u32
}

/// The STOP_ARM_* channels whose FE_ACK came after their FORCE, within one bound from `t0` shared by
/// both. A still line counter is not evidence of a stop and is not used. Sleeps: never on the frame
/// path.
fn fe_ack_wait(l: Lrc, armed: u8, t0: u64, bound_us: u32) -> u8 {
    let mut acked = 0;
    for (bit, which) in [(STOP_ARM_IMAGE, 0), (STOP_ARM_EMB, 1)] {
        if armed & bit == 0 {
            continue;
        }
        let left = bound_us.saturating_sub(us_since(t0));
        // SAFETY: the exported conversion, valid for any value.
        let left = unsafe { bindings::__usecs_to_jiffies(left as c_uint) };
        // SAFETY: the completion is initialised at probe; process context, with `ctl` held and no
        // spinlock. A zero timeout only looks.
        if unsafe { bindings::wait_for_completion_timeout(l.stop_done(which), left) } != 0 {
            acked |= bit;
        }
    }
    acked
}

/// reinit_completion(): no completion left over from a stop before this one. Frame lock held, as the
/// handler completes under it.
fn reinit_completion(c: *mut bindings::completion) {
    // SAFETY: the completion is the receiver's, initialised at probe; no waiter exists (`ctl` is held
    // by the one caller, the stop), and the handler is kept out by the frame lock.
    unsafe { (*c).done = 0 };
}

/// Stops the channels, waits for them, and gives back what they held. Takes no stream lock of its own
/// (the stream's ctl may be held by a caller above us). Stops the receiver whole: the frame stream and
/// the embedded stream, if one is attached. `ctl` held, and streaming.
///
/// A channel's stop is confirmed by its FE_ACK after FORCE, or by the channel having been between
/// frames when FORCE landed with no frame start before the bound ran out. The board gives no FE_ACK for
/// a FORCE between frames (a sensor stopped first; testing/board-runs/CZ-MOD-FE-ACK.md), and a channel
/// between frames has nothing in flight: its next frame would start with the address its ADDR0 holds,
/// which the stop zeroes, and a frame start with an address raises FS, which the handler records while
/// the stop is armed. Only when every channel is confirmed does anything go back. Otherwise the streams
/// are quarantined (pinned, kept by the receiver) and the receiver is wedged until `lrc_unwedge` sees
/// the late FE_ACK.
fn stop_locked(ctl: &mut Ctl) {
    let l = ctl.l;
    let dma = l.dma();
    let (s, e, start, armed, mid);
    {
        let mut f = l.frame();
        s = f.stream();
        e = f.emb_stream();
        start = *f.start();
        f.set_streaming(false);
        reinit_completion(l.stop_done(0));
        reinit_completion(l.stop_done(1));
        armed = STOP_ARM_IMAGE | if e.is_null() { 0 } else { STOP_ARM_EMB };
        // Where each channel is. In a frame: its start was handled and its end wasn't. A frame end the
        // handler hasn't taken yet closed it. A frame start it hasn't taken is left pending: the
        // handler takes it once this lock is dropped, while the stop is armed (stop_fs).
        let pending = rd(dma, CSI2_STATUS);
        let in_frame = |open: bool, ch: usize| open && pending & irq_fe_ack(ch) == 0;
        mid = (if in_frame(*f.fs_int(), LRC_CH) { STOP_ARM_IMAGE } else { 0 })
            | (if !e.is_null() && in_frame(f.emb_cur().valid, LRC_EMB_CH) {
                STOP_ARM_EMB
            } else {
                0
            });
        // Only a frame end after the FORCE below is the stop's acknowledgement. An FE_ACK still in the
        // status is from before the stop, so it is cleared here; the handler counts one only while
        // the stop is armed.
        let emb_ack = if e.is_null() {
            0
        } else {
            irq_fe_ack(LRC_EMB_CH)
        };
        wr(dma, CSI2_STATUS, irq_fe_ack(LRC_CH) | emb_ack);
        *f.stop_fs() = 0;
        *f.stop_armed() = armed;
        // csi2_stop_channel writes FORCE alone, which drops the channel's IRQ enables. This keeps the
        // FE_ACK enable set, so the frame end that follows FORCE is delivered, and the FS enable, so a
        // frame that starts after FORCE is seen.
        wr(dma, csi2_ch_ctrl(LRC_CH), FORCE | IRQ_EN_FE_ACK | IRQ_EN_FS);
        wr(dma, csi2_ch_addr0(LRC_CH), 0);
        wr(dma, csi2_ch_addr0(LRC_CH), 0);
        if !e.is_null() {
            wr(dma, csi2_ch_ctrl(LRC_EMB_CH), FORCE | IRQ_EN_FE_ACK | IRQ_EN_FS);
            wr(dma, csi2_ch_addr0(LRC_EMB_CH), 0);
            wr(dma, csi2_ch_addr0(LRC_EMB_CH), 0);
        }
    }
    // Everything written above is posted across a bus: a read makes it land first.
    let _ = rd(dma, csi2_ch_ctrl(LRC_CH));
    let t0 = now_ns();
    let bound_us = stop_bound_us(&start);

    // A channel in a frame needs its frame end; one between frames needs the whole bound to pass
    // with no frame start, so the wait is the bound unless every channel's frame end comes first.
    let acked = fe_ack_wait(l, armed, t0, bound_us);
    let waited_us = us_since(t0);

    let started = {
        let mut f = l.frame();
        *f.stop_armed() = 0;
        wr(dma, CSI2_IRQ_MASK, 0);
        wr(l.cfg(), MIPICFG_INTE, 0);
        *f.stop_fs()
    };
    // Not streaming, so a retry finds nothing to do and doesn't come again.
    // SAFETY: the work was initialised at probe; process context, and the work takes no lock held here.
    unsafe { bindings::cancel_delayed_work_sync(l.retry()) };

    // No handler still holds cur/next, and none will start.
    // SAFETY: the block's interrupt, requested at probe; process context, no lock the handler takes.
    unsafe { bindings::synchronize_irq(l.irq() as c_uint) };
    // A start the handler took after the wait: in the count too.
    let started = started | *l.frame().stop_fs();

    let unconfirmed = armed & !acked & (mid | started);
    let outcome = if unconfirmed != 0 {
        STOP_UNCONFIRMED
    } else if acked == armed {
        STOP_ACKED
    } else {
        STOP_IDLE
    };
    *ctl.last_stop() = LrcStop {
        outcome,
        us: waited_us,
        bound_us,
    };

    if unconfirmed == 0 {
        let mut f = l.frame();
        release_slots(&mut f, s, e);
    } else {
        let (count, bytes) = pinned_of(s, e);
        // Not known to be finished: pinned, and refused until the DMA is seen to stop.
        dev_err!(
            l.device(),
            "DMA did not stop: no frame end after FORCE within {} us, with a frame in flight; {} buffers ({} bytes) stay pinned until it does\n",
            bound_us,
            count,
            bytes
        );
        *ctl.wedged() |= WEDGED_CSI2;
        let held = [
            Handle(s).quarantine(),
            if e.is_null() {
                ptr::null_mut()
            } else {
                Handle(e).quarantine()
            },
        ];
        *ctl.held() = held;
        *ctl.wedged_ns() = t0;
        *ctl.unwedge_attempt() = 0;
        {
            let mut f = l.frame();
            *f.wedge_armed() = unconfirmed;
            *f.wedge_seen() = 0;
        }
        l.queue_unwedge(LRC_UNWEDGE_FIRST_MS);
    }

    l.frame().set_streams(ptr::null_mut(), ptr::null_mut());
    // SAFETY: the stream is alive: the producer's reference to it is given up only by this call (and
    // a quarantined one is the receiver's own besides).
    if unsafe { lem_stream_detach(s) } {
        l.ref_put();
    }
    // SAFETY: as above, for the embedded stream.
    if !e.is_null() && unsafe { lem_stream_detach(e) } {
        l.ref_put();
    }
}

/// Gives back every buffer the channels held: the stop is confirmed. Frame lock held.
fn release_slots(f: &mut Frame, s: *mut Stream, e: *mut Stream) {
    if f.cur().valid {
        Handle(s).cancel(slot_of(f.cur()));
    }
    if f.next().valid {
        Handle(s).cancel(slot_of(f.next()));
    }
    f.cur().valid = false;
    f.next().valid = false;
    if !e.is_null() {
        if f.emb_cur().valid {
            Handle(e).cancel(slot_of(f.emb_cur()));
        }
        if f.emb_next().valid {
            Handle(e).cancel(slot_of(f.emb_next()));
        }
    }
    f.emb_cur().valid = false;
    f.emb_next().valid = false;
}

/// The dma-bufs both streams pin, and their bytes. Either may be null.
fn pinned_of(s: *mut Stream, e: *mut Stream) -> (u32, u64) {
    let mut out = (0, 0);
    for st in [s, e] {
        if !st.is_null() {
            let (n, b) = Handle(st).pinned();
            out = (out.0 + n, out.1 + b);
        }
    }
    out
}

/// While wedged: looks for the frame end after the stop's FORCE that never came in time. The handler's
/// interrupt is off once a stop is over, so the channel's status is read here; an FE_ACK in it is the
/// DMA stopping, for the channels the wedge waits on. Seen on all of them: every buffer goes back, the
/// quarantined streams are let go, and the wedge is over. Not yet: looks again, the gap doubling up to
/// LRC_UNWEDGE_MAX_MS, for as long as the receiver is bound.
unsafe extern "C" fn lrc_unwedge(work: *mut bindings::work_struct) {
    // The work is the delayed work's, which is the receiver's `unwedge`; unbinding cancels it before
    // the receiver can be freed, so the receiver is alive.
    let off = offset_of!(bindings::delayed_work, work) + offset_of!(LrcDev, unwedge);
    let l = Lrc(work.wrapping_byte_sub(off).cast());
    let held;
    {
        let mut ctl = l.ctl();
        if ctl.gone() || *ctl.wedged() & WEDGED_CSI2 == 0 {
            return;
        }
        let dma = l.dma();
        let stopped = {
            let mut f = l.frame();
            let status = rd(dma, CSI2_STATUS);
            let mut seen = 0;
            if status & irq_fe_ack(LRC_CH) != 0 {
                seen |= STOP_ARM_IMAGE;
            }
            if status & irq_fe_ack(LRC_EMB_CH) != 0 {
                seen |= STOP_ARM_EMB;
            }
            let clear = (if seen & STOP_ARM_IMAGE != 0 { irq_fe_ack(LRC_CH) } else { 0 })
                | (if seen & STOP_ARM_EMB != 0 { irq_fe_ack(LRC_EMB_CH) } else { 0 });
            if clear != 0 {
                wr(dma, CSI2_STATUS, clear); // write back to clear
            }
            *f.wedge_seen() |= seen & *f.wedge_armed();
            let stopped = *f.wedge_seen() == *f.wedge_armed();
            if stopped {
                let [s, e] = *ctl.held();
                release_slots(&mut f, s, e);
                *f.wedge_armed() = 0;
                *f.wedge_seen() = 0;
            }
            stopped
        };
        if !stopped {
            let attempt = *ctl.unwedge_attempt();
            *ctl.unwedge_attempt() = attempt.saturating_add(1);
            let ms = LRC_UNWEDGE_FIRST_MS
                .checked_shl(attempt.min(16) + 1)
                .unwrap_or(LRC_UNWEDGE_MAX_MS)
                .min(LRC_UNWEDGE_MAX_MS);
            l.queue_unwedge(ms);
            return;
        }
        let late_us = us_since(*ctl.wedged_ns());
        let bound_us = ctl.last_stop().bound_us;
        *ctl.last_stop() = LrcStop {
            outcome: STOP_LATE,
            us: late_us,
            bound_us,
        };
        *ctl.wedged() &= !WEDGED_CSI2;
        held = core::mem::replace(ctl.held(), [ptr::null_mut(); 2]);
        dev_info!(
            l.device(),
            "DMA stopped late: frame end after FORCE {} us after the stop; buffers released\n",
            late_us
        );
    }
    // Without `ctl`: the stream's control mutex comes before it.
    for st in held {
        if !st.is_null() {
            // SAFETY: the reference lem_stream_quarantine answered, given back once, here.
            unsafe { lem_stream_unquarantine(st) };
        }
    }
}

/// The frame stream is going: its stop() contract. Takes `ctl` under the stream's ctl.
unsafe extern "C" fn lrc_stream_stop(private: *mut c_void, s: *mut Stream) {
    // `private` is the receiver, and the stream holds the reference taken at attach until this returns.
    let l = Lrc(private.cast());
    {
        let mut ctl = l.ctl();
        // Unbinding stops it first; a late call finds nothing to do.
        if l.stream_now() == s {
            stop_locked(&mut ctl);
        }
    }
    // Our reference, taken at attach: the stream gave it up by calling this.
    l.ref_put();
}

/// The embedded stream is going: as lrc_stream_stop, and the receiver stops with it.
unsafe extern "C" fn lrc_emb_stream_stop(private: *mut c_void, s: *mut Stream) {
    // As lrc_stream_stop.
    let l = Lrc(private.cast());
    {
        let mut ctl = l.ctl();
        let emb = l.frame().emb_stream();
        if emb == s {
            stop_locked(&mut ctl);
        }
    }
    l.ref_put();
}

fn check_start(l: Lrc, s: &lb::lemnos_rp1_csi2_start) -> Result {
    if s.lanes < 1 || s.lanes > l.max_lanes() {
        return Err(EINVAL);
    }
    if s.mbps < LRC_MIN_MBPS || s.mbps > LRC_MAX_MBPS {
        return Err(EINVAL);
    }
    if s.vc > LRC_MAX_VC || s.dt < 1 || s.dt > LRC_MAX_DT {
        return Err(EINVAL);
    }
    if s.width == 0 || s.height == 0 || s.width > LRC_MAX_DIMENSION || s.height > LRC_MAX_DIMENSION
    {
        return Err(EINVAL);
    }
    if s.stride == 0 || s.stride & 15 != 0 || s.size == 0 || s.size & 15 != 0 {
        return Err(EINVAL);
    }
    if s.flags & !lb::LEMNOS_RP1_CSI2_UNPACK16 != 0 {
        return Err(EINVAL);
    }
    if s.reserved[0] != 0 || s.reserved[1] != 0 {
        return Err(EINVAL);
    }
    if u64::from(s.stride) * u64::from(s.height) > u64::from(s.size) {
        return Err(EINVAL);
    }
    if s.flags & lb::LEMNOS_RP1_CSI2_UNPACK16 != 0 && u64::from(s.stride) < 2 * u64::from(s.width) {
        return Err(EINVAL);
    }
    Ok(())
}

/// A file from fget, put when dropped.
struct File(*mut bindings::file);

impl File {
    fn get(fd: c_int) -> Result<Self> {
        // SAFETY: fget takes any number and answers null for one that names no file.
        let file = unsafe { bindings::fget(fd as c_uint) };
        if file.is_null() {
            return Err(EBADF);
        }
        Ok(File(file))
    }
}

impl Drop for File {
    fn drop(&mut self) {
        // SAFETY: the reference fget took.
        unsafe { bindings::fput(self.0) };
    }
}

/// Takes a stream made on this node, by its fd, as a producer for frames of `bytes`. The node's
/// reference is taken here, and released by whoever takes the producer off the stream: stop, unbind,
/// or the stream's own close, as for the frame stream. The fd's file is held only while this runs.
fn attach(
    l: Lrc,
    fd: c_int,
    bytes: u32,
    stop: unsafe extern "C" fn(*mut c_void, *mut Stream),
) -> Result<*mut Stream> {
    let producer = Producer {
        stop: Some(stop),
        private: l.0.cast(),
        job_bytes: 0,
        kick: None,
        ioctl: None,
        mmap: None,
        release: None,
    };
    let file = File::get(fd)?;
    // SAFETY: the file is held (`file`), and the device is the receiver's own.
    let stream = from_err_ptr(unsafe { lem_stream_from_file(file.0.cast(), l.dev().cast()) })?;
    l.ref_get();
    // SAFETY: the stream is alive while its file is held; the producer is copied during the call.
    if let Err(e) = to_result(unsafe { lem_stream_attach(stream, &raw const producer, bytes) }) {
        l.ref_put();
        return Err(e);
    }
    Ok(stream)
}

/// LEMNOS_RP1_CSI2_IOC_START. `ctl` held, not gone, and the caller holds the receiver's claim.
fn start(ctl: &mut Ctl, s: &lb::lemnos_rp1_csi2_start) -> Result {
    let l = ctl.l;
    check_start(l, s)?;
    if *ctl.wedged() != 0 {
        return Err(EIO);
    }
    let fe = l.fe();
    // SAFETY: the front end lives as long as its receiver; `ctl` is held, as the call wants.
    if !l.stream_now().is_null() || (!fe.is_null() && unsafe { lem_pisp_fe_streaming(fe.cast()) }) {
        return Err(EBUSY);
    }

    let stream = attach(l, s.stream_fd, s.size, lrc_stream_stop)?;
    // The request is for this START only: it is asked again before the next one.
    let req_fd = *ctl.emb_req_fd();
    let req_bytes = *ctl.emb_req_bytes();
    *ctl.emb_req_fd() = -1;
    let mut emb: *mut Stream = ptr::null_mut();
    if req_fd >= 0 {
        match attach(l, req_fd, req_bytes, lrc_emb_stream_stop) {
            Ok(e) => emb = e,
            Err(e) => {
                // Undo the frame stream's attach: it was only just made.
                // SAFETY: the stream is alive: its producer's reference is given up only by this call.
                if unsafe { lem_stream_detach(stream) } {
                    l.ref_put();
                }
                return Err(e);
            }
        }
    }

    {
        let mut f = l.frame();
        f.set_start(s);
        f.set_streams(stream, emb);
        f.cur().valid = false;
        f.next().valid = false;
        *f.frames() = 0;
        *f.written() = 0;
        *f.dropped() = 0;
        *f.errors() = 0;
        *f.fs_ns() = 0;
        *f.fs_int() = false;
        *f.fe_int() = false;
        *f.fe_line() = 0; // the line count is cleared below
        *f.emb_bytes() = if emb.is_null() { 0 } else { req_bytes };
        f.emb_cur().valid = false;
        f.emb_next().valid = false;
        *f.emb_fe_line() = 0;
    }

    // csi2_start_channel, the caller's vc and dt.
    let dma = l.dma();
    wr(dma, csi2_ch_ctrl(LRC_CH), 0);
    wr(dma, csi2_ch_debug(LRC_CH), 0);
    wr(
        dma,
        CSI2_STATUS,
        irq_ch_mask(LRC_CH) | irq_ch_mask(LRC_EMB_CH),
    );
    let mut ctrl = DMA_EN | IRQ_EN_FS | IRQ_EN_FE_ACK | PACK_LINE;
    // CH_MODE (bits 2:1): CSI2_MODE_REMAP, as cfe_start_channel picks for a 16-bit format.
    if s.flags & lb::LEMNOS_RP1_CSI2_UNPACK16 != 0 {
        ctrl |= 1 << CH_MODE_SHIFT;
    }
    wr(dma, csi2_ch_frame_size(LRC_CH), (s.height << 16) | s.width);
    ctrl |= (s.vc << VC_SHIFT) | (s.dt << DT_SHIFT);
    wr(dma, csi2_ch_ctrl(LRC_CH), ctrl);

    // The embedded channel, as cfe_start_channel starts a meta node: one line of the buffer's size,
    // packed as bytes, auto arm off, the image's virtual channel and data type 0x12. Its ADDR0 stays
    // zero until a buffer is armed, and a channel not asked for stays off.
    wr(dma, csi2_ch_ctrl(LRC_EMB_CH), 0);
    wr(dma, csi2_ch_debug(LRC_EMB_CH), 0);
    wr(dma, csi2_ch_addr0(LRC_EMB_CH), 0);
    if !emb.is_null() {
        let mut ctrl = DMA_EN | IRQ_EN_FS | IRQ_EN_FE_ACK | PACK_LINE | PACK_BYTES;
        ctrl |= (s.vc << VC_SHIFT) | (lb::LEMNOS_RP1_CSI2_EMBEDDED_DT << DT_SHIFT);
        wr(dma, csi2_ch_frame_size(LRC_EMB_CH), (1 << 16) | req_bytes);
        wr(dma, csi2_ch_ctrl(LRC_EMB_CH), ctrl);
    }

    {
        let mut f = l.frame();
        f.set_streaming(true);
        f.arm();
        f.emb_arm();
    }

    // cfe_start_streaming's order: MIPI config, then csi2_open_rx.
    wr(l.cfg(), MIPICFG_CFG, MIPICFG_CFG_SEL_CSI);
    wr(l.cfg(), MIPICFG_INTE, MIPICFG_INT_CSI_DMA);
    wr(dma, CSI2_IRQ_MASK, 0);
    l.dphy_start(s.lanes, s.mbps);
    wr(dma, CSI2_CTRL, EOP_IS_EOL);
    Ok(())
}

/// csi2_start_channel(CSI2_MODE_FE_STREAMING, auto arm) and the latch write, in that order:
/// cfe_start_channel for the channel feeding the front end. The channel's addresses are zero and
/// never come from anywhere else: it hands the stream to the front end, and the front end has every
/// DMA address.
///
/// # Safety
/// `l` is the front end's receiver, alive, with its `ctl` held and nothing streaming.
pub(crate) unsafe extern "C" fn lrc_fe_channel_start(
    l: *mut LrcDev,
    width: u32,
    height: u32,
    vc: u32,
    dt: u32,
) {
    let dma = Lrc(l).dma();
    wr(dma, csi2_ch_ctrl(LRC_CH), 0);
    wr(dma, csi2_ch_debug(LRC_CH), 0);
    wr(dma, CSI2_STATUS, irq_ch_mask(LRC_CH));
    let mut ctrl = DMA_EN | IRQ_EN_FS | IRQ_EN_FE_ACK | PACK_LINE | AUTO_ARM;
    ctrl |= CSI2_MODE_FE_STREAMING << CH_MODE_SHIFT;
    wr(dma, csi2_ch_frame_size(LRC_CH), (height << 16) | width);
    ctrl |= (vc << VC_SHIFT) | (dt << DT_SHIFT);
    wr(dma, csi2_ch_ctrl(LRC_CH), ctrl);
    wr(dma, csi2_ch_length(LRC_CH), 0xffff_ffff >> 4);
    wr(dma, csi2_ch_stride(LRC_CH), 0);
    wr(dma, csi2_ch_addr1(LRC_CH), 0);
    wr(dma, csi2_ch_addr0(LRC_CH), 0);
}

/// cfe_start_streaming's order after the channel and the front end are armed.
///
/// # Safety
/// As lrc_fe_channel_start.
pub(crate) unsafe extern "C" fn lrc_fe_open_rx(l: *mut LrcDev, lanes: u32, mbps: u32) {
    let l = Lrc(l);
    wr(l.cfg(), MIPICFG_CFG, MIPICFG_CFG_SEL_CSI);
    wr(
        l.cfg(),
        MIPICFG_INTE,
        MIPICFG_INT_CSI_DMA | MIPICFG_INT_PISP_FE,
    );
    wr(l.dma(), CSI2_IRQ_MASK, 0);
    l.dphy_start(lanes, mbps);
    wr(l.dma(), CSI2_CTRL, EOP_IS_EOL);
}

/// The channel feeding the front end, stopped.
///
/// # Safety
/// `l` is the front end's receiver, alive, with its `ctl` held.
pub(crate) unsafe extern "C" fn lrc_fe_channel_stop(l: *mut LrcDev) {
    let l = Lrc(l);
    wr(l.dma(), csi2_ch_ctrl(LRC_CH), FORCE);
    wr(l.dma(), csi2_ch_addr0(LRC_CH), 0);
    wr(l.dma(), csi2_ch_addr0(LRC_CH), 0);
    wr(l.dma(), CSI2_IRQ_MASK, 0);
    wr(l.cfg(), MIPICFG_INTE, 0);
}

/// A reference to the receiver, for the front end's stream.
///
/// # Safety
/// `l` is a receiver the caller holds a reference to.
pub(crate) unsafe extern "C" fn lrc_ref_get(l: *mut LrcDev) {
    Lrc(l).ref_get();
}

/// Gives up a reference taken with lrc_ref_get.
///
/// # Safety
/// `l` is a receiver the caller holds a reference to, and gives up here.
pub(crate) unsafe extern "C" fn lrc_ref_put(l: *mut LrcDev) {
    Lrc(l).ref_put();
}

fn read_user<T: FromBytes>(arg: c_ulong) -> Result<T> {
    UserSlice::new(UserPtr::from_addr(arg as usize), size_of::<T>())
        .reader()
        .read::<T>()
}

fn write_user<T: AsBytes>(arg: c_ulong, v: &T) -> Result {
    UserSlice::new(UserPtr::from_addr(arg as usize), size_of::<T>())
        .writer()
        .write(v)
}

/// The embedded stream for the next START: checked now (a stream made on this node, a length the
/// channel can take), used at START. -1 asks for none.
fn embedded(l: Lrc, arg: c_ulong) -> Result {
    let e: lb::lemnos_rp1_csi2_embedded = read_user(arg)?;
    if e.reserved[0] != 0 || e.reserved[1] != 0 {
        return Err(EINVAL);
    }
    if e.stream_fd < 0 {
        if e.bytes != 0 {
            return Err(EINVAL);
        }
    } else {
        if e.bytes == 0 || e.bytes > lb::LEMNOS_RP1_CSI2_EMBEDDED_MAX || e.bytes & 15 != 0 {
            return Err(EINVAL);
        }
        let file = File::get(e.stream_fd)?;
        // SAFETY: the file is held (`file`), and the device is the receiver's own.
        from_err_ptr(unsafe { lem_stream_from_file(file.0.cast(), l.dev().cast()) })?;
    }
    let mut ctl = l.ctl();
    if ctl.gone() {
        return Err(ENODEV);
    }
    if !l.stream_now().is_null() {
        return Err(EBUSY);
    }
    *ctl.emb_req_fd() = e.stream_fd;
    *ctl.emb_req_bytes() = if e.stream_fd < 0 { 0 } else { e.bytes };
    Ok(())
}

fn info(l: Lrc, arg: c_ulong) -> Result {
    let mut info = lb::lemnos_rp1_csi2_info {
        abi: lb::LEMNOS_RP1_CSI2_ABI,
        host_version: l.host_version(),
        max_lanes: l.max_lanes(),
        ..Default::default()
    };
    {
        let mut ctl = l.ctl();
        info.flags = if *ctl.wedged() != 0 {
            lb::LEMNOS_RP1_CSI2_INFO_WEDGED
        } else {
            0
        };
        let mut f = l.frame();
        info.streaming = u32::from(f.streaming());
        info.frames = *f.frames();
        info.written = *f.written();
        info.dropped = *f.dropped();
        info.errors = *f.errors();
    }
    write_user(arg, &info)
}

/// What the receiver's DMA holds and how its last stop ended, `ctl` held. A wedged receiver's pins are
/// its quarantined streams'.
fn csi2_dma(ctl: &mut Ctl) -> lb::lemnos_rp1_csi2_dma_part {
    let l = ctl.l;
    let stop = *ctl.last_stop();
    let (state, (pinned, bytes)) = if *ctl.wedged() & WEDGED_CSI2 != 0 {
        let [s, e] = *ctl.held();
        (DMA_WEDGED, pinned_of(s, e))
    } else {
        let f = l.frame();
        let (s, e) = (f.stream(), f.emb_stream());
        drop(f);
        // Under `ctl`: an attached stream stays attached, and alive, until a stop that takes it.
        if s.is_null() {
            (DMA_IDLE, (0, 0))
        } else {
            (DMA_STREAMING, pinned_of(s, e))
        }
    };
    lb::lemnos_rp1_csi2_dma_part {
        state,
        stop: stop.outcome,
        stop_us: stop.us,
        bound_us: stop.bound_us,
        pinned,
        reserved: 0,
        pinned_bytes: bytes,
    }
}

/// Both parts' DMA: the receiver's and, on a block with one, the front end's.
fn dma_report(l: Lrc) -> lb::lemnos_rp1_csi2_dma {
    let mut ctl = l.ctl();
    let csi2 = csi2_dma(&mut ctl);
    let fe = l.fe();
    let fe = if fe.is_null() {
        lb::lemnos_rp1_csi2_dma_part::default()
    } else {
        // SAFETY: the front end lives as long as its receiver; `ctl` is held, as the call wants.
        unsafe { lem_pisp_fe_dma(fe.cast()) }
    };
    lb::lemnos_rp1_csi2_dma { csi2, fe }
}

fn dma(l: Lrc, arg: c_ulong) -> Result {
    write_user(arg, &dma_report(l))
}

/// The frame-start count since START and the latest start's time, read together under the frame
/// path's lock. `valid` is zero when the receiver isn't streaming: then the count isn't one since
/// START and must not be read as zero. It is zero too while no buffer is armed: a channel with no
/// buffer address makes no frame start (`Frame::arm`), so the count cannot move and a reader must not
/// take its standing still for a silent sensor. Arming is what makes it valid again.
fn frame_starts(l: Lrc, arg: c_ulong) -> Result {
    let mut fs = lb::lemnos_rp1_csi2_frame_starts::default();
    {
        let _ctl = l.ctl();
        let mut f = l.frame();
        let valid = f.streaming() && f.next().valid;
        fs.valid = u32::from(valid);
        if valid {
            fs.frames = *f.frames();
            fs.latest_ns = *f.fs_ns();
        }
    }
    write_user(arg, &fs)
}

/// Runs lem_stream_create without `ctl`: see the locking note above.
fn stream_create(l: Lrc, arg: c_ulong) -> Result {
    let mut c: StreamCreate = read_user(arg)?;
    let ret = {
        let going = l.going();
        if going.gone() {
            ENODEV.to_errno() as c_long
        } else {
            // SAFETY: the device is the receiver's own, bound while `going` is held and not gone; the
            // arguments are this function's copy.
            unsafe { lem_stream_create(l.dev().cast(), &raw mut c.0) }
        }
    };
    if ret != 0 {
        return Err(Error::from_errno(ret as c_int));
    }
    // A failed copy leaves the stream to the file's close: the caller has lost it.
    write_user(arg, &c)
}

/// One open file of the node: `struct lrc_file`, by address, valid from open to release.
#[derive(Clone, Copy)]
struct LrcFile(*mut LrcOpen);

impl LrcFile {
    fn l(self) -> Lrc {
        // SAFETY: the file's data is alive from open to release, and `l` is set once, at open.
        Lrc(unsafe { (*self.0).l })
    }

    /// The parts this file has claimed. Written only by the file's own CLAIM, under `ctl`; the file's
    /// own calls read it, from any thread that shares the file, so every access is atomic.
    fn parts(&self) -> &AtomicU32 {
        // SAFETY: the file's data is alive from open to release; only the field's address is taken.
        let parts = unsafe { &raw mut (*self.0).parts };
        // SAFETY: a u32, aligned as one, that nothing reads or writes except through this view.
        unsafe { AtomicU32::from_ptr(parts) }
    }

    /// Whether this file has claimed one of `part`'s bits.
    fn holds(self, part: u32) -> bool {
        self.parts().load(Ordering::Relaxed) & part != 0
    }
}

/// CLAIM: each named part gets this file as its claimant, or none does (EBUSY).
fn claim(l: Lrc, lf: LrcFile, arg: c_ulong) -> Result {
    let c: lb::lemnos_rp1_csi2_claim = read_user(arg)?;
    if c.parts == 0 || c.parts & !(PART_CSI2 | PART_FE) != 0 || c.reserved != 0 {
        return Err(EINVAL);
    }
    if c.parts & PART_FE != 0 && l.fe().is_null() {
        return Err(EINVAL);
    }

    let mut ctl = l.ctl();
    if ctl.gone() {
        return Err(ENODEV);
    }
    for part in 0..PARTS {
        if c.parts & (1 << part) != 0 && !ctl.owner(part).is_null() {
            return Err(EBUSY);
        }
    }
    for part in 0..PARTS {
        if c.parts & (1 << part) != 0 {
            *ctl.owner(part) = lf.0;
            lf.parts().fetch_or(1 << part, Ordering::Relaxed);
        }
    }
    Ok(())
}

fn ioctl(lf: LrcFile, cmd: c_uint, arg: c_ulong) -> Result {
    let l = lf.l();
    match cmd {
        IOC_INFO => info(l, arg),
        IOC_FRAME_STARTS => frame_starts(l, arg),
        IOC_DMA => dma(l, arg),
        IOC_CLAIM => claim(l, lf, arg),
        IOC_STREAM_CREATE => {
            if !lf.holds(PART_CSI2 | PART_FE) {
                return Err(EPERM);
            }
            stream_create(l, arg)
        }
        IOC_EMBEDDED => {
            if !lf.holds(PART_CSI2) {
                return Err(EPERM);
            }
            embedded(l, arg)
        }
        IOC_START => {
            let s: lb::lemnos_rp1_csi2_start = read_user(arg)?;
            let mut ctl = l.ctl();
            if ctl.gone() {
                Err(ENODEV)
            } else if !lf.holds(PART_CSI2) {
                Err(EPERM)
            } else {
                start(&mut ctl, &s)
            }
        }
        IOC_STOP => {
            let mut ctl = l.ctl();
            if ctl.gone() {
                return Err(ENODEV);
            }
            if !lf.holds(PART_CSI2) {
                return Err(EPERM);
            }
            if !l.stream_now().is_null() {
                stop_locked(&mut ctl);
            }
            Ok(())
        }
        _ => {
            let fe = l.fe();
            if fe.is_null() {
                return Err(ENOTTY);
            }
            // The front end's INFO is observe-only; its driving calls need its claim.
            if cmd != FE_IOC_INFO && !lf.holds(PART_FE) {
                return Err(EPERM);
            }
            // SAFETY: the front end lives as long as its receiver, which this file holds.
            let ret = unsafe { lem_pisp_fe_ioctl(fe.cast(), cmd, arg as *mut c_void) };
            if ret < 0 {
                Err(Error::from_errno(ret as c_int))
            } else {
                Ok(())
            }
        }
    }
}

unsafe extern "C" fn lrc_ioctl(
    file: *mut bindings::file,
    cmd: c_uint,
    arg: c_ulong,
) -> kernel::ffi::c_long {
    // SAFETY: the VFS calls this with a file of the node, whose private data is its lrc_file from
    // open to release.
    let lf = LrcFile(unsafe { (*file).private_data }.cast());
    match ioctl(lf, cmd, arg) {
        Ok(()) => 0,
        Err(e) => e.to_errno() as kernel::ffi::c_long,
    }
}

unsafe extern "C" fn lrc_open(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
    // SAFETY: misc_open leaves the node's miscdevice in the file's private data before this is called.
    let misc = unsafe { (*file).private_data };
    // The miscdevice is the receiver's `misc`; the node is deregistered before the block's reference
    // is given up, so the receiver is alive.
    let l = Lrc(misc.wrapping_byte_sub(offset_of!(LrcDev, misc)).cast());
    let lf = match KBox::new(LrcOpen { l: l.0, parts: 0 }, GFP_KERNEL) {
        Ok(lf) => lf,
        Err(_) => return ENOMEM.to_errno(),
    };
    l.ref_get();
    // SAFETY: the VFS's file, being opened: nothing else reads its private data yet.
    unsafe { (*file).private_data = KBox::into_raw(lf).cast() };
    0
}

unsafe extern "C" fn lrc_release(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
    // SAFETY: the file's private data is its lrc_file, from lrc_open; this is its last use.
    let lf = LrcFile(unsafe { (*file).private_data }.cast());
    let l = lf.l();
    {
        let mut ctl = l.ctl();
        let fe = l.fe();
        if lf.holds(PART_CSI2) {
            if !l.stream_now().is_null() {
                stop_locked(&mut ctl);
            }
            // The embedded stream is asked for per open, as the frame stream is per START.
            *ctl.emb_req_fd() = -1;
            *ctl.emb_req_bytes() = 0;
            // The front end is fed by the receiver: with the receiver's claim gone it stops too.
            if !fe.is_null() {
                // SAFETY: the front end lives as long as its receiver; `ctl` is held.
                unsafe { lem_pisp_fe_stop(fe.cast()) };
            }
        }
        if lf.holds(PART_FE) && !fe.is_null() {
            // SAFETY: as above.
            unsafe { lem_pisp_fe_stop(fe.cast()) };
        }
        for part in 0..PARTS {
            if *ctl.owner(part) == lf.0 {
                *ctl.owner(part) = ptr::null_mut();
            }
        }
    }
    // SAFETY: the allocation lrc_open made with KBox, freed once: no call on the file is left.
    drop(unsafe { KBox::from_raw(lf.0) });
    l.ref_put();
    0
}

/// The node's file operations.
#[repr(transparent)]
struct Fops(bindings::file_operations);

// SAFETY: never written; the kernel only reads it.
unsafe impl Sync for Fops {}

static LRC_FOPS: Fops = Fops(bindings::file_operations {
    #[cfg(MODULE)]
    // `__this_module` is made by the kernel at load and lives until the module is unloaded.
    owner: (&raw const bindings::__this_module).cast_mut(),
    #[cfg(not(MODULE))]
    owner: ptr::null_mut(),
    open: Some(lrc_open),
    release: Some(lrc_release),
    unlocked_ioctl: Some(lrc_ioctl),
    compat_ioctl: bindings::compat_ptr_ioctl,
    llseek: Some(bindings::noop_llseek),
    // SAFETY: every other operation absent (null), as C's designated initialiser leaves them.
    ..unsafe { core::mem::zeroed() }
});

/// The node's sysfs attributes, `dma` (the receiver's channels) and `fe_dma` (the front end on the same
/// block, which has no node of its own): what LEMNOS_RP1_CSI2_IOC_DMA answers, one `key value` a line.
/// Made once at module load (`sysfs_init`) and given to every receiver's node as its groups, so they
/// come and go with the node.
#[repr(C)]
struct Sysfs {
    attrs: [bindings::device_attribute; 2],
    list: [*mut bindings::attribute; 3],
    group: bindings::attribute_group,
    groups: [*const bindings::attribute_group; 2],
}

struct SysfsCell(UnsafeCell<Sysfs>);

// SAFETY: written once by sysfs_init, at module load before any receiver can be bound, and only read
// after (by the driver core, and by the show for its own address).
unsafe impl Sync for SysfsCell {}

// SAFETY: attributes, pointers and a group of zeros: no name, no show, no attribute; sysfs_init fills
// them in before anything reads them.
static LRC_SYSFS: SysfsCell = SysfsCell(UnsafeCell::new(unsafe { core::mem::zeroed() }));

/// Fills in the attributes. Static storage, so lockdep needs no key of their own.
fn sysfs_init() {
    let s = LRC_SYSFS.0.get();
    for (i, name) in [c"dma", c"fe_dma"].into_iter().enumerate() {
        // SAFETY: at module load, before the driver is registered: nothing else reads the statics yet.
        unsafe {
            (*s).attrs[i].attr.name = name.as_ptr().cast();
            (*s).attrs[i].attr.mode = 0o444;
            (*s).attrs[i].__bindgen_anon_1.show = Some(lrc_dma_show);
            (*s).list[i] = &raw mut (*s).attrs[i].attr;
        }
    }
    // SAFETY: as above.
    unsafe {
        (*s).list[2] = ptr::null_mut();
        (*s).group.__bindgen_anon_2.attrs = (&raw mut (*s).list).cast();
        (*s).groups[0] = &raw const (*s).group;
        (*s).groups[1] = ptr::null();
    }
}

/// The groups a receiver's node is made with.
fn sysfs_groups() -> *mut *const bindings::attribute_group {
    // SAFETY: only the field's address is taken.
    unsafe { (&raw mut (*LRC_SYSFS.0.get()).groups).cast() }
}

/// A sysfs page being written: a show's buffer, PAGE_SIZE bytes, cut at its end.
struct Page {
    buf: *mut u8,
    len: usize,
}

impl core::fmt::Write for Page {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let room = (bindings::PAGE_SIZE as usize - 1).saturating_sub(self.len);
        let n = text.len().min(room);
        // SAFETY: the show's buffer is PAGE_SIZE bytes; `len + n` stays below it.
        unsafe { ptr::copy_nonoverlapping(text.as_ptr(), self.buf.add(self.len), n) };
        self.len += n;
        Ok(())
    }
}

fn state_word(state: u32) -> &'static str {
    match state {
        DMA_IDLE => "idle",
        DMA_STREAMING => "streaming",
        DMA_WEDGED => "wedged",
        _ => "unknown",
    }
}

fn stop_word(stop: u32) -> &'static str {
    match stop {
        lb::LEMNOS_RP1_CSI2_STOP_NONE => "none",
        STOP_ACKED => "acked",
        STOP_IDLE => "idle",
        STOP_UNCONFIRMED => "unconfirmed",
        STOP_LATE => "late",
        _ => "unknown",
    }
}

/// `dma` and `fe_dma`: `state`, `pinned` (dma-bufs), `pinned_bytes`, `stop` and its `stop_us` and
/// `bound_us`; `fe_dma` on a block with no front end is `none`.
unsafe extern "C" fn lrc_dma_show(
    dev: *mut bindings::device,
    attr: *mut bindings::device_attribute,
    buf: *mut c_char,
) -> isize {
    use core::fmt::Write;
    // SAFETY: the node's device, whose driver data misc_register set to the miscdevice: the
    // receiver's `misc`. The node is deregistered, which waits out every show, before the block's
    // reference is given up, so the receiver is alive.
    let misc = unsafe { bindings::dev_get_drvdata(dev) };
    let l = Lrc(misc.wrapping_byte_sub(offset_of!(LrcDev, misc)).cast());
    // SAFETY: only the field's address is taken.
    let fe_attr = ptr::eq(attr, unsafe { &raw mut (*LRC_SYSFS.0.get()).attrs[1] });
    let mut page = Page {
        buf: buf.cast(),
        len: 0,
    };
    if fe_attr && l.fe().is_null() {
        let _ = page.write_str("none\n");
        return page.len as isize;
    }
    let report = dma_report(l);
    let part = if fe_attr { &report.fe } else { &report.csi2 };
    let _ = write!(
        page,
        "state {}\npinned {}\npinned_bytes {}\nstop {}\nstop_us {}\nbound_us {}\n",
        state_word(part.state),
        part.pinned,
        part.pinned_bytes,
        stop_word(part.stop),
        part.stop_us,
        part.bound_us
    );
    page.len as isize
}

/// The lanes wired and their polarities, from the device tree's endpoint: the first endpoint of port
/// 0, whatever its far end is.
fn read_dt(d: &mut LrcDev) {
    d.max_lanes = 2;
    // SAFETY: the device is bound; its firmware node, or null with none.
    let node = unsafe { bindings::__dev_fwnode(d.dev) };
    if node.is_null() {
        return;
    }
    let flags = FWNODE_GRAPH_ENDPOINT_NEXT | FWNODE_GRAPH_DEVICE_DISABLED;
    // SAFETY: the device's own node; the endpoint comes back with a reference, or null.
    let ep = unsafe { bindings::fwnode_graph_get_endpoint_by_id(node, 0, 0, flags) };
    if ep.is_null() {
        return;
    }
    // SAFETY: the endpoint is held; a null array asks for the property's count.
    let n = unsafe {
        bindings::fwnode_property_read_u32_array(
            ep,
            c"data-lanes".as_ptr().cast(),
            ptr::null_mut(),
            0,
        )
    };
    if (1..=4).contains(&n) {
        d.max_lanes = n as u32;
    }
    let mut pol = [0u32; 5];
    // SAFETY: as above.
    let n = unsafe {
        bindings::fwnode_property_read_u32_array(
            ep,
            c"lane-polarities".as_ptr().cast(),
            ptr::null_mut(),
            0,
        )
    };
    if (1..=pol.len() as c_int).contains(&n) {
        // SAFETY: the endpoint is held; `pol` has room for the `n` values asked for.
        let ret = unsafe {
            bindings::fwnode_property_read_u32_array(
                ep,
                c"lane-polarities".as_ptr().cast(),
                pol.as_mut_ptr(),
                n as usize,
            )
        };
        if ret == 0 {
            for (lane, p) in pol.iter().take(n as usize).enumerate() {
                d.polarity[lane] = u8::from(*p != 0);
            }
        }
    }
    // SAFETY: the reference fwnode_graph_get_endpoint_by_id gave.
    unsafe { bindings::fwnode_handle_put(ep) };
}

/// The block's register window, refused when it doesn't cover what's used.
fn window(pdev: *mut bindings::platform_device, index: c_uint, need: u64) -> Result<*mut c_void> {
    // SAFETY: the platform device being probed.
    let res = unsafe { bindings::platform_get_resource(pdev, bindings::IORESOURCE_MEM, index) };
    if res.is_null() {
        return Err(ENODEV);
    }
    // SAFETY: the device's own resource, which lives as long as the device.
    if unsafe { bindings::resource_size(res) } < need {
        return Err(ENODEV);
    }
    // SAFETY: the platform device being probed; the mapping is the device's (devm) until it is unbound.
    from_err_ptr(unsafe { bindings::devm_platform_ioremap_resource(pdev, index) })
}

/// What probe does once the receiver is allocated; an error gives up the block's reference.
fn probe(pdev: *mut bindings::platform_device, dev: *mut bindings::device, l: Lrc) -> Result {
    // SAFETY: nothing else can reach the receiver until the handler is requested and the node
    // registered, below; this borrow ends before each call that hands the receiver on.
    let d = unsafe { &mut *l.0 };
    d.dma = window(pdev, 0, LRC_DMA_WINDOW)?;
    d.host = window(pdev, 1, LRC_HOST_WINDOW)?;
    d.cfg = window(pdev, 2, LRC_CFG_WINDOW)?;
    // The front end's own registers, when the block has them (cfe.c, resource 3).
    // SAFETY: the platform device being probed.
    if !unsafe { bindings::platform_get_resource(pdev, bindings::IORESOURCE_MEM, 3) }.is_null() {
        let fe_window = window(pdev, 3, FE_WINDOW as u64)?;
        // SAFETY: the receiver is initialised as far as the front end reads it at creation, and
        // outlives the front end; the window is mapped and covers LEM_PISP_FE_WINDOW.
        let fe = from_err_ptr(unsafe { lem_pisp_fe_create(l.0, fe_window) })?.cast();
        // SAFETY: as `d` above: probe is still the receiver's only user.
        unsafe { (*l.0).fe = fe };
    }
    // SAFETY: as above; the front end keeps the receiver's address and reads nothing through it here.
    let d = unsafe { &mut *l.0 };
    // SAFETY: the platform device being probed.
    d.irq = unsafe { bindings::platform_get_irq(pdev, 0) };
    if d.irq <= 0 {
        return Err(if d.irq == 0 {
            EINVAL
        } else {
            Error::from_errno(d.irq)
        });
    }
    // SAFETY: the device being probed. DMA_BIT_MASK(64).
    to_result(unsafe { bindings::dma_set_mask_and_coherent(dev, u64::MAX) })?;
    // SAFETY: the device being probed; the clock is the device's (devm), enabled until it is unbound.
    if let Err(e) =
        from_err_ptr(unsafe { bindings::devm_clk_get_optional_enabled(dev, ptr::null()) })
    {
        // SAFETY: `dev` is live; the format has no arguments.
        unsafe { bindings::dev_err_probe(dev, e.to_errno(), c"no clock\n".as_ptr().cast()) };
        return Err(e);
    }
    read_dt(d);
    d.host_version = rd(d.host, HOST_VERSION);
    // Quiet until START.
    wr(d.cfg, MIPICFG_INTE, 0);
    wr(d.dma, CSI2_IRQ_MASK, 0);

    // SAFETY: `dev` is live; its name lives as long as it does.
    let dev_name = unsafe { bindings::dev_name(dev) };
    // SAFETY: `name` is 64 bytes; the format takes two strings, the driver's name and the device's,
    // both NUL-terminated.
    unsafe {
        bindings::snprintf(
            d.name.as_mut_ptr(),
            d.name.len(),
            c"%s-%s".as_ptr().cast::<c_char>(),
            LRC_NAME.as_ptr(),
            dev_name,
        )
    };
    d.misc.minor = bindings::MISC_DYNAMIC_MINOR as c_int;
    d.misc.name = d.name.as_ptr();
    d.misc.fops = &raw const LRC_FOPS.0;
    d.misc.parent = dev;
    d.misc.groups = sysfs_groups();
    let (irq, host_version, max_lanes, name) = (d.irq, d.host_version, d.max_lanes, d.misc.name);

    // SAFETY: the handler's data is the receiver, alive until the handler is freed: at unbind, or
    // just below when the node cannot be registered.
    to_result(unsafe {
        bindings::devm_request_threaded_irq(
            dev,
            irq as c_uint,
            Some(lrc_isr),
            None,
            0,
            LRC_NAME.as_ptr().cast(),
            l.0.cast(),
        )
    })?;
    // SAFETY: only the field's address is taken.
    let misc = unsafe { &raw mut (*l.0).misc };
    // SAFETY: the miscdevice is filled in above and lives until it is deregistered at unbind.
    if let Err(e) = to_result(unsafe { bindings::misc_register(misc) }) {
        // The receiver is freed when this probe fails, so its handler goes first, not with devm.
        // SAFETY: the interrupt requested above, with the same data.
        unsafe { bindings::devm_free_irq(dev, irq as c_uint, l.0.cast()) };
        return Err(e);
    }
    // SAFETY: `dev` is live; the format takes an unsigned, an unsigned and a string, as passed.
    unsafe {
        bindings::_dev_info(
            dev,
            c"CSI-2 host 0x%08x, %u lanes, /dev/%s\n".as_ptr().cast(),
            host_version,
            max_lanes,
            name,
        )
    };
    Ok(())
}

unsafe extern "C" fn lrc_probe(pdev: *mut bindings::platform_device) -> c_int {
    // SAFETY: the platform core calls this with a live device; only the field's address is taken.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: `dev` is live.
    if !unsafe { lem_block_owned(dev.cast()) } {
        // SAFETY: `dev` is live; the format has no arguments.
        return unsafe {
            bindings::dev_err_probe(
                dev,
                ENODEV.to_errno(),
                c"not a block Lemnos owns (see owned=)\n".as_ptr().cast(),
            )
        };
    }
    let mut b = match KBox::<LrcDev>::new_uninit(GFP_KERNEL | __GFP_ZERO) {
        // SAFETY: zeroed, and zero is a value of every field: null pointers, zero counts, false, no
        // function, and locks that are initialised in place below before any use.
        Ok(b) => unsafe { b.assume_init() },
        Err(_) => return ENOMEM.to_errno(),
    };
    // SAFETY: `dev` is live; the reference is held until the receiver is freed.
    b.dev = unsafe { bindings::get_device(dev) };
    b.emb.req_fd = -1;
    // SAFETY: kref_init: the block's reference, given up at unbind or when this probe fails.
    unsafe { bindings::refcount_set(&raw mut b.ref_.refcount, 1) };
    // SAFETY: mutex_init, in place, before any use; the name and class are static.
    unsafe {
        bindings::__mutex_init(
            &raw mut b.ctl,
            c"lrc_dev.ctl".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: init_rwsem, in place, before any use; the name and class are static.
    unsafe {
        bindings::__init_rwsem(
            &raw mut b.going,
            c"lrc_dev.going".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: spin_lock_init, in place, before any use; the name and class are static.
    unsafe {
        bindings::__spin_lock_init(
            &raw mut b.lock,
            c"lrc_dev.lock".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: init_completion, in place, before any use.
    unsafe { bindings::init_completion(&raw mut b.stop_done[0]) };
    // SAFETY: as above.
    unsafe { bindings::init_completion(&raw mut b.stop_done[1]) };
    // INIT_DELAYED_WORK(&l->retry, lrc_retry): the work, then its timer.
    // SAFETY: initialised in place, before any queueing; the name and class are static.
    unsafe {
        bindings::init_work_with_key(
            &raw mut b.retry.work,
            Some(lrc_retry),
            false,
            c"lrc_dev.retry".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: as above; the timer is the work's own, so delayed_work_timer_fn is its function.
    unsafe {
        bindings::timer_init_key(
            &raw mut b.retry.timer,
            Some(bindings::delayed_work_timer_fn),
            bindings::TIMER_IRQSAFE,
            c"lrc_dev.retry".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // INIT_DELAYED_WORK(&l->unwedge, lrc_unwedge), as `retry`.
    // SAFETY: as `retry`'s.
    unsafe {
        bindings::init_work_with_key(
            &raw mut b.unwedge.work,
            Some(lrc_unwedge),
            false,
            c"lrc_dev.unwedge".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: as `retry`'s.
    unsafe {
        bindings::timer_init_key(
            &raw mut b.unwedge.timer,
            Some(bindings::delayed_work_timer_fn),
            bindings::TIMER_IRQSAFE,
            c"lrc_dev.unwedge".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    let l = Lrc(KBox::into_raw(b));
    // SAFETY: platform_set_drvdata: `dev` is live, and lrc_remove reads this back.
    unsafe { bindings::dev_set_drvdata(dev, l.0.cast()) };

    match probe(pdev, dev, l) {
        Ok(()) => 0,
        Err(e) => {
            l.ref_put();
            e.to_errno()
        }
    }
}

unsafe extern "C" fn lrc_remove(pdev: *mut bindings::platform_device) {
    // SAFETY: the platform core calls this with the device lrc_probe bound; only the address is taken.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: platform_get_drvdata: set by lrc_probe, which succeeded.
    let l = Lrc(unsafe { bindings::dev_get_drvdata(dev) }.cast());

    // No new opens; a file already open now fails with ENODEV.
    // SAFETY: only the field's address is taken.
    let misc = unsafe { &raw mut (*l.0).misc };
    // SAFETY: registered by lrc_probe.
    unsafe { bindings::misc_deregister(misc) };
    // SAFETY: only the field's address is taken.
    let going = unsafe { &raw mut (*l.0).going };
    // SAFETY: the semaphore is initialised at probe; process context.
    unsafe { bindings::down_write(going) };
    {
        let _ctl = l.ctl();
        // SAFETY: `going` is held for writing and `ctl` is held: every reader holds one of them.
        unsafe { (*l.0).gone = true };
    }
    // SAFETY: taken for writing above, on this task.
    unsafe { bindings::up_write(going) };
    // Gone: a look for the late frame end finds nothing to do, and doesn't come again. What a wedge
    // kept stays kept: its DMA was never seen to stop.
    // SAFETY: the work was initialised at probe; process context, no lock held.
    unsafe { bindings::cancel_delayed_work_sync(l.unwedge()) };

    {
        let mut ctl = l.ctl();
        if !l.stream_now().is_null() {
            stop_locked(&mut ctl);
        }
        let fe = l.fe();
        if !fe.is_null() {
            // SAFETY: the front end lives as long as its receiver; `ctl` is held.
            unsafe { lem_pisp_fe_stop(fe.cast()) };
        }
        wr(l.cfg(), MIPICFG_INTE, 0);
        wr(l.dma(), CSI2_IRQ_MASK, 0);
    }

    // The handler touches the receiver, which a last file close may free below.
    // SAFETY: the interrupt lrc_probe requested, with the same data.
    unsafe { bindings::devm_free_irq(dev, l.irq() as c_uint, l.0.cast()) };
    // A stream holds dma-bufs mapped to this device; they go before it does.
    // SAFETY: `dev` is live until this returns.
    unsafe { lem_streams_block_gone(dev.cast()) };
    l.ref_put();
}

/// `struct platform_driver`, with no of_match_table: bound only through driver_override.
#[repr(transparent)]
struct Driver(UnsafeCell<bindings::platform_driver>);

// SAFETY: written by the driver core at registration and at unregistration, both under its own locks;
// this module only passes its address.
unsafe impl Sync for Driver {}

static LRC_DRIVER: Driver = Driver(UnsafeCell::new(bindings::platform_driver {
    probe: Some(lrc_probe),
    remove: Some(lrc_remove),
    driver: bindings::device_driver {
        name: LRC_NAME.as_ptr().cast(),
        // SAFETY: the rest of a struct device_driver is pointers, integers and flags, zero by default.
        ..unsafe { core::mem::zeroed() }
    },
    // SAFETY: every other field absent (null or zero), as C's designated initialiser leaves them.
    ..unsafe { core::mem::zeroed() }
}));

/// THIS_MODULE, the driver's owner: the kernel's own `__this_module` for a module, none built in.
fn this_module() -> *mut bindings::module {
    #[cfg(MODULE)]
    {
        // `__this_module` is made by the kernel at load and lives until the module is unloaded.
        (&raw const bindings::__this_module).cast_mut()
    }
    #[cfg(not(MODULE))]
    {
        ptr::null_mut()
    }
}

/// platform_driver_register(&lrc_driver): the receiver's platform driver, at module load.
pub(crate) extern "C" fn lem_rp1_csi2_register() -> c_int {
    sysfs_init();
    // SAFETY: the driver is a static; the owner is this module and the name is the module's, the
    // string KBUILD_MODNAME gives.
    unsafe {
        bindings::__platform_driver_register(
            LRC_DRIVER.0.get(),
            this_module(),
            c"lemnos".as_ptr().cast(),
        )
    }
}

/// platform_driver_unregister(&lrc_driver): unbinds every receiver, at module unload.
pub(crate) extern "C" fn lem_rp1_csi2_unregister() {
    // SAFETY: registered by lem_rp1_csi2_register.
    unsafe { bindings::platform_driver_unregister(LRC_DRIVER.0.get()) };
}
