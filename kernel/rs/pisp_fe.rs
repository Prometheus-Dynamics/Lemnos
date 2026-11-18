// SPDX-License-Identifier: GPL-2.0
//! The PiSP front end of the camera front-end block, feeding a frame stream (stage 1l of the port;
//! replaces lemnos_pisp_fe.c).
//!
//! Bound through the receiver (lemnos_rp1_csi2.c): both are one platform device and one interrupt, so
//! this file is reached through the receiver's node and uses the receiver's channel to hand frames to
//! the block's own stream input.
//!
//! Ported from the stock driver, drivers/media/platform/raspberrypi/rp1-cfe/pisp-fe.c:
//!   Copyright (c) 2021-2024 Raspberry Pi Ltd.
//! The register offsets, the job submission order (input and global first, then the other blocks, then
//! QUEUE), start, stop and the interrupt bits. The start sequence relative to the receiver is cfe.c's:
//! channel, front end, first job, then the link. The rest, the validation in particular, is new:
//!   Copyright (c) 2026 the Lemnos authors.
//!
//! What runs on the block is decided by the module, not the caller:
//!  - The configuration is copied once, into kernel memory, from a dma-buf the consumer also has
//!    mapped. Only that copy is checked and only that copy is written to the block. There is no later
//!    read of the consumer's memory.
//!  - The front end's DMA addresses (statistics, output 0, output 1, the unused memory input) all live
//!    in the configuration. None is taken from it: every one must be zero in the copy and is filled in
//!    here, from the frame's stream buffer and its job's ranges, which are the stream's checks and are
//!    resolved to pinned dma-bufs.
//!  - Sizes are the ones the block will really use: the input is fixed by START, an output's size is
//!    derived from its crop and downscale and must agree with its format, and an output's stride times
//!    height must fit the buffer its address came from. Refused, not clamped, on any disagreement.
//!  - Output 0's address is the frame's buffer, from the stream: its slice of the pool, or its own
//!    dma-buf on a stream without a pool.
//!  - Every register of the configuration is written from the checked copy on every job; the caller's
//!    dirty flags are ignored. The hardware cannot end up running a stale value the checks never saw.
//!  - Only the front end's own window is touched here, at constant offsets.
//!
//! Stopping is one path: ABORT, then the channel, then waiting for the block to report idle (bounded).
//! If it never does, nothing it could reach is released: the stream is wedged, its pins kept for good,
//! and START is refused until the block is bound again.
//!
//! Locking: the receiver's `ctl` orders START, STOP, close and unbind (see lemnos_rp1_csi2.c). `state`
//! is the frame path's, nesting the stream's lock. The stream's ctl is taken only before the receiver's
//! (START enables the stream's CPU mapping first) or by the stream's own stop callback, in that order.
//!
//! The receiver stays C in this stage. Its `struct lrc_dev` is read through the bindings made from
//! lemnos_rp1_csi2_priv.h, so its layout is the compiler's; this file touches only `ctl`, `gone`,
//! `wedged`, `stream`, `max_lanes`, `irq` and `dev` of it, as the C did.

use crate::abi::{err_ptr, ret};
use crate::bindings as lb;
use crate::param::param;
use crate::stream::{
    DelayedWork, Handle, JobRange, Producer, Slot, Stream, dev_warn_ratelimited,
    lem_stream_cpu_read_enable, lem_stream_from_file, msecs_to_jiffies, usleep_range,
};
use core::{
    ffi::c_void,
    mem::{offset_of, size_of},
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering},
};
use kernel::{
    alloc::{KBox, KVec},
    bindings,
    device::Device,
    error::{Result, code::*, from_err_ptr, to_result},
    ffi::{c_long, c_uint},
    ioctl::{_IO, _IOR, _IOW, _IOWR},
    new_spinlock_irq,
    prelude::*,
    sync::SpinLockIrq,
    transmute::{AsBytes, FromBytes},
    uaccess::{UserPtr, UserSlice},
};

param!(
    TEST_PISP_FE,
    test_pisp_fe: bool = false,
    param_ops_bool,
    "bool",
    "Test only: keeps a log of the front end's register writes, read by LEMNOS_PISP_FE_IOC_TEST_TRACE, and enables LEMNOS_PISP_FE_IOC_TEST_SCRIBBLE"
);

// Registers (pisp-fe.c).
const FE_VERSION: usize = 0x000;
const FE_CONTROL: usize = 0x004;
const FE_STATUS: usize = 0x008;
const FE_INT_EN: usize = 0x018;
const FE_INT_STATUS: usize = 0x01c;
const FE_CONTROL_QUEUE: u32 = 1 << 0;
const FE_CONTROL_ABORT: u32 = 1 << 1;
const FE_CONTROL_RESET: u32 = 1 << 2;
const FE_CONTROL_LATCH_REGS: u32 = 1 << 3;
const FE_INT_EOF: u32 = 1 << 0;
const FE_INT_SOF: u32 = 1 << 1;
const FE_INT_LINES0: u32 = 1 << 8;
const FE_INT_LINES1: u32 = 1 << 9;
const FE_STATUS_QUEUED: u32 = 1 << 0;
const FE_CONFIG_BASE: usize = 0x0040;

/// The outputs of the block (PISP_FE_NUM_OUTPUTS).
const OUTPUTS: usize = 2;

/// The configuration registers: everything up to the non-register dirty flags.
const FE_CONFIG_BYTES: usize = offset_of!(lb::pisp_fe_config, dirty_flags);
const _: () = assert!(FE_CONFIG_BYTES % 4 == 0);
const _: () = assert!(FE_CONFIG_BASE + FE_CONFIG_BYTES == lb::LEM_PISP_FE_WINDOW as usize);
const _: () = assert!(
    FE_CONFIG_BYTES
        == offset_of!(lb::pisp_fe_config, ch)
            + OUTPUTS * size_of::<lb::pisp_fe_output_branch_config>()
);

// The enables (enum pisp_fe_enable), and the per-output ones (PISP_FE_ENABLE_CROP(i) and the rest).
const EN_INPUT: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_INPUT;
const EN_DECOMPRESS: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_DECOMPRESS;
const EN_DECOMPAND: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_DECOMPAND;
const EN_BLA: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_BLA;
const EN_DPC: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_DPC;
const EN_STATS_CROP: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_STATS_CROP;
const EN_DECIMATE: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_DECIMATE;
const EN_BLC: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_BLC;
const EN_CDAF_STATS: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_CDAF_STATS;
const EN_AWB_STATS: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_AWB_STATS;
const EN_RGBY: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_RGBY;
const EN_LSC: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_LSC;
const EN_AGC_STATS: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_AGC_STATS;
const EN_CROP0: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_CROP0;
const EN_DOWNSCALE0: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_DOWNSCALE0;
const EN_COMPRESS0: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_COMPRESS0;
const EN_OUTPUT0: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_OUTPUT0;
const EN_OUTPUT1: u32 = lb::pisp_fe_enable_PISP_FE_ENABLE_OUTPUT1;

const fn en_crop(i: usize) -> u32 {
    EN_CROP0 << (4 * i)
}
const fn en_downscale(i: usize) -> u32 {
    EN_DOWNSCALE0 << (4 * i)
}
const fn en_compress(i: usize) -> u32 {
    EN_COMPRESS0 << (4 * i)
}
const fn en_output(i: usize) -> u32 {
    EN_OUTPUT0 << (4 * i)
}
const fn en_output_cluster(i: usize) -> u32 {
    (EN_CROP0 | EN_DOWNSCALE0 | EN_COMPRESS0 | EN_OUTPUT0) << (4 * i)
}
const EN_STATS_CLUSTER: u32 = EN_STATS_CROP
    | EN_DECIMATE
    | EN_BLC
    | EN_CDAF_STATS
    | EN_AWB_STATS
    | EN_RGBY
    | EN_LSC
    | EN_AGC_STATS;
const EN_KNOWN: u32 = EN_INPUT
    | EN_DECOMPRESS
    | EN_DECOMPAND
    | EN_BLA
    | EN_DPC
    | EN_STATS_CLUSTER
    | en_output_cluster(0)
    | en_output_cluster(1);
const _: () = assert!(en_output(1) == EN_OUTPUT1);

// The image formats (enum pisp_image_format).
const FMT_BPS_8: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_8;
const FMT_BPS_16: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_16;
const FMT_BPS_MASK: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_MASK;
const FMT_COMPRESSION_MODE_1: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_COMPRESSION_MODE_1;
const FMT_COMPRESSION_MASK: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_COMPRESSION_MASK;

// The output AXI settings the block is run with; anything else is refused.
const AXI_MAXLEN_FLAGS: u8 = 0xaf;
const AXI_CACHE_PROT: u8 = 0x32;
const AXI_QOS: u16 = 0x8410;
const AXI_THRESH: u16 = 0x0140;
const AXI_THROTTLE: u16 = 0x4100;

/// LEM_STREAM_DMA_ALIGN of lemnos_internal.h: a device address a block is programmed with is aligned this much.
const DMA_ALIGN: u64 = 16;

// Waiting for the block to go idle after ABORT: STATUS must read zero this many times in a row, a read
// every 200-400 us, at most STOP_POLLS reads.
const STOP_STILL_READS: u32 = 8;
const STOP_POLLS: u32 = 250;

/// How often a front end with no job queued looks for a free buffer again.
const RETRY_MS: u32 = 1;

/// Test only: entries the write log holds.
const TRACE_ENTRIES: usize = 2048;

const IOC_INFO: u32 = _IOR::<lb::lemnos_pisp_fe_info>(b'L' as u32, 0xc8);
const IOC_START: u32 = _IOW::<lb::lemnos_pisp_fe_start>(b'L' as u32, 0xc9);
const IOC_STOP: u32 = _IO(b'L' as u32, 0xca);
const IOC_TEST_TRACE: u32 = _IOWR::<lb::lemnos_pisp_fe_test_trace>(b'L' as u32, 0xcb);
const IOC_TEST_SCRIBBLE: u32 = _IO(b'L' as u32, 0xcc);

const _: () = assert!(size_of::<lb::lemnos_pisp_fe_info>() == 56);
const _: () = assert!(size_of::<lb::lemnos_pisp_fe_start>() == 60);
const _: () = assert!(size_of::<lb::lemnos_pisp_fe_test_trace>() == 16);
const _: () = assert!(size_of::<lb::lemnos_pisp_fe_trace_entry>() == 8);

// SAFETY: the uAPI structs are integers only, with no padding (the sizes above are the sums of their fields).
unsafe impl AsBytes for lb::lemnos_pisp_fe_info {}
// SAFETY: as above; any bytes are a value.
unsafe impl FromBytes for lb::lemnos_pisp_fe_start {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_pisp_fe_test_trace {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_pisp_fe_test_trace {}

// Only the receiver's address crosses here; its struct names kernel types that have no fields in some
// configurations, which the lint would refuse by value.
#[allow(improper_ctypes)]
unsafe extern "C" {
    // lemnos_rp1_csi2.c (lemnos_rp1_csi2_priv.h): the receiver's reference count, and the link with the
    // channel feeding the front end. The last three are called holding the receiver's ctl.
    fn lrc_ref_get(l: *mut lb::lrc_dev);
    fn lrc_ref_put(l: *mut lb::lrc_dev);
    fn lrc_fe_channel_start(l: *mut lb::lrc_dev, width: u32, height: u32, vc: u32, dt: u32);
    fn lrc_fe_open_rx(l: *mut lb::lrc_dev, lanes: u32, mbps: u32);
    fn lrc_fe_channel_stop(l: *mut lb::lrc_dev);
}

/// A frame the front end holds (`struct lpf_frame`).
#[derive(Clone, Copy, Default)]
struct Frame {
    slot: Slot,
    valid: bool,
}

/// The frame path's state, under `LemPispFe::state`.
#[derive(Default)]
struct FeState {
    streaming: bool,
    /// Being written; queued for the next frame.
    cur: Frame,
    next: Frame,
    fs_int: bool,
    fe_int: bool,
    /// The receiver started a frame the front end has not: a front end with no job queued starts
    /// nothing and raises nothing, so the receiver's frame start is the only sign of a frame it let go by.
    csi_pending: bool,
    fs_ns: u64,
    /// The copy of the job being prepared, kernel memory no one else sees, and the fields a job must match.
    cfg: lb::pisp_fe_config,
    start: lb::lemnos_pisp_fe_start,
    frames: u64,
    written: u64,
    dropped: u64,
    errors: u64,
    stalled: u64,
}

/// The front end (`struct lem_pisp_fe`). Made by the receiver's probe and destroyed when the receiver
/// is freed; the receiver holds it for its own life.
#[pin_data]
pub(crate) struct LemPispFe {
    /// Our receiver; holds us for its life.
    l: *mut lb::lrc_dev,
    /// Our window, checked at the receiver's probe.
    base: *mut c_void,
    hw_version: u32,
    /// Non-null from START until stopped. Written under `state` (and the receiver's ctl); read anywhere.
    stream: AtomicPtr<Stream>,
    /// Test only: the next copy gets overwritten.
    test_scribble: AtomicBool,
    #[pin]
    state: SpinLockIrq<FeState>,
    /// Queues again while nothing is queued and no buffer was free; see `arm`.
    #[pin]
    retry: DelayedWork,
    /// Test only, None unless test_pisp_fe: each entry is its offset, and its value above it.
    trace: Option<KVec<AtomicU64>>,
    trace_count: AtomicU32,
}

// SAFETY: `l` and `base` are the receiver's and its window, valid for the front end's life; everything
// that changes is in atomics or under `state`.
unsafe impl Send for LemPispFe {}
// SAFETY: as Send.
unsafe impl Sync for LemPispFe {}

/// The receiver's ctl, held: `mutex_lock(&l->ctl)` until dropped.
struct Ctl(*mut bindings::mutex);

impl Ctl {
    fn lock(l: *mut lb::lrc_dev) -> Self {
        // SAFETY: the receiver outlives its front end (see LemPispFe); only the field's address is taken.
        let m = unsafe { &raw mut (*l).ctl };
        // SAFETY: the receiver's mutex, initialised at its probe; process context.
        unsafe { bindings::mutex_lock(m) };
        Ctl(m)
    }
}

impl Drop for Ctl {
    fn drop(&mut self) {
        // SAFETY: locked by `lock`, on this task.
        unsafe { bindings::mutex_unlock(self.0) };
    }
}

/// A file from fget, put when dropped.
struct File(*mut bindings::file);

impl Drop for File {
    fn drop(&mut self) {
        // SAFETY: the reference fget took.
        unsafe { bindings::fput(self.0) };
    }
}

/// Whether every byte of a padding field is zero.
macro_rules! zeroed {
    ($field:expr) => {{
        let bytes = $field;
        bytes.iter().all(|b| *b == 0)
    }};
}

/// The only two kinds of line an output writes, as in the stock driver's front end formats: 16 bits a
/// sample, or a compressed mode at 8 bits a sample. Uncompressed 8, 10 and 12 bit aren't offered by the
/// stock driver either.
fn check_format(format: u32) -> Result {
    let mode = format & FMT_COMPRESSION_MASK;
    let bps = format & FMT_BPS_MASK;
    if format & !(FMT_BPS_MASK | FMT_COMPRESSION_MASK) != 0 {
        return Err(EINVAL);
    }
    if if mode != 0 {
        bps != FMT_BPS_8
    } else {
        bps != FMT_BPS_16
    } {
        return Err(EINVAL);
    }
    Ok(())
}

/// An output branch's format against what the block will really make of its input, and the bytes it
/// will write. `cfg` is the kernel's copy.
fn check_output(cfg: &lb::pisp_fe_config, i: usize) -> Result<u64> {
    let ch = &cfg.ch[i];
    let o = ch.output.format;
    let enables = cfg.global.enables;
    let (iw, ih) = (
        u32::from(cfg.input.format.width),
        u32::from(cfg.input.format.height),
    );
    let (mut w, mut h) = (iw, ih);
    let format = o.format;
    let bps = format & FMT_BPS_MASK;
    let mode = format & FMT_COMPRESSION_MASK;
    let compressing = enables & en_compress(i) != 0;

    // A zero-sized crop can lock the block (pisp_fe_validate_output).
    if enables & en_crop(i) != 0 {
        let c = ch.crop;
        let (x, y, cw, chh) = (
            u32::from(c.offset_x),
            u32::from(c.offset_y),
            u32::from(c.width),
            u32::from(c.height),
        );
        if cw < 2 || chh < 2 || x >= (iw & !1) || y >= ih {
            return Err(EINVAL);
        }
        if x + cw > iw || y + chh > ih {
            return Err(EINVAL);
        }
        w = cw;
        h = chh;
    }
    if enables & en_downscale(i) != 0 {
        let d = ch.downscale;
        if d.xin == 0
            || d.yin == 0
            || d.xout == 0
            || d.yout == 0
            || d.xout > d.xin
            || d.yout > d.yin
            || d.flags & !3 != 0
        {
            return Err(EINVAL);
        }
        let dw = ((w >> 1) * u32::from(d.xout) / u32::from(d.xin)) * 2;
        let dh = ((h >> 1) * u32::from(d.yout) / u32::from(d.yin)) * 2;
        if dw < 2 || dh < 2 || u32::from(d.output_width) != dw || u32::from(d.output_height) != dh {
            return Err(EINVAL);
        }
        w = dw;
        h = dh;
    }

    // Every other format bit (planes, sampling, order, shift, 32 bpp, HOG, integral image, wallpaper,
    // three channels) changes what a line is.
    check_format(format)?;
    // The enable changes the unit, so it must agree with the format that sizes it.
    if compressing != (mode != 0) {
        return Err(EINVAL);
    }
    if compressing && u32::from(ch.compress.mode) != mode / FMT_COMPRESSION_MODE_1 {
        return Err(EINVAL);
    }

    // The stride and size the block will use, and it must agree with the format.
    if u32::from(o.width) != w || u32::from(o.height) != h || w < 2 || h < 2 {
        return Err(EINVAL);
    }
    let (stride, stride2) = (o.stride, o.stride2);
    if stride <= 0 || stride & 15 != 0 || stride2 != 0 {
        return Err(EINVAL);
    }
    // Compressed lines are written in blocks of 8 pixels, a byte each.
    let wbytes = if bps == FMT_BPS_16 {
        2 * w
    } else {
        (w + 7) & !7
    };
    if wbytes > stride as u32 {
        return Err(EINVAL);
    }
    u64::from(stride as u32)
        .checked_mul(u64::from(h))
        .ok_or(EOVERFLOW)
}

/// A zero-sized crop can lock the block (pisp_fe_validate_stats); decimating halves what the statistics
/// path sees, so a crop that decimates to nothing is the same crop. The grids the library always sizes
/// at 2 or more are held to it for the same reason. Unused floating regions are legitimately empty. The
/// statistics buffer's size doesn't depend on any of this.
fn check_stats(cfg: &lb::pisp_fe_config) -> Result {
    let enables = cfg.global.enables;
    let min = if enables & EN_DECIMATE != 0 { 4 } else { 2 };
    let (iw, ih) = (
        u32::from(cfg.input.format.width),
        u32::from(cfg.input.format.height),
    );

    if enables & EN_STATS_CROP != 0 {
        let c = cfg.stats_crop;
        let (x, y, cw, chh) = (
            u32::from(c.offset_x),
            u32::from(c.offset_y),
            u32::from(c.width),
            u32::from(c.height),
        );
        if cw < min || chh < min || x >= (iw & !1) || y >= ih || x + cw > iw || y + chh > ih {
            return Err(EINVAL);
        }
    } else if iw < min || ih < min {
        return Err(EINVAL);
    }
    let agc = cfg.agc_stats;
    if enables & EN_AGC_STATS != 0
        && (agc.size_x < 2 || agc.size_y < 2 || agc.row_size_x < 2 || agc.row_size_y < 2)
    {
        return Err(EINVAL);
    }
    let awb = cfg.awb_stats;
    if enables & EN_AWB_STATS != 0 && (awb.size_x < 2 || awb.size_y < 2) {
        return Err(EINVAL);
    }
    let cdaf = cfg.cdaf_stats;
    if enables & EN_CDAF_STATS != 0 && (cdaf.size_x < 2 || cdaf.size_y < 2) {
        return Err(EINVAL);
    }
    Ok(())
}

/// The padding the block's registers have holes for: must be zero. Kernel copy.
fn check_padding(cfg: &lb::pisp_fe_config) -> Result {
    if !zeroed!(cfg.global.pad)
        || !zeroed!(cfg.input.pad)
        || !zeroed!(cfg.input.pad2)
        || cfg.decompand.pad != 0
        || cfg.decompress.pad != 0
        || !zeroed!(cfg.bla.pad)
        || !zeroed!(cfg.blc.pad)
        || cfg.lsc.pad0 != 0
        || cfg.rgby.pad != 0
        || !zeroed!(cfg.agc_stats.pad1)
        || !zeroed!(cfg.awb_stats.pad)
        || cfg.spare1 != 0
    {
        return Err(EINVAL);
    }
    for i in 0..OUTPUTS {
        let ch = &cfg.ch[i];
        if !zeroed!(ch.downscale.pad)
            || !zeroed!(ch.output.pad)
            || ch.pad != 0
            || ch.compress.pad != 0
        {
            return Err(EINVAL);
        }
    }
    Ok(())
}

/// Checks a job's configuration, on the kernel's copy, against what START fixed and the buffers the
/// job's ranges name, and fills in the address fields the caller may not set. Nothing here touches the
/// block.
fn validate(
    s: Handle,
    cfg: &mut lb::pisp_fe_config,
    start: &lb::lemnos_pisp_fe_start,
    slot: &Slot,
) -> Result {
    let job = &slot.job;
    let enables = cfg.global.enables;
    let stats = enables & EN_STATS_CLUSTER != 0;
    let out1 = enables & EN_OUTPUT1 != 0;
    let (mut stats_addr, mut out1_addr) = (0u64, 0u64);
    let want = 1 + u32::from(stats) + u32::from(out1);
    let mut r = 1usize;

    // Enables first: nothing else means anything until they are known good.
    if enables & !EN_KNOWN != 0 {
        return Err(EINVAL);
    }
    if enables & EN_INPUT == 0 || enables & EN_OUTPUT0 == 0 {
        return Err(EINVAL);
    }
    // Changes the input's unit, and this module's input is always 16 bits: nothing sizes it.
    if enables & EN_DECOMPRESS != 0 {
        return Err(EOPNOTSUPP);
    }
    // Disabled outputs: no crop, downscale or compression may be left on.
    for i in 0..OUTPUTS {
        if enables & en_output(i) == 0 && enables & en_output_cluster(i) != 0 {
            return Err(EINVAL);
        }
    }

    // Every address field: only ever filled in below.
    let (sb, ib) = (cfg.stats_buffer, cfg.input_buffer);
    if sb.addr_lo != 0
        || sb.addr_hi != 0
        || ib.addr_lo != 0
        || ib.addr_hi != 0
        || ib.frame_id != 0
        || ib.pad != 0
    {
        return Err(EINVAL);
    }
    for i in 0..OUTPUTS {
        let ob = cfg.output_buffer[i];
        if ob.addr_lo != 0 || ob.addr_hi != 0 || cfg.ch[i].output.ilines != 0 {
            return Err(EINVAL);
        }
    }

    // The job must name exactly what the enables need, in the order lemnos_pisp_fe.h gives.
    if job.count != want {
        return Err(EINVAL);
    }

    // Input: the frame on the link, unchanged, from the stream interface only.
    let input = cfg.input;
    let inf = input.format;
    if input.streaming != 1
        || inf.format != FMT_BPS_16
        || u32::from(inf.width) != start.width
        || u32::from(inf.height) != start.height
        || inf.stride != 0
        || inf.stride2 != 0
        || start.width < 2
        || start.height < 2
    {
        return Err(EINVAL);
    }
    // The memory-input path's settings: this module only ever uses the stream.
    if input.axi.maxlen_flags != 0
        || input.axi.cache_prot != 0
        || input.axi.qos != 0
        || input.holdoff != 0
    {
        return Err(EINVAL);
    }
    let axi = cfg.output_axi;
    if axi.maxlen_flags != AXI_MAXLEN_FLAGS
        || axi.cache_prot != AXI_CACHE_PROT
        || axi.qos != AXI_QOS
        || axi.thresh != AXI_THRESH
        || axi.throttle != AXI_THROTTLE
    {
        return Err(EINVAL);
    }

    // A disabled branch is inert: nothing of it can reach the registers.
    if !out1 {
        cfg.ch[1] = Default::default();
    }
    // Floating regions have no enable of their own, and with no statistics no buffer either.
    if !stats {
        cfg.floating_stats = Default::default();
    }
    check_padding(cfg)?;
    if cfg.global.bayer_order > 3 && cfg.global.bayer_order != 128 {
        return Err(EINVAL);
    }
    // The same for what's disabled inside a branch: inert, so nothing of it is written.
    for i in 0..OUTPUTS {
        if enables & en_crop(i) == 0 {
            cfg.ch[i].crop = Default::default();
        }
        if enables & en_downscale(i) == 0 {
            cfg.ch[i].downscale = Default::default();
        }
        if enables & en_compress(i) == 0 {
            cfg.ch[i].compress = Default::default();
        }
    }

    // Output 0: fixed at START, into the frame's buffer of the stream.
    let bytes = check_output(cfg, 0)?;
    let of = cfg.ch[0].output.format;
    if u32::from(of.width) != start.output_width
        || u32::from(of.height) != start.output_height
        || of.stride as u32 != start.output_stride
        || of.format != start.output_format
        || bytes > u64::from(start.output_size)
        || bytes > u64::from(slot.bytes)
    {
        return Err(EINVAL);
    }

    if stats {
        let rng = &job.range[r];
        r += 1;
        check_stats(cfg)?;
        if rng.len != size_of::<lb::pisp_statistics>() as u64 {
            return Err(EINVAL);
        }
        stats_addr = s.range_addr(rng)?;
        if stats_addr & (DMA_ALIGN - 1) != 0 {
            return Err(EINVAL);
        }
    }
    if out1 {
        let rng = &job.range[r];
        let bytes = check_output(cfg, 1)?;
        if bytes > rng.len {
            return Err(EINVAL);
        }
        out1_addr = s.range_addr(rng)?;
        if out1_addr & (DMA_ALIGN - 1) != 0 {
            return Err(EINVAL);
        }
    }

    // Only now does any address reach the copy: these are the module's own.
    cfg.stats_buffer.addr_lo = stats_addr as u32;
    cfg.stats_buffer.addr_hi = (stats_addr >> 32) as u32;
    cfg.output_buffer[0].addr_lo = slot.addr as u32;
    cfg.output_buffer[0].addr_hi = (slot.addr >> 32) as u32;
    cfg.output_buffer[1].addr_lo = out1_addr as u32;
    cfg.output_buffer[1].addr_hi = (out1_addr >> 32) as u32;
    // ILINES interrupts 3/4 of the way down each output (pisp_fe_submit_job).
    for i in 0..OUTPUTS {
        let height = u32::from(cfg.ch[i].output.format.height);
        cfg.ch[i].output.ilines = core::cmp::max(0x80, (3 * height) >> 2) as u16;
    }
    Ok(())
}

impl LemPispFe {
    // --- registers: the only way this file touches the block ---

    /// Test only: one entry of the write log, while it has room.
    fn trace(&self, offset: u32, value: u32) {
        if let Some(log) = &self.trace {
            let n = self.trace_count.load(Ordering::Relaxed) as usize;
            if n < TRACE_ENTRIES {
                log[n].store(
                    u64::from(offset) | u64::from(value) << 32,
                    Ordering::Relaxed,
                );
                self.trace_count.store(n as u32 + 1, Ordering::Relaxed);
            }
        }
    }

    fn rd(&self, offset: usize) -> u32 {
        // SAFETY: the window is LEM_PISP_FE_WINDOW bytes (the receiver's probe checked its resource),
        // and every offset read is a constant below it.
        unsafe { bindings::readl(self.base.cast::<u8>().add(offset).cast()) }
    }

    fn wr(&self, offset: usize, value: u32) {
        self.trace(offset as u32, value);
        // SAFETY: as rd.
        unsafe { bindings::writel(value, self.base.cast::<u8>().add(offset).cast()) };
    }

    fn wr_relaxed(&self, offset: usize, value: u32) {
        self.trace(offset as u32, value);
        // SAFETY: as rd; the offsets are FE_CONFIG_BASE up to the window's end (asserted above).
        unsafe { bindings::writel_relaxed(value, self.base.cast::<u8>().add(offset).cast()) };
    }

    /// The receiver's device, for the log.
    fn device(&self) -> &Device {
        // SAFETY: the receiver outlives its front end; `dev` is set at its probe and never changes.
        let dev = unsafe { (*self.l).dev };
        // SAFETY: the receiver holds its device until it is freed.
        unsafe { Device::from_raw(dev) }
    }

    /// The stream between START and the stop. Only called where there is one.
    fn handle(&self) -> Handle {
        Handle(self.stream.load(Ordering::Relaxed))
    }

    /// Copies one job's configuration from the consumer's dma-buf and checks it. The caller, on Ok, may
    /// write the copy to the block; on an errno it must cancel the job. The consumer's memory is read
    /// once, here.
    fn prepare(
        &self,
        s: Handle,
        cfg: &mut lb::pisp_fe_config,
        start: &lb::lemnos_pisp_fe_start,
        slot: &Slot,
    ) -> Result {
        let config: &JobRange = &slot.job.range[0];
        if slot.job.count < 1 {
            return Err(EINVAL);
        }
        s.range_read(config, cfg)?;
        if self.test_scribble.swap(false, Ordering::Relaxed) {
            // Test only: the consumer's copy goes bad right after the read.
            let stride = offset_of!(lb::pisp_fe_config, ch)
                + offset_of!(lb::pisp_fe_output_branch_config, output)
                + offset_of!(lb::pisp_fe_output_config, format)
                + offset_of!(lb::pisp_image_format_config, stride);
            s.range_poke(config, stride, 0x7fff_fff0);
        }
        validate(s, cfg, start, slot)
    }

    /// pisp_fe_config_write and pisp_fe_submit_job: every register from the kernel's copy, then QUEUE,
    /// ordered by the non-relaxed write. `cfg` was validated.
    fn program(&self, cfg: &lb::pisp_fe_config) {
        let words = ptr::from_ref(cfg).cast::<u32>();
        for i in 0..FE_CONFIG_BYTES / 4 {
            // SAFETY: word `i` lies inside the configuration (FE_CONFIG_BYTES is below its size); the
            // struct is packed, so the read is unaligned.
            let word = unsafe { words.add(i).read_unaligned() };
            self.wr_relaxed(FE_CONFIG_BASE + 4 * i, word);
        }
        self.wr(FE_CONTROL, FE_CONTROL_QUEUE);
    }

    /// Gives the block the stream's next job when it has none queued. `state` held. As the receiver's:
    /// with no buffer free, this tries again every RETRY_MS until one is or the block stops, since a
    /// block with nothing queued may make no interrupt that would.
    fn arm(&self, st: &mut FeState) {
        if !st.streaming || st.next.valid {
            return;
        }
        // The previous job must have been consumed; this read also orders what follows.
        if self.rd(FE_STATUS) & FE_STATUS_QUEUED != 0 {
            st.stalled += 1;
            return;
        }
        let s = self.handle();
        if let Err(e) = s.next(&mut st.next.slot) {
            if e == ENOBUFS {
                self.retry.queue_highpri(msecs_to_jiffies(RETRY_MS));
            }
            // None free, or the stream's own error.
            return;
        }
        if let Err(e) = self.prepare(s, &mut st.cfg, &st.start, &st.next.slot) {
            s.cancel(&st.next.slot);
            s.fault(e.to_errno());
            return;
        }
        self.program(&st.cfg);
        st.next.valid = true;
    }

    /// As the receiver's: a start before the last frame's end is a frame not called complete.
    fn sof(&self, st: &mut FeState, now: u64) {
        if st.cur.valid {
            self.handle().complete(&st.cur.slot, 0, st.frames, st.fs_ns);
            st.errors += 1;
            st.cur.valid = false;
        }
        st.csi_pending = false;
        st.cur = st.next;
        st.next.valid = false;
        if st.cur.valid {
            st.fs_ns = now;
        } else {
            st.dropped += 1;
        }
        st.frames += 1;
        st.fs_int = true;
        st.fe_int = false;
        self.arm(st);
    }

    fn eof(&self, st: &mut FeState, now: u64) {
        if st.cur.valid {
            st.written += 1;
            // Timestamped at the start, stamped at the end of frame: now is the EOF.
            self.handle().complete_at(
                &st.cur.slot,
                st.cur.slot.bytes,
                st.frames - 1,
                st.fs_ns,
                now,
            );
            st.cur.valid = false;
        }
        st.fe_int = true;
        st.fs_int = false;
        self.arm(st);
    }

    /// The block reports idle after ABORT and holds it. Bounded; sleeps; never on the frame path.
    fn idle(&self) -> bool {
        let mut still = 0;
        for _ in 0..STOP_POLLS {
            usleep_range(200, 400);
            if self.rd(FE_STATUS) == 0 {
                still += 1;
                if still >= STOP_STILL_READS {
                    return true;
                }
            } else {
                still = 0;
            }
        }
        false
    }

    /// pisp_fe_stop, then cfe_stop_channel's channel stop: the other way round from cfe_stop_channel.
    /// The abort lands while the channel still feeds the block. A block the channel's stop cuts off
    /// mid-frame waits for the rest of that frame for good, ABORT or not: STATUS stayed 0x5 (queued and
    /// active) on the board after the stock order. A block still busy once the channel is stopped is
    /// reset, which pisp_fe_start does to every block before it runs. Only when the block reports idle
    /// does anything go back; otherwise every pin stays, for good. The receiver's ctl held.
    fn stop(&self) {
        let l = self.l;
        let s = self.stream.load(Ordering::Relaxed);
        if s.is_null() {
            return;
        }
        let s = Handle(s);
        self.state.lock().streaming = false;
        // Not streaming, so a retry finds nothing to do and doesn't come again.
        self.retry.cancel_sync();

        self.wr(FE_INT_EN, 0);
        self.wr(FE_CONTROL, FE_CONTROL_ABORT);
        // Those writes are posted across a bus: a read lands them first.
        let _ = self.rd(FE_STATUS);
        // pisp_fe_stop waits this long before it expects idle; so does this.
        usleep_range(1000, 2000);
        let mut idle = self.idle();

        self.trace(lb::LEMNOS_PISP_FE_MARK_CHANNEL_STOP, 0);
        // SAFETY: the receiver, with its ctl held and the front end's stream still attached.
        unsafe { lrc_fe_channel_stop(l) };
        // SAFETY: the receiver's interrupt, set at its probe and requested for its life.
        let irq = unsafe { (*l).irq };
        // SAFETY: waits out a running handler; process context, no lock of the handler's held.
        unsafe { bindings::synchronize_irq(irq as c_uint) };
        if !idle {
            let status = self.rd(FE_STATUS);
            self.wr(FE_CONTROL, FE_CONTROL_RESET);
            let _ = self.rd(FE_STATUS);
            idle = self.idle();
            dev_warn_ratelimited!(
                self.device(),
                "PiSP front end busy after abort (STATUS {:#x}): reset, {}\n",
                status,
                if idle { "idle" } else { "still busy" }
            );
        }
        if idle {
            self.wr(FE_INT_STATUS, !0);
            let mut st = self.state.lock();
            if st.cur.valid {
                s.cancel(&st.cur.slot);
            }
            if st.next.valid {
                s.cancel(&st.next.slot);
            }
            st.cur.valid = false;
            st.next.valid = false;
        } else {
            dev_err!(
                self.device(),
                "PiSP front end did not stop; buffers stay pinned until reboot\n"
            );
            // SAFETY: the receiver's flag, written under its ctl, which the caller holds.
            unsafe { (*l).wedged = true };
            s.wedge();
        }

        {
            let _st = self.state.lock();
            self.stream.store(ptr::null_mut(), Ordering::Relaxed);
        }
        if s.detach() {
            // SAFETY: the reference START took for the attached stream.
            unsafe { lrc_ref_put(l) };
        }
    }

    fn check_start(&self, a: &lb::lemnos_pisp_fe_start) -> Result {
        // SAFETY: the receiver's lane count, set at its probe and never changed.
        let max_lanes = unsafe { (*self.l).max_lanes };
        if a.lanes < 1 || a.lanes > max_lanes || a.mbps < 80 || a.mbps > 1500 {
            return Err(EINVAL);
        }
        if a.vc > 3 || a.dt < 1 || a.dt > 63 {
            return Err(EINVAL);
        }
        if a.width < 2 || a.height < 2 || a.width > 0xffff || a.height > 0xffff {
            return Err(EINVAL);
        }
        if a.output_width < 2
            || a.output_height < 2
            || a.output_width > 0xffff
            || a.output_height > 0xffff
        {
            return Err(EINVAL);
        }
        if a.output_stride == 0
            || a.output_stride & 15 != 0
            || a.output_size == 0
            || u64::from(a.output_stride) * u64::from(a.output_height) > u64::from(a.output_size)
        {
            return Err(EINVAL);
        }
        if a.flags != 0 || a.reserved[0] != 0 || a.reserved[1] != 0 {
            return Err(EINVAL);
        }
        check_format(a.output_format)
    }

    /// The first job is taken and checked before anything is written: a bad first configuration, or a
    /// stream with none posted, gets its errno here. Then the order is cfe.c's: channel, front end,
    /// first job, and the link last. The receiver's ctl held.
    fn start_locked(&self, a: &lb::lemnos_pisp_fe_start, stream: *mut Stream) -> Result {
        let l = self.l;
        let producer = Producer {
            stop: Some(lpf_stream_stop),
            private: ptr::from_ref(self).cast_mut().cast(),
            job_bytes: 0,
            kick: None,
            ioctl: None,
            mmap: None,
            release: None,
        };
        // SAFETY: the receiver's flag, read under its ctl.
        if unsafe { (*l).wedged } {
            return Err(EIO);
        }
        // SAFETY: the receiver's own stream, written under its ctl.
        let receiving = unsafe { !(*l).stream.is_null() };
        if receiving || !self.stream.load(Ordering::Relaxed).is_null() {
            return Err(EBUSY);
        }

        let s = Handle(stream);
        // SAFETY: the receiver is live: the caller holds its ctl through one of its open files.
        unsafe { lrc_ref_get(l) };
        if let Err(e) = s.attach(&producer, a.output_size) {
            // SAFETY: the reference taken just above.
            unsafe { lrc_ref_put(l) };
            return Err(e);
        }

        let mut st = self.state.lock();
        st.start = *a;
        self.stream.store(stream, Ordering::Relaxed);
        st.cur.valid = false;
        st.next.valid = false;
        st.frames = 0;
        st.written = 0;
        st.dropped = 0;
        st.errors = 0;
        st.stalled = 0;
        st.fs_int = false;
        st.fe_int = false;
        st.csi_pending = false;
        let mut first = s.next(&mut st.next.slot);
        if first.is_ok() {
            let state = &mut *st;
            first = self.prepare(s, &mut state.cfg, &state.start, &state.next.slot);
            if first.is_err() {
                s.cancel(&state.next.slot);
            }
        }
        if let Err(e) = first {
            self.stream.store(ptr::null_mut(), Ordering::Relaxed);
            drop(st);
            let e = if e == ENOBUFS { EINVAL } else { e };
            s.fault(e.to_errno());
            if s.detach() {
                // SAFETY: the reference taken above for the attached stream.
                unsafe { lrc_ref_put(l) };
            }
            return Err(e);
        }
        st.next.valid = true;
        drop(st);

        // Nothing has touched the block before this line.
        self.trace(lb::LEMNOS_PISP_FE_MARK_CHANNEL_START, 0);
        // SAFETY: the receiver, with its ctl held and nothing streaming on it (checked above).
        unsafe { lrc_fe_channel_start(l, a.width, a.height, a.vc, a.dt) };

        // pisp_fe_start.
        self.wr(FE_CONTROL, FE_CONTROL_RESET);
        self.wr(FE_INT_STATUS, !0);
        self.wr(
            FE_INT_EN,
            FE_INT_EOF | FE_INT_SOF | FE_INT_LINES0 | FE_INT_LINES1,
        );

        // The first job, already checked; no interrupt can run yet.
        {
            let mut st = self.state.lock();
            st.streaming = true;
            self.program(&st.cfg);
        }

        self.trace(lb::LEMNOS_PISP_FE_MARK_OPEN_RX, 0);
        // SAFETY: as lrc_fe_channel_start; the front end is ready for frames.
        unsafe { lrc_fe_open_rx(l, a.lanes, a.mbps) };
        Ok(())
    }

    fn start(&self, a: &lb::lemnos_pisp_fe_start) -> Result {
        let l = self.l;
        self.check_start(a)?;
        // SAFETY: any number may be given: fget returns null for one that names no file.
        let file = unsafe { bindings::fget(a.stream_fd as c_uint) };
        if file.is_null() {
            return Err(EBADF);
        }
        let file = File(file);
        // SAFETY: the receiver's device, set at its probe and held for its life.
        let dev = unsafe { (*l).dev };
        // SAFETY: the file is held by `file` for the call and beyond; the stream is checked against `dev`.
        let stream = from_err_ptr(unsafe { lem_stream_from_file(file.0.cast(), dev.cast()) })?;
        // The stream's ctl: before the receiver's, the order the stream's stop callback already keeps.
        // SAFETY: the stream lives while its file is held.
        to_result(unsafe { lem_stream_cpu_read_enable(stream) })?;
        let _ctl = Ctl::lock(l);
        // SAFETY: the receiver's flag, read under its ctl.
        if unsafe { (*l).gone } {
            return Err(ENODEV);
        }
        self.start_locked(a, stream)
    }

    fn info(&self, arg: *mut c_void) -> Result {
        let mut info = lb::lemnos_pisp_fe_info {
            abi: lb::LEMNOS_PISP_FE_ABI,
            hw_version: self.hw_version,
            ..Default::default()
        };
        {
            let _ctl = Ctl::lock(self.l);
            // SAFETY: the receiver's flag, read under its ctl.
            let wedged = unsafe { (*self.l).wedged };
            info.flags = if wedged {
                lb::LEMNOS_PISP_FE_INFO_WEDGED
            } else {
                0
            };
            let st = self.state.lock();
            info.streaming = u32::from(st.streaming);
            info.frames = st.frames;
            info.written = st.written;
            info.dropped = st.dropped;
            info.errors = st.errors;
            info.stalled = st.stalled;
        }
        UserSlice::new(
            UserPtr::from_addr(arg as usize),
            size_of::<lb::lemnos_pisp_fe_info>(),
        )
        .writer()
        .write(&info)
    }

    /// Test only: copies the write log out, and empties it.
    fn test_trace(&self, arg: *mut c_void) -> Result {
        let Some(log) = &self.trace else {
            return Err(EPERM);
        };
        let mut t: lb::lemnos_pisp_fe_test_trace = UserSlice::new(
            UserPtr::from_addr(arg as usize),
            size_of::<lb::lemnos_pisp_fe_test_trace>(),
        )
        .reader()
        .read()?;
        let mut copy = KVec::<u8>::with_capacity(TRACE_ENTRIES * 8, GFP_KERNEL)?;
        let count = {
            let _st = self.state.lock();
            let count = core::cmp::min(self.trace_count.load(Ordering::Relaxed), t.max);
            for entry in &log[..count as usize] {
                let e = entry.load(Ordering::Relaxed);
                // Within the capacity reserved above, so neither allocates. An entry is its offset, then its value.
                copy.extend_from_slice(&(e as u32).to_ne_bytes(), GFP_ATOMIC)?;
                copy.extend_from_slice(&((e >> 32) as u32).to_ne_bytes(), GFP_ATOMIC)?;
            }
            self.trace_count.store(0, Ordering::Relaxed);
            count
        };
        t.count = count;
        UserSlice::new(UserPtr::from_addr(t.entries as usize), copy.len())
            .writer()
            .write_slice(&copy)?;
        UserSlice::new(
            UserPtr::from_addr(arg as usize),
            size_of::<lb::lemnos_pisp_fe_test_trace>(),
        )
        .writer()
        .write(&t)
    }

    fn ioctl(&self, cmd: u32, arg: *mut c_void) -> Result {
        match cmd {
            IOC_INFO => self.info(arg),
            IOC_START => {
                let a: lb::lemnos_pisp_fe_start = UserSlice::new(
                    UserPtr::from_addr(arg as usize),
                    size_of::<lb::lemnos_pisp_fe_start>(),
                )
                .reader()
                .read()?;
                self.start(&a)
            }
            IOC_STOP => {
                let _ctl = Ctl::lock(self.l);
                // SAFETY: the receiver's flag, read under its ctl.
                if unsafe { (*self.l).gone } {
                    return Err(ENODEV);
                }
                self.stop();
                Ok(())
            }
            IOC_TEST_TRACE => self.test_trace(arg),
            IOC_TEST_SCRIBBLE => {
                if !TEST_PISP_FE.get() {
                    return Err(EPERM);
                }
                self.test_scribble.store(true, Ordering::Relaxed);
                Ok(())
            }
            _ => Err(ENOTTY),
        }
    }
}

/// The retry: looks for a free buffer again.
unsafe extern "C" fn lpf_retry(work: *mut bindings::work_struct) {
    // SAFETY: the work is the `retry` field of a LemPispFe (its delayed work begins with its work), which
    // lives until lem_pisp_fe_destroy has cancelled it.
    let fe = unsafe {
        &*(work
            .cast::<u8>()
            .sub(offset_of!(LemPispFe, retry))
            .cast::<LemPispFe>())
    };
    let mut st = fe.state.lock();
    fe.arm(&mut st);
}

/// The stream is going: its stop() contract. Takes the receiver's ctl under the stream's.
unsafe extern "C" fn lpf_stream_stop(priv_: *mut c_void, s: *mut Stream) {
    // SAFETY: the producer's private data is the front end, which the reference START took on the
    // receiver keeps alive until the put below.
    let fe = unsafe { &*(priv_ as *const LemPispFe) };
    let l = fe.l;
    {
        let _ctl = Ctl::lock(l);
        if fe.stream.load(Ordering::Relaxed) == s {
            fe.stop();
        }
    }
    // SAFETY: the reference the stream's producer held on the receiver, given up once, here.
    unsafe { lrc_ref_put(l) };
}

/// pisp_fe_isr, and the frame-event ordering cfe_isr handles. The status is only tested for known
/// bits. Called from the receiver's handler.
///
/// # Safety
/// `fe` is the receiver's front end, from `lem_pisp_fe_create`, not yet destroyed.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pisp_fe_isr(fe: *const LemPispFe, now: u64) {
    // SAFETY: the caller's contract.
    let fe = unsafe { &*fe };
    fe.wr(FE_CONTROL, FE_CONTROL_LATCH_REGS);
    let ints = fe.rd(FE_INT_STATUS);
    fe.wr(FE_INT_STATUS, ints);
    let mut sof = ints & FE_INT_SOF != 0;
    let eof = ints & FE_INT_EOF != 0;

    let mut st = fe.state.lock();
    if st.streaming && (sof || eof) {
        // FE then FS of the next frame, FS and FE of one, FS with no FE before it.
        if eof {
            if sof && !st.fs_int {
                fe.sof(&mut st, now);
                sof = false;
            }
            fe.eof(&mut st, now);
        }
        if sof {
            fe.sof(&mut st, now);
        }
    }
}

/// The receiver's frame start on the channel feeding the front end, handled before the front end's own
/// interrupt when both are pending, since the receiver starts a frame before the front end does. A
/// frame the receiver started and the front end didn't by the next one had no job queued.
///
/// # Safety
/// As `lem_pisp_fe_isr`.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pisp_fe_csi_fs(fe: *const LemPispFe) {
    // SAFETY: the caller's contract.
    let fe = unsafe { &*fe };
    let mut st = fe.state.lock();
    if st.streaming {
        if st.csi_pending {
            st.dropped += 1;
        }
        st.csi_pending = true;
    }
}

/// Whether the front end has a stream (between START and the stop's end).
///
/// # Safety
/// As `lem_pisp_fe_isr`.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pisp_fe_streaming(fe: *const LemPispFe) -> bool {
    // SAFETY: the caller's contract.
    let fe = unsafe { &*fe };
    !fe.stream.load(Ordering::Relaxed).is_null()
}

/// The stop, for the receiver: its ctl held; nothing streaming afterwards, or the block is wedged.
///
/// # Safety
/// As `lem_pisp_fe_isr`, with the receiver's ctl held.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pisp_fe_stop(fe: *const LemPispFe) {
    // SAFETY: the caller's contract.
    let fe = unsafe { &*fe };
    fe.stop();
}

/// The front end's requests, on the receiver's node.
///
/// # Safety
/// As `lem_pisp_fe_isr`; `arg` is the ioctl's argument, a user address.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pisp_fe_ioctl(
    fe: *const LemPispFe,
    cmd: c_uint,
    arg: *mut c_void,
) -> c_long {
    // SAFETY: the caller's contract.
    let fe = unsafe { &*fe };
    ret(fe.ioctl(cmd, arg).map(|()| 0))
}

fn create(l: *mut lb::lrc_dev, window: *mut c_void) -> Result<*mut LemPispFe> {
    let test = TEST_PISP_FE.get();
    let trace = if test {
        let mut log = KVec::with_capacity(TRACE_ENTRIES, GFP_KERNEL)?;
        for _ in 0..TRACE_ENTRIES {
            log.push(AtomicU64::new(0), GFP_KERNEL)?;
        }
        Some(log)
    } else {
        None
    };
    // SAFETY: the window the receiver mapped and checked: LEM_PISP_FE_WINDOW bytes, FE_VERSION its first word.
    let hw_version = unsafe { bindings::readl(window.cast::<u8>().add(FE_VERSION).cast()) };
    let fe = KBox::pin_init(
        pin_init!(LemPispFe {
            l,
            base: window,
            hw_version,
            stream: AtomicPtr::new(ptr::null_mut()),
            test_scribble: AtomicBool::new(false),
            state <- new_spinlock_irq!(FeState::default()),
            retry <- DelayedWork::new(c"lem_pisp_fe.retry", lpf_retry),
            trace,
            trace_count: AtomicU32::new(0),
        }),
        GFP_KERNEL,
    )?;
    if test {
        dev_warn!(
            fe.device(),
            "test_pisp_fe: register log and scribble enabled\n"
        );
    }
    dev_info!(
        fe.device(),
        "PiSP FE HW v{}.{}\n",
        (hw_version >> 24) & 0xff,
        (hw_version >> 20) & 0x0f
    );
    // SAFETY: the front end stays where it is: the receiver keeps this pointer until lem_pisp_fe_destroy.
    Ok(KBox::into_raw(unsafe { Pin::into_inner_unchecked(fe) }))
}

/// Makes the front end of the receiver `l`, whose window is `window`: the front end, or an ERR_PTR.
///
/// # Safety
/// `l` is the receiver being probed, which outlives the front end; `window` is LEM_PISP_FE_WINDOW bytes
/// of the block's registers, mapped for as long.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pisp_fe_create(
    l: *mut lb::lrc_dev,
    window: *mut c_void,
) -> *mut c_void {
    match create(l, window) {
        Ok(fe) => fe.cast(),
        Err(e) => err_ptr(e),
    }
}

/// Frees the front end: nothing streams on it, and the receiver is being freed.
///
/// # Safety
/// `fe` is from `lem_pisp_fe_create`, and is not used after this.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_pisp_fe_destroy(fe: *mut LemPispFe) {
    // SAFETY: the caller's contract: the pointer `create` leaked, given back once.
    let fe = unsafe { KBox::from_raw(fe) };
    fe.retry.cancel_sync();
    drop(fe);
}
