// SPDX-License-Identifier: GPL-2.0
//! The PiSP back end, memory to memory, as a frame stream of lemnos.ko (stage 1m of the port; replaces
//! lemnos_pisp_be.c). Jobs are posted on the stream's ring (lemnos_pisp_be.h).
//!
//! The register sequence, the interrupt's job accounting and the probe check are ported from the stock
//! driver, drivers/media/platform/raspberrypi/pisp_be/pisp_be.c, by way of the lemnos_pisp_be module on
//! the dev branch:
//!   Copyright (c) 2021-2024 Raspberry Pi Limited.
//! Everything else, the checks and the stream integration in particular:
//!   Copyright (c) 2026 the Lemnos authors.
//!
//! What runs on the block is decided by the kernel's copy of a job. The config slot and the job word
//! are read once from memory the consumer can write, into memory it cannot, and that copy is what is
//! checked and what the block reads: there is no second read of anything the consumer holds. Every
//! address, length and index is proved against what the kernel holds, using the bytes the block will
//! touch, or the job is refused. Nothing is clamped.
//!
//! The addresses the config can name, and how each is proved, are listed at `validate`. All are checked
//! with u64 arithmetic that cannot wrap, because every term is a product of u32 and u16 values no
//! larger than the stream's limits; the stream's job_take does its own overflow check.
//!
//! DMA and buffer lifetime. A job holds its regions from acceptance until the interrupt that finishes
//! it, or until it is dropped having never started. Closing the stream or unbinding the block waits for
//! jobs on the block. A job that does not finish in time ends the stream; if the block cannot be shown
//! idle after that, it is wedged: the job's regions are never released and the stream's memory is never
//! freed, for the life of the system, because the block may still be using it.
//!
//! The stream side is the producer interface of rs/stream_producer.rs: a job is taken with job_take, which
//! resolves its addresses and holds what they name in one step, and ends with complete or cancel.
//!
//! Locking: the stream's ctl (held by the stream across `lpb_stop`), the stream's `collect_lock` (one
//! collector, and `lpb_stop` waits it out: after that the block is not touched from a job collection),
//! the block's `state` (the block's job state, the attached stream's jobs, and the registers while a
//! job is written), and the stream's own lock in the stream code. Never the stream's lock then `state`.
//! The interrupt takes `state`.

use crate::bindings as lb;
use crate::param::param;
use crate::stream::{
    DelayedWork, Handle, Producer, Slot, Stream, WaitQueue, Wq, ktime_now, module_hold,
    msecs_to_jiffies,
};
use core::{
    cell::UnsafeCell,
    ffi::{c_int, c_long, c_void},
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use kernel::{
    alloc::{KBox, KVBox, KVec},
    bindings,
    device::Device,
    error::{Error, Result, code::*, from_err_ptr, to_result},
    ffi::{c_uint, c_ulong},
    ioctl::_IOR,
    mm::virt::VmaNew,
    new_mutex, new_spinlock_irq,
    page::{PAGE_SHIFT, page_align},
    prelude::*,
    sync::{Arc, Mutex, SpinLockIrq},
    transmute::AsBytes,
    uaccess::{UserPtr, UserSlice},
};

// Registers (pisp_be.c).
const BE_VERSION_REG: usize = 0x0;
const BE_CONTROL_REG: usize = 0x4;
const BE_CONTROL_COPY_CONFIG: u32 = 1 << 1;
const BE_CONTROL_QUEUE_JOB: u32 = 1 << 0;
const fn be_control_num_tiles(n: u32) -> u32 {
    n << 16
}
const BE_TILE_ADDR_LO_REG: usize = 0x8;
const BE_TILE_ADDR_HI_REG: usize = 0xc;
const BE_STATUS_REG: usize = 0x10;
const BE_STATUS_QUEUED: u32 = 1 << 0;
const BE_BATCH_STATUS_REG: usize = 0x14;
const BE_INTERRUPT_EN_REG: usize = 0x18;
const BE_INTERRUPT_STATUS_REG: usize = 0x1c;
const BE_AXI_REG: usize = 0x20;
const BE_AXI_VALUE: u32 = 0x3270_3200;
const BE_CONFIG_BASE_REG: usize = 0x40;
const fn be_io_addr_low(n: usize) -> usize {
    BE_CONFIG_BASE_REG + 8 * n
}
const fn be_io_addr_high(n: usize) -> usize {
    be_io_addr_low(n) + 4
}
const BE_GLOBAL_BAYER_ENABLE: usize = 0xb0;
const BE_GLOBAL_RGB_ENABLE: usize = 0xb4;
const BE_VERSION_BLOCK: u32 = 0x0225_2700;
const BE_VERSION_MINOR_BITS: u32 = 0xf;

/// The window every register access stays inside; checked at probe.
const LPB_WINDOW: usize = BE_CONFIG_BASE_REG + size_of::<lb::pisp_be_config>();
const LPB_CONFIG_BYTES: usize = size_of::<lb::pisp_be_tiles_config>();
const LPB_TILES_BYTES: usize = size_of::<[lb::pisp_tile; NUM_TILES]>();
const NUM_TILES: usize = lb::PISP_BACK_END_NUM_TILES as usize;
const NUM_OUTPUTS: usize = lb::PISP_BACK_END_NUM_OUTPUTS as usize;

/// mmap offset of the config slots (pages). The ring is at 0.
const LPB_CONFIG_PGOFF: c_ulong = 0x1000;

// The bytes of a config before the first register the module writes, and the bytes after the last.
// Older uAPI layouts called them pad0 and pad1, or named the addresses and non-register fields that
// lived there: refused non-zero under any name.
const LPB_HEAD_BYTES: usize = offset_of!(lb::pisp_be_config, global);
const LPB_TAIL_OFFSET: usize =
    offset_of!(lb::pisp_be_config, axi) + size_of::<lb::pisp_be_axi_config>();
/// The first word of the config the module writes from the copy, and one past the last.
const LPB_FIRST_WORD: usize =
    (LPB_HEAD_BYTES + offset_of!(lb::pisp_be_global_config, bayer_order)) / 4;
const LPB_END_WORD: usize = size_of::<lb::pisp_be_config>() / 4;

const _: () = assert!(LPB_HEAD_BYTES == 112);
const _: () = assert!(LPB_CONFIG_BYTES == 16720);
const _: () = assert!(size_of::<lb::pisp_be_config>() % 4 == 0);
const _: () = assert!(offset_of!(lb::pisp_be_tiles_config, config) == 0);
const _: () = assert!(BE_GLOBAL_BAYER_ENABLE == BE_CONFIG_BASE_REG + LPB_HEAD_BYTES);
const _: () = assert!(BE_CONFIG_BASE_REG + 4 * LPB_FIRST_WORD == BE_GLOBAL_RGB_ENABLE + 4);

/// The addresses a job names: the block's address slots.
const LPB_ADDRS: usize = lb::LEMNOS_PISP_BE_ADDRS as usize;
const ADDR_TDN_INPUT: usize = lb::LEMNOS_PISP_BE_ADDR_TDN_INPUT as usize;
const ADDR_STITCH_INPUT: usize = lb::LEMNOS_PISP_BE_ADDR_STITCH_INPUT as usize;
const ADDR_TDN_OUTPUT: usize = lb::LEMNOS_PISP_BE_ADDR_TDN_OUTPUT as usize;
const ADDR_STITCH_OUTPUT: usize = lb::LEMNOS_PISP_BE_ADDR_STITCH_OUTPUT as usize;
/// LEMNOS_PISP_BE_ADDR_OUTPUT(n, plane).
const fn addr_output(n: usize, plane: usize) -> usize {
    7 + 3 * n + plane
}
const _: () = assert!(addr_output(NUM_OUTPUTS - 1, 2) + 1 == LPB_ADDRS);
const _: () = assert!(LPB_ADDRS <= crate::stream::JOB_HOLDS);

/// A job this long is the block having stopped (the stock driver's experience: it is).
const LPB_TIMEOUT_MS: u32 = 1000;

// The enables (enum pisp_be_bayer_enable, enum pisp_be_rgb_enable).
const BAYER_INPUT: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_INPUT;
const BAYER_DECOMPRESS: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_DECOMPRESS;
const BAYER_TDN_INPUT: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_TDN_INPUT;
const BAYER_TDN_DECOMPRESS: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_TDN_DECOMPRESS;
const BAYER_TDN: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_TDN;
const BAYER_TDN_COMPRESS: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_TDN_COMPRESS;
const BAYER_TDN_OUTPUT: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_TDN_OUTPUT;
const BAYER_STITCH_INPUT: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_STITCH_INPUT;
const BAYER_STITCH_DECOMPRESS: u32 =
    lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_STITCH_DECOMPRESS;
const BAYER_STITCH: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_STITCH;
const BAYER_STITCH_COMPRESS: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_STITCH_COMPRESS;
const BAYER_STITCH_OUTPUT: u32 = lb::pisp_be_bayer_enable_PISP_BE_BAYER_ENABLE_STITCH_OUTPUT;
const RGB_INPUT: u32 = lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_INPUT;
const RGB_OUTPUT0: u32 = lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_OUTPUT0;
const RGB_HOG: u32 = lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_HOG;
/// PISP_BE_RGB_ENABLE_OUTPUT(i).
const fn rgb_output(i: usize) -> u32 {
    RGB_OUTPUT0 << i
}

const LPB_KNOWN_BAYER: u32 = 0x7f_ffff;
const LPB_KNOWN_RGB: u32 = RGB_INPUT
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_CCM
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_SAT_CONTROL
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_YCBCR
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_FALSE_COLOUR
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_SHARPEN
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_YCBCR_INVERSE
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_GAMMA
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_CSC0
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_CSC1
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_DOWNSCALE0
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_DOWNSCALE1
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_RESAMPLE0
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_RESAMPLE1
    | RGB_OUTPUT0
    | lb::pisp_be_rgb_enable_PISP_BE_RGB_ENABLE_OUTPUT1
    | RGB_HOG;

// The image formats (enum pisp_image_format).
const FMT_BPS_8: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_8;
const FMT_BPS_10: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_10;
const FMT_BPS_12: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_12;
const FMT_BPS_16: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_16;
const FMT_BPS_MASK: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPS_MASK;
const FMT_INTERLEAVED: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_PLANARITY_INTERLEAVED;
const FMT_SEMI_PLANAR: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_PLANARITY_SEMI_PLANAR;
const FMT_PLANAR: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_PLANARITY_PLANAR;
const FMT_PLANARITY_MASK: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_PLANARITY_MASK;
const FMT_SAMPLING_444: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_SAMPLING_444;
const FMT_SAMPLING_422: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_SAMPLING_422;
const FMT_SAMPLING_420: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_SAMPLING_420;
const FMT_SAMPLING_MASK: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_SAMPLING_MASK;
const FMT_ORDER_SWAPPED: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_ORDER_SWAPPED;
const FMT_SHIFT_MASK: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_SHIFT_MASK;
const FMT_BPP_32: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_BPP_32;
const FMT_COMPRESSION_MASK: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_COMPRESSION_MASK;
const FMT_HOG_SIGNED: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_HOG_SIGNED;
const FMT_HOG_UNSIGNED: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_HOG_UNSIGNED;
const FMT_INTEGRAL_IMAGE: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_INTEGRAL_IMAGE;
const FMT_WALLPAPER_ROLL: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_WALLPAPER_ROLL;
const FMT_THREE_CHANNEL: u32 = lb::pisp_image_format_PISP_IMAGE_FORMAT_THREE_CHANNEL;

/// Every format bit that has a meaning (X_VALUE, 0x00200000, has no name in the kernel headers).
const LPB_KNOWN_FORMAT: u32 = FMT_BPS_MASK
    | FMT_PLANARITY_MASK
    | FMT_SAMPLING_MASK
    | FMT_ORDER_SWAPPED
    | FMT_SHIFT_MASK
    | FMT_BPP_32
    | 0x0020_0000
    | FMT_COMPRESSION_MASK
    | FMT_HOG_SIGNED
    | FMT_HOG_UNSIGNED
    | FMT_INTEGRAL_IMAGE
    | FMT_WALLPAPER_ROLL
    | FMT_THREE_CHANNEL;

// The alignments the block needs (pisp_be_config.h).
const INPUT_ALIGN: u32 = lb::PISP_BACK_END_INPUT_ALIGN;
const COMPRESSED_ALIGN: u32 = lb::PISP_BACK_END_COMPRESSED_ALIGN;
const OUTPUT_MIN_ALIGN: u32 = lb::PISP_BACK_END_OUTPUT_MIN_ALIGN;
const TRANSFORM_HFLIP: u8 = lb::pisp_be_transform_PISP_BE_TRANSFORM_HFLIP as u8;

const IOC_STATUS: u32 = _IOR::<lb::lemnos_pisp_be_status>(b'L' as u32, 0xb1);
const _: () = assert!(size_of::<lb::lemnos_pisp_be_status>() == 24);
const _: () = assert!(size_of::<lb::lemnos_pisp_be_job>() == 16 + 8 * LPB_ADDRS);

// SAFETY: the status is integers only, with no padding (the size above is the sum of its fields).
unsafe impl AsBytes for lb::lemnos_pisp_be_status {}

param!(
    FAKE_REGS,
    test_pb_fake_regs: bool = false,
    param_ops_bool,
    "bool",
    "Test only: the block's register window is ordinary memory (the test harness)"
);

type Image = lb::pisp_image_format_config;

/// One job slot of a stream (`struct lpb_job`): the checked copy of a config, the tiles where the block
/// reads them, and what the job holds on the stream.
struct LpbJob {
    /// For the tiles' release.
    dev: *mut bindings::device,
    /// The checked copy; kernel only.
    cfg: KVBox<lb::pisp_be_tiles_config>,
    /// Coherent: what the block reads.
    tiles: *mut lb::pisp_tile,
    tiles_dma: bindings::dma_addr_t,
    addr: [u64; LPB_ADDRS],
    bayer_en: u32,
    rgb_en: u32,
    num_tiles: u32,
    /// What the job holds, from job_take to its end.
    slot: Slot,
}

impl LpbJob {
    /// A job slot with its zeroed config copy and its tiles. Process context.
    fn new(dev: *mut bindings::device) -> Result<KBox<Self>> {
        let mut cfg = KVBox::<lb::pisp_be_tiles_config>::new_uninit(GFP_KERNEL)?;
        // SAFETY: writes zeroes over the whole of the new allocation.
        unsafe { cfg.as_mut_ptr().write_bytes(0, 1) };
        // SAFETY: the config is C structs of integers, for which all zeroes are valid.
        let cfg = unsafe { cfg.assume_init() };
        let mut tiles_dma: bindings::dma_addr_t = 0;
        // SAFETY: the device the stream is made on; the handle is written before the call returns.
        let tiles = unsafe {
            bindings::dma_alloc_attrs(
                dev,
                LPB_TILES_BYTES,
                &raw mut tiles_dma,
                bindings::GFP_KERNEL,
                0,
            )
        };
        if tiles.is_null() {
            return Err(ENOMEM);
        }
        // From here the tiles are the job's: its drop frees them, whether or not the box is made.
        let job = Self {
            dev,
            cfg,
            tiles: tiles.cast(),
            tiles_dma,
            addr: [0; LPB_ADDRS],
            bayer_en: 0,
            rgb_en: 0,
            num_tiles: 0,
            slot: Slot::default(),
        };
        Ok(KBox::new(job, GFP_KERNEL)?)
    }
}

impl Drop for LpbJob {
    /// Process context only, with no lock held: the stream's release, or a creation that failed.
    fn drop(&mut self) {
        // SAFETY: the tiles `new` allocated on this device, freed once; the stream holds the device.
        unsafe {
            bindings::dma_free_attrs(
                self.dev,
                LPB_TILES_BYTES,
                self.tiles.cast(),
                self.tiles_dma,
                0,
            )
        };
    }
}

/// The jobs taken and waiting for the block, oldest first: a ring of as many places as the stream has
/// job slots, so a push never allocates and never fails.
struct Ready {
    places: KVec<Option<KBox<LpbJob>>>,
    head: usize,
    len: usize,
}

impl Ready {
    fn new(njobs: usize) -> Result<Self> {
        let mut places = KVec::with_capacity(njobs, GFP_KERNEL)?;
        for _ in 0..njobs {
            places.push(None, GFP_KERNEL)?;
        }
        Ok(Self {
            places,
            head: 0,
            len: 0,
        })
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// At the tail. There is a place for every job the stream has; one past that is handed back.
    fn push_back(&mut self, job: KBox<LpbJob>) -> Option<KBox<LpbJob>> {
        if self.len >= self.places.len() {
            return Some(job);
        }
        let at = (self.head + self.len) % self.places.len();
        self.places[at] = Some(job);
        self.len += 1;
        None
    }

    fn pop_front(&mut self) -> Option<KBox<LpbJob>> {
        if self.len == 0 {
            return None;
        }
        let job = self.places[self.head].take();
        self.head = (self.head + 1) % self.places.len();
        self.len -= 1;
        job
    }
}

/// What `struct lpb_stream` keeps under the block's lock: where each of the stream's jobs is.
struct StreamJobs {
    free: KVec<KBox<LpbJob>>,
    ready: Ready,
    queued: Option<KBox<LpbJob>>,
    running: Option<KBox<LpbJob>>,
    stopping: bool,
    /// `lpb_release` keeps everything.
    wedged: bool,
    sequence: u64,
}

impl StreamJobs {
    /// A job back on the free list. The list has room for every job of the stream, so the push never
    /// allocates. Called under the block's lock, where a job may not be freed: one that did not fit
    /// (there is none) would be left for good.
    fn put_free(&mut self, job: KBox<LpbJob>) {
        if let Err(e) = self.free.push_within_capacity(job) {
            core::mem::forget(e);
        }
    }

    /// A job at the tail of the ready ones; as `put_free` for one with no place.
    fn put_ready(&mut self, job: KBox<LpbJob>) {
        if let Some(job) = self.ready.push_back(job) {
            core::mem::forget(job);
        }
    }
}

/// The config slots the consumer writes: vmalloc_user memory, freed with the stream.
struct Configs(*mut c_void);

impl Drop for Configs {
    fn drop(&mut self) {
        // SAFETY: the memory vmalloc_user gave, freed once; no mapping of it outlives the stream's file.
        unsafe { lb::vfree(self.0) };
    }
}

/// One stream on the block (`struct lpb_stream`): the producer's private data, freed at release unless
/// the block is wedged.
#[pin_data]
struct LpbStream {
    l: Arc<LemPb>,
    /// The stream the core made for this producer, set before any job can run.
    s: AtomicPtr<Stream>,
    /// The consumer writes these.
    configs: Configs,
    configs_bytes: usize,
    config_slots: u32,
    njobs: u32,
    /// One collector at a time; `true` once no collection may touch the block.
    #[pin]
    collect_lock: Mutex<bool>,
    /// All of it under the block's `state`.
    jobs: UnsafeCell<StreamJobs>,
    #[pin]
    idle: WaitQueue,
    #[pin]
    collect: DelayedWork,
}

// SAFETY: `jobs` is only reached through `jobs()`, under the block's lock; the config slots are plain
// memory the consumer shares; the rest is immutable or has its own lock.
unsafe impl Send for LpbStream {}
// SAFETY: as Send.
unsafe impl Sync for LpbStream {}

impl LpbStream {
    /// The stream's jobs.
    ///
    /// # Safety
    /// The block's `state` is held for as long as the result is used, and no other result of this call
    /// is in use on this stream.
    #[allow(clippy::mut_from_ref)]
    unsafe fn jobs(&self) -> &mut StreamJobs {
        // SAFETY: the caller's contract: the block's lock makes this the only access.
        unsafe { &mut *self.jobs.get() }
    }

    fn handle(&self) -> Handle {
        Handle(self.s.load(Ordering::Relaxed))
    }
}

/// The state under `LemPb::state`.
struct PbState {
    /// The attached stream, or null. Its `jobs` belong to this lock while it is attached.
    attached: *mut LpbStream,
    done: u8,
    started: u8,
    /// A job is queued and not yet seen to start.
    busy: bool,
    /// See the file header.
    wedged: bool,
    module_held: bool,
    finished: u64,
    refused: u64,
}

/// One block (`struct lem_pb`). Made by `lem_pb_probe`, and shared by the probe's reference (given up
/// when the device is unbound) and each stream's.
#[pin_data]
pub(crate) struct LemPb {
    dev: *mut bindings::device,
    regs: *mut c_void,
    wq: Wq,
    hw_version: u32,
    /// One stream at a time.
    #[pin]
    create_lock: Mutex<()>,
    #[pin]
    state: SpinLockIrq<PbState>,
    #[pin]
    watchdog: DelayedWork,
}

// SAFETY: the raw pointers are the device's own, valid for the block's life. The registers are accessed
// under `state`, or are the interrupt's own status registers; the rest is immutable after probe.
unsafe impl Send for LemPb {}
// SAFETY: as Send.
unsafe impl Sync for LemPb {}

fn rd_reg(regs: *mut c_void, off: usize) -> u32 {
    // SAFETY: the register window is mapped for LPB_WINDOW bytes (probe checked the resource's size),
    // and every offset read is below it.
    let reg = unsafe { regs.cast::<u8>().add(off) };
    // SAFETY: a register inside the mapped window.
    unsafe { bindings::readl(reg.cast()) }
}

fn wr_reg(regs: *mut c_void, off: usize, val: u32) {
    // SAFETY: as rd_reg.
    let reg = unsafe { regs.cast::<u8>().add(off) };
    // SAFETY: a register inside the mapped window.
    unsafe { bindings::writel(val, reg.cast()) };
}

// --- the checks ---

/// Bytes a row of `w` pixels of an interleaved plane (or plane 0) takes, rounded up.
fn row_bytes(format: u32, w: u64) -> u64 {
    if format & FMT_BPP_32 != 0 {
        return w * 4;
    }
    let mut b = match format & FMT_BPS_MASK {
        FMT_BPS_16 => w * 2,
        FMT_BPS_12 => (w * 3).div_ceil(2),
        FMT_BPS_10 => w.div_ceil(3) * 4,
        _ => w,
    };
    if format & FMT_THREE_CHANNEL != 0 && format & FMT_PLANARITY_MASK == FMT_INTERLEAVED {
        b *= if format & FMT_SAMPLING_MASK == FMT_SAMPLING_422 {
            2
        } else {
            3
        };
    }
    b
}

/// Bytes of a row of `w` pixels in `plane` of `format`, as much as the block touches.
fn plane_row(format: u32, plane: u32, w: u32) -> u64 {
    let planarity = format & FMT_PLANARITY_MASK;
    let sampling = format & FMT_SAMPLING_MASK;
    let mut pixels = u64::from(w);

    if plane != 0 && planarity == FMT_PLANAR && sampling != FMT_SAMPLING_444 {
        pixels = pixels.div_ceil(2);
    }
    // Semi-planar chroma: a pair per two pixels, so a row is as long as luma.
    if format & FMT_COMPRESSION_MASK != 0 {
        // Fixed-rate blocks of 8 pixels, 1 byte each.
        pixels = (pixels + 7) & !7;
    }
    row_bytes(format, pixels)
}

fn plane_lines(format: u32, plane: u32, h: u32) -> u32 {
    if plane != 0 && format & FMT_SAMPLING_MASK == FMT_SAMPLING_420 {
        return h.div_ceil(2);
    }
    h
}

fn nplanes(format: u32) -> u32 {
    if format & FMT_THREE_CHANNEL == 0 {
        return 1;
    }
    match format & FMT_PLANARITY_MASK {
        FMT_SEMI_PLANAR => 2,
        FMT_PLANAR => 3,
        _ => 1,
    }
}

/// Refuses a format the module can't prove what it touches. `align` is the stride alignment the block
/// needs for this kind of plane.
fn check_format(f: &Image, mut align: u32) -> Result {
    let fmt = f.format;
    let planarity = fmt & FMT_PLANARITY_MASK;
    let sampling = fmt & FMT_SAMPLING_MASK;
    let (width, height, stride, stride2) = (u32::from(f.width), f.height, f.stride, f.stride2);

    if width == 0 || height == 0 {
        return Err(EINVAL);
    }
    if fmt & !LPB_KNOWN_FORMAT != 0 {
        return Err(EINVAL);
    }
    // Nothing the module can size: refused, whatever else the config says.
    if fmt & (FMT_WALLPAPER_ROLL | FMT_HOG_SIGNED | FMT_HOG_UNSIGNED | FMT_INTEGRAL_IMAGE) != 0 {
        return Err(EOPNOTSUPP);
    }
    if planarity == FMT_PLANARITY_MASK || sampling == FMT_SAMPLING_MASK {
        return Err(EINVAL);
    }
    if planarity != FMT_INTERLEAVED && fmt & FMT_THREE_CHANNEL == 0 {
        return Err(EINVAL);
    }
    if planarity == FMT_SEMI_PLANAR && sampling == FMT_SAMPLING_444 {
        return Err(EOPNOTSUPP);
    }
    if planarity == FMT_INTERLEAVED && sampling == FMT_SAMPLING_420 {
        return Err(EOPNOTSUPP);
    }
    if fmt & FMT_COMPRESSION_MASK != 0 {
        if fmt & FMT_THREE_CHANNEL != 0 || fmt & FMT_BPS_MASK != FMT_BPS_8 {
            return Err(EOPNOTSUPP);
        }
        if align < COMPRESSED_ALIGN {
            align = COMPRESSED_ALIGN;
        }
    }

    // Strides: positive (a negative stride reads backwards from the image's end).
    if stride < 0 {
        return Err(EOPNOTSUPP);
    }
    if stride == 0 || stride as u32 % align != 0 {
        return Err(EINVAL);
    }
    if u64::from(stride as u32) < plane_row(fmt, 0, width) {
        return Err(EINVAL);
    }
    if nplanes(fmt) > 1 {
        if stride2 < 0 {
            return Err(EOPNOTSUPP);
        }
        if stride2 == 0 || stride2 as u32 % align != 0 {
            return Err(EINVAL);
        }
        if u64::from(stride2 as u32) < plane_row(fmt, 1, width) {
            return Err(EINVAL);
        }
    } else if stride2 != 0 {
        // A stride for a plane the format doesn't have.
        return Err(EINVAL);
    }
    Ok(())
}

/// What one of the hardware's address slots covers for this job (`struct lpb_plane`).
#[derive(Clone, Copy, Default)]
struct Plane {
    /// False: the slot is unused.
    used: bool,
    /// The format of the image the slot is a plane of, and which plane.
    format: u32,
    plane: u32,
    stride: u32,
    bytes: u64,
}

impl Plane {
    fn of(f: &Image, plane: u32) -> Self {
        let stride = (if plane != 0 { f.stride2 } else { f.stride }) as u32;
        Self {
            used: true,
            format: f.format,
            plane,
            stride,
            bytes: u64::from(stride) * u64::from(plane_lines(f.format, plane, u32::from(f.height))),
        }
    }

    /// The block reads or writes `h` rows of `w` pixels starting `off` bytes into the plane, at the
    /// plane's stride. Those bytes, all of them, must lie inside the plane's extent, which is inside a
    /// held region. Overflow can't happen: every term is below 2^48.
    fn tile_fits(&self, off: u32, w: u32, h: u32) -> Result {
        let lines = plane_lines(self.format, self.plane, h);
        let row = plane_row(self.format, self.plane, w);
        if lines == 0 || row > u64::from(self.stride) {
            return Err(EFAULT);
        }
        let end = u64::from(off) + u64::from(lines - 1) * u64::from(self.stride) + row;
        if end > self.bytes {
            return Err(EFAULT);
        }
        Ok(())
    }
}

/// The enables and the jobs that must follow them, refused rather than sanitised (the stock driver
/// clears bits here to avoid a lockup; a config that contradicts itself is the caller's bug and we say
/// so instead). Uses and returns the enables of the kernel copy: Bayer, then RGB.
fn check_enables(c: &lb::pisp_be_config) -> Result<(u32, u32)> {
    let (bayer, rgb) = (c.global.bayer_enables, c.global.rgb_enables);
    let tdn_reset = c.tdn.reset & 1 != 0;
    let on = |bit: u32| bayer & bit != 0;

    if bayer & !LPB_KNOWN_BAYER != 0 || rgb & !LPB_KNOWN_RGB != 0 {
        return Err(EINVAL);
    }
    // Exactly one input.
    if on(BAYER_INPUT) == (rgb & RGB_INPUT != 0) {
        return Err(EINVAL);
    }
    // HOG's address and size aren't provable.
    if rgb & RGB_HOG != 0 {
        return Err(EOPNOTSUPP);
    }

    if on(BAYER_INPUT) {
        // Bits that move data must come with what they read or write.
        if on(BAYER_TDN_INPUT) && !on(BAYER_TDN) {
            return Err(EINVAL);
        }
        // TDN without its average and not resetting: lockup.
        if on(BAYER_TDN) && !on(BAYER_TDN_INPUT) && !tdn_reset {
            return Err(EINVAL);
        }
        // Reset means there is no average to read.
        if on(BAYER_TDN_INPUT) && tdn_reset {
            return Err(EINVAL);
        }
        if on(BAYER_TDN_DECOMPRESS) && !on(BAYER_TDN_INPUT) {
            return Err(EINVAL);
        }
        if on(BAYER_TDN_COMPRESS) && !on(BAYER_TDN_OUTPUT) {
            return Err(EINVAL);
        }
        // Each needs the other.
        if on(BAYER_STITCH_INPUT) != on(BAYER_STITCH) {
            return Err(EINVAL);
        }
        if on(BAYER_STITCH_DECOMPRESS) && !on(BAYER_STITCH_INPUT) {
            return Err(EINVAL);
        }
        if on(BAYER_STITCH_COMPRESS) && !on(BAYER_STITCH_OUTPUT) {
            return Err(EINVAL);
        }
    } else if bayer != 0 {
        // No Bayer input: the whole Bayer pipe off.
        return Err(EINVAL);
    }
    Ok((bayer, rgb))
}

/// A block that compresses works in units of 8 pixels, so its format has to say so.
fn compression_sized(bayer: u32, bit: u32, f: &Image) -> bool {
    bayer & bit == 0 || f.format & FMT_COMPRESSION_MASK != 0
}

/// Whether every byte of `bytes` bytes at `offset` of the config is zero.
fn config_zero(c: &lb::pisp_be_config, offset: usize, bytes: usize) -> bool {
    let base = ptr::from_ref(c).cast::<u8>();
    // SAFETY: `offset + bytes` is within the config at every call (constants of its own layout).
    let first = unsafe { base.add(offset) };
    // SAFETY: the config is a packed struct of integers with no padding, so each of its bytes is
    // initialised; the range is within it, as above.
    let all = unsafe { core::slice::from_raw_parts(first, bytes) };
    all.iter().all(|b| *b == 0)
}

/// What `validate` proved of a job.
struct Checked {
    nrange: usize,
    bayer: u32,
    rgb: u32,
    num_tiles: u32,
}

/// The addresses. A config (in its kernel copy) and a job's thirteen addresses name, as device
/// addresses or as offsets from them, everything the block will access. Each is in one of these and is
/// checked here, in the kernel's copy, against held memory with sizes computed from the config itself:
///
/// ```text
///   addr[0..2]        input planes              input_format, enabled by the input
///   addr[3]           TDN input                 tdn_input_format, TDN_INPUT
///   addr[4]           stitch input              stitch_input_format, STITCH_INPUT
///   addr[5]           TDN output                tdn_output_format, TDN_OUTPUT
///   addr[6]           stitch output             stitch_output_format, STITCH_OUTPUT
///   addr[7..9]        output 0 planes           output_format[0], OUTPUT0
///   addr[10..12]      output 1 planes           output_format[1], OUTPUT1
///   tile input_addr_offset, input_addr_offset2   input planes (0, then 1 and 2)
///   tile tdn_input_addr_offset                   addr[3]
///   tile tdn_output_addr_offset                  addr[5]
///   tile stitch_input_addr_offset                addr[4]
///   tile stitch_output_addr_offset               addr[6]
///   tile output_addr_offset[j], ..._offset2[j]   addr[7+3j], then 8+3j and 9+3j
///   tile output_hog_addr_offset                  refused: HOG is refused entirely
///   hog (config block)                           refused: rgb_enables HOG, see above
///   output transform                             VFLIP refused: the block walks up
///                                                from the tile's bottom line. HFLIP
///                                                stays: the offset names the left end
///   pad0[0..103], the config's first addresses   must be zero, never written
///   pad0[104..111], the HOG address              must be zero, never written
/// ```
///
/// A slot's address is checked for its whole extent (stride * lines of the format, for every plane the
/// format has and the enables use) against the stream's job_take, and each tile's touched rows against
/// that extent. Slots the enables don't use must carry address 0, and a format of a disabled stage
/// isn't looked at, because the hardware doesn't either.
fn validate(
    t: &lb::pisp_be_tiles_config,
    addr_in: &[u64; LPB_ADDRS],
    addr_out: &mut [u64; LPB_ADDRS],
    range: &mut [lb::lemnos_stream_range; LPB_ADDRS],
) -> Result<Checked> {
    let c = &t.config;
    let mut p = [Plane::default(); LPB_ADDRS];
    let num_tiles = t.num_tiles;

    // Bytes where older layouts kept addresses and fields the hardware may use.
    if !config_zero(c, 0, LPB_HEAD_BYTES)
        || !config_zero(
            c,
            LPB_TAIL_OFFSET,
            size_of::<lb::pisp_be_config>() - LPB_TAIL_OFFSET,
        )
        || !config_zero(
            c,
            offset_of!(lb::pisp_be_config, axi),
            size_of::<lb::pisp_be_axi_config>(),
        )
    {
        return Err(EINVAL);
    }
    if num_tiles == 0 || num_tiles as usize > NUM_TILES {
        return Err(EINVAL);
    }
    let (bayer, rgb) = check_enables(c)?;

    // Which formats describe which address slots. Input first.
    let input = &c.input_format;
    check_format(input, INPUT_ALIGN)?;
    for i in 0..nplanes(input.format) {
        p[i as usize] = Plane::of(input, i);
    }
    if bayer & BAYER_INPUT != 0 {
        let compressed = input.format & FMT_COMPRESSION_MASK != 0;
        if compressed != (bayer & BAYER_DECOMPRESS != 0) {
            return Err(EINVAL);
        }
        // Seen on a board: the job never finishes and only a reboot frees the block.
        if !compressed && input.format & FMT_BPS_MASK == FMT_BPS_8 {
            return Err(EOPNOTSUPP);
        }
    }

    if bayer & BAYER_TDN_INPUT != 0 {
        check_format(&c.tdn_input_format, INPUT_ALIGN)?;
        p[ADDR_TDN_INPUT] = Plane::of(&c.tdn_input_format, 0);
    }
    if bayer & BAYER_STITCH_INPUT != 0 {
        check_format(&c.stitch_input_format, INPUT_ALIGN)?;
        p[ADDR_STITCH_INPUT] = Plane::of(&c.stitch_input_format, 0);
    }
    if bayer & BAYER_TDN_OUTPUT != 0 {
        check_format(&c.tdn_output_format, OUTPUT_MIN_ALIGN)?;
        p[ADDR_TDN_OUTPUT] = Plane::of(&c.tdn_output_format, 0);
    }
    if bayer & BAYER_STITCH_OUTPUT != 0 {
        check_format(&c.stitch_output_format, OUTPUT_MIN_ALIGN)?;
        p[ADDR_STITCH_OUTPUT] = Plane::of(&c.stitch_output_format, 0);
    }
    if !compression_sized(bayer, BAYER_TDN_DECOMPRESS, &c.tdn_input_format)
        || !compression_sized(bayer, BAYER_TDN_COMPRESS, &c.tdn_output_format)
        || !compression_sized(bayer, BAYER_STITCH_DECOMPRESS, &c.stitch_input_format)
        || !compression_sized(bayer, BAYER_STITCH_COMPRESS, &c.stitch_output_format)
    {
        return Err(EINVAL);
    }
    for o in 0..NUM_OUTPUTS {
        if rgb & rgb_output(o) == 0 {
            continue;
        }
        let out = &c.output_format[o];
        if out.transform & !TRANSFORM_HFLIP != 0 {
            return Err(EOPNOTSUPP);
        }
        let f = &out.image;
        check_format(f, OUTPUT_MIN_ALIGN)?;
        for i in 0..nplanes(f.format) {
            p[addr_output(o, i as usize)] = Plane::of(f, i);
        }
    }

    // Every slot: an address only where the job uses one, inside held memory.
    let mut nrange = 0;
    for i in 0..LPB_ADDRS {
        let addr = addr_in[i];
        let align = if i <= ADDR_STITCH_INPUT {
            INPUT_ALIGN
        } else {
            OUTPUT_MIN_ALIGN
        };
        if !p[i].used {
            // Disabled stage or a plane the format doesn't have: can't become live.
            if addr != 0 {
                return Err(EINVAL);
            }
            addr_out[i] = 0;
            continue;
        }
        if addr == 0 || addr & u64::from(align - 1) != 0 {
            return Err(EINVAL);
        }
        // Proved, and held, by the stream's job_take.
        range[nrange].addr = addr;
        range[nrange].len = p[i].bytes;
        nrange += 1;
        addr_out[i] = addr;
    }

    // Every tile, for every address it can use.
    for n in 0..num_tiles as usize {
        let tile = &t.tiles[n];
        let (w, h) = (u32::from(tile.input_width), u32::from(tile.input_height));
        let mut img_w = u32::from(input.width);

        if w == 0 || h == 0 {
            return Err(EINVAL);
        }
        if input.format & FMT_COMPRESSION_MASK != 0 {
            // Tiles are padded to the block grid.
            img_w = (img_w + 7) & !7;
        }
        if u32::from(tile.input_offset_x) + w > img_w
            || u32::from(tile.input_offset_y) + h > u32::from(input.height)
        {
            return Err(EINVAL);
        }
        p[0].tile_fits(tile.input_addr_offset, w, h)?;
        if p[1].used {
            p[1].tile_fits(tile.input_addr_offset2, w, h)?;
        }
        if p[2].used {
            p[2].tile_fits(tile.input_addr_offset2, w, h)?;
        }
        if p[ADDR_TDN_INPUT].used {
            p[ADDR_TDN_INPUT].tile_fits(tile.tdn_input_addr_offset, w, h)?;
        }
        if p[ADDR_STITCH_INPUT].used {
            p[ADDR_STITCH_INPUT].tile_fits(tile.stitch_input_addr_offset, w, h)?;
        }
        if p[ADDR_TDN_OUTPUT].used {
            p[ADDR_TDN_OUTPUT].tile_fits(tile.tdn_output_addr_offset, w, h)?;
        }
        if p[ADDR_STITCH_OUTPUT].used {
            p[ADDR_STITCH_OUTPUT].tile_fits(tile.stitch_output_addr_offset, w, h)?;
        }
        for o in 0..NUM_OUTPUTS {
            if rgb & rgb_output(o) == 0 {
                continue;
            }
            let f = &c.output_format[o].image;
            let (ow, oh) = (
                u32::from(tile.output_width[o]),
                u32::from(tile.output_height[o]),
            );
            if (ow == 0) != (oh == 0) {
                return Err(EINVAL);
            }
            if ow == 0 {
                // This tile writes nothing to this output.
                continue;
            }
            if u32::from(tile.output_offset_x[o]) + ow > u32::from(f.width)
                || u32::from(tile.output_offset_y[o]) + oh > u32::from(f.height)
            {
                return Err(EINVAL);
            }
            p[addr_output(o, 0)].tile_fits(tile.output_addr_offset[o], ow, oh)?;
            for i in 1..3 {
                let op = &p[addr_output(o, i)];
                if op.used {
                    op.tile_fits(tile.output_addr_offset2[o], ow, oh)?;
                }
            }
        }
    }

    Ok(Checked {
        nrange,
        bayer,
        rgb,
        num_tiles,
    })
}

impl LemPb {
    fn rd(&self, off: usize) -> u32 {
        rd_reg(self.regs, off)
    }

    fn wr(&self, off: usize, val: u32) {
        wr_reg(self.regs, off, val);
    }

    fn device(&self) -> &Device {
        // SAFETY: the device is live while the block is.
        unsafe { Device::from_raw(self.dev) }
    }

    fn timeout(&self) {
        self.watchdog
            .modify(self.wq, msecs_to_jiffies(LPB_TIMEOUT_MS));
    }

    /// The block can't be given up safely: from here on nothing it touched goes away. `state` held.
    fn wedge(
        &self,
        st: &mut PbState,
        ps: &LpbStream,
        j: &mut StreamJobs,
        err: Error,
        status: u32,
        batch: u32,
    ) {
        if !st.wedged {
            dev_crit!(
                self.device(),
                "block stopped (status 0x{:08x}, batch 0x{:08x}): its buffers stay held until a reboot\n",
                status,
                batch
            );
        }
        st.wedged = true;
        if !st.module_held {
            st.module_held = true;
            module_hold();
        }
        // Jobs not yet on the block can go; the ones on it can't.
        let s = ps.handle();
        while let Some(job) = j.ready.pop_front() {
            s.cancel(&job.slot);
            j.put_free(job);
        }
        j.wedged = true;
        s.fault(err.to_errno());
        ps.idle.wake_all();
    }

    // --- writing a job to the block ---

    /// Writes a job (pispbe_queue_job). EIO when the block isn't holding what was written.
    fn queue_job(&self, job: &LpbJob) -> Result {
        for (u, addr) in job.addr.iter().enumerate() {
            self.wr(be_io_addr_low(u), *addr as u32);
            self.wr(be_io_addr_high(u), (*addr >> 32) as u32);
        }
        self.wr(BE_GLOBAL_BAYER_ENABLE, job.bayer_en);
        self.wr(BE_GLOBAL_RGB_ENABLE, job.rgb_en);

        // Everything else, from the copy: reserved and non-register bytes were refused non-zero in
        // `validate`, so what is written here is what was proved.
        let words = ptr::from_ref(&job.cfg.config).cast::<u32>();
        for u in LPB_FIRST_WORD..LPB_END_WORD {
            // SAFETY: word `u` lies inside the config (LPB_END_WORD is its size in words).
            let at = unsafe { words.add(u) };
            // SAFETY: a word of the config, as above; the struct is packed, so the read is unaligned.
            let word = unsafe { at.read_unaligned() };
            self.wr(BE_CONFIG_BASE_REG + 4 * u, word);
        }

        // Read back the addresses before letting the block run them (stock does too).
        for (u, addr) in job.addr.iter().enumerate() {
            let got = u64::from(self.rd(be_io_addr_low(u)))
                | u64::from(self.rd(be_io_addr_high(u))) << 32;
            if got != *addr {
                return Err(EIO);
            }
        }

        self.wr(BE_TILE_ADDR_LO_REG, job.tiles_dma as u32);
        self.wr(BE_TILE_ADDR_HI_REG, (job.tiles_dma >> 32) as u32);
        self.wr(
            BE_CONTROL_REG,
            BE_CONTROL_COPY_CONFIG | BE_CONTROL_QUEUE_JOB | be_control_num_tiles(job.num_tiles),
        );
        Ok(())
    }

    /// Starts the next ready job if the block has room. `state` held.
    fn schedule(&self, st: &mut PbState, ps: &LpbStream, j: &mut StreamJobs) {
        if st.busy || st.wedged || j.stopping || j.ready.is_empty() {
            return;
        }
        // The block saying it still holds a queued job is one we didn't count: writing now would
        // replace addresses under it. Wait; the watchdog ends the stream if this doesn't clear.
        if self.rd(BE_STATUS_REG) & BE_STATUS_QUEUED != 0 {
            self.timeout();
            return;
        }
        let Some(job) = j.ready.pop_front() else {
            return;
        };
        if self.queue_job(&job).is_err() {
            // Never reached the block: nothing started, so this can be dropped.
            let s = ps.handle();
            s.cancel(&job.slot);
            j.put_free(job);
            s.fault(EIO.to_errno());
            return;
        }
        j.queued = Some(job);
        st.busy = true;
        self.timeout();
    }

    /// A job the block has finished: its frame, and what it held. `state` held.
    fn finish(&self, st: &mut PbState, ps: &LpbStream, j: &mut StreamJobs, job: KBox<LpbJob>) {
        // The block has no start stamp: the finish is both the frame's time and its end.
        let done = ktime_now() as u64;
        ps.handle()
            .complete_at(&job.slot, job.slot.bytes, j.sequence, done, done);
        j.sequence += 1;
        j.put_free(job);
        st.finished += 1;
    }

    /// The job accounting of pispbe_isr. The hardware's counters are only trusted as far as the jobs we
    /// put on it: a counter that moved more than our jobs can explain is a block we no longer
    /// understand, and it is wedged rather than guessed at, with its jobs still held. A job the counters
    /// can't account for stays in place. `state` held.
    fn process(&self, st: &mut PbState, ps: &LpbStream, j: &mut StreamJobs, batch: u32) {
        let mut done_delta = (batch as u8).wrapping_sub(st.done);
        let started_delta = ((batch >> 8) as u8).wrapping_sub(st.started);
        let running = u8::from(j.running.is_some());
        let queued = u8::from(j.queued.is_some());

        if started_delta > queued
            || u32::from(done_delta) > u32::from(running) + u32::from(started_delta)
            || (started_delta != 0 && j.running.is_some() && done_delta == 0)
        {
            self.wedge(st, ps, j, EIO, self.rd(BE_STATUS_REG), batch);
            return;
        }
        if done_delta != 0 {
            if let Some(job) = j.running.take() {
                self.finish(st, ps, j, job);
                st.done = st.done.wrapping_add(1);
                done_delta -= 1;
            }
        }
        if started_delta != 0 {
            let job = j.queued.take();
            st.started = st.started.wrapping_add(1);
            st.busy = false;
            if done_delta != 0 {
                if let Some(job) = job {
                    self.finish(st, ps, j, job);
                }
                st.done = st.done.wrapping_add(1);
            } else {
                j.running = job;
            }
        }
        if j.stopping && j.queued.is_none() && j.running.is_none() {
            ps.idle.wake_all();
        }
        if !j.stopping && !st.wedged {
            self.schedule(st, ps, j);
            if !j.free.is_empty() {
                ps.collect.queue(self.wq, 0);
            }
        }
        if j.queued.is_some() || j.running.is_some() || !j.ready.is_empty() {
            self.timeout();
        } else {
            self.watchdog.cancel();
        }
    }

    /// A job outstanding too long: the block stopped. `state` held.
    fn watchdog_locked(&self, st: &mut PbState, ps: &LpbStream, j: &mut StreamJobs) {
        let batch = self.rd(BE_BATCH_STATUS_REG);
        let status = self.rd(BE_STATUS_REG);

        if status == 0 && batch as u8 == (batch >> 8) as u8 {
            let finished = st.finished;
            // Nothing on the block: a job was waiting for it to say it had room.
            if j.queued.is_none() && j.running.is_none() {
                self.schedule(st, ps, j);
                return;
            }
            // Counters agree the block is idle and nothing is queued: the jobs are done and the
            // interrupt was lost. Account them. If none finished, the counters and the jobs disagree
            // and the block is wedged; whatever came after them has its own timeout, set by `process`.
            self.process(st, ps, j, batch);
            if st.finished != finished || st.wedged {
                return;
            }
        }
        self.wedge(st, ps, j, ETIMEDOUT, status, batch);
    }
}

/// The block's interrupt.
unsafe extern "C" fn lpb_isr(_irq: c_int, data: *mut c_void) -> bindings::irqreturn_t {
    // SAFETY: the interrupt is freed before the block's reference is given up (probe's order).
    let l = unsafe { &*(data as *const LemPb) };
    let status = l.rd(BE_INTERRUPT_STATUS_REG);
    if status == 0 {
        return bindings::irqreturn::IRQ_NONE;
    }
    l.wr(BE_INTERRUPT_STATUS_REG, status);
    let batch = l.rd(BE_BATCH_STATUS_REG);

    let mut st = l.state.lock();
    let ps = st.attached;
    // A wedged block's jobs are never finished, whatever it says later.
    if !ps.is_null() && !st.wedged {
        // SAFETY: the attached stream lives until it is detached, which `state` orders.
        let ps = unsafe { &*ps };
        // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
        let j = unsafe { ps.jobs() };
        l.process(&mut st, ps, j, batch);
    }
    bindings::irqreturn::IRQ_HANDLED
}

/// The watchdog's work.
unsafe extern "C" fn lpb_watchdog(work: *mut bindings::work_struct) {
    // SAFETY: the work is the watchdog's (its delayed work begins with its work), and the block is the
    // container of the watchdog; the block outlives its work (lem_pb_remove cancels it).
    let l = unsafe { work.cast::<u8>().sub(offset_of!(LemPb, watchdog)) }.cast::<LemPb>();
    // SAFETY: the container found above, which is live while its work runs.
    let l = unsafe { &*l };
    let mut st = l.state.lock();
    let ps = st.attached;
    if ps.is_null() || st.wedged {
        return;
    }
    // SAFETY: the attached stream lives until it is detached, which `state` orders.
    let ps = unsafe { &*ps };
    // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
    let j = unsafe { ps.jobs() };
    if j.queued.is_some() || j.running.is_some() || !j.ready.is_empty() {
        l.watchdog_locked(&mut st, ps, j);
    }
}

// --- jobs from the ring ---

impl LpbStream {
    /// Gives a job taken off the free list back to it.
    fn put_back(&self, job: KBox<LpbJob>, refused: bool) {
        let mut st = self.l.state.lock();
        if refused {
            st.refused += 1;
        }
        // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
        unsafe { self.jobs() }.put_free(job);
    }

    /// The copy, the checks and the take of the job at the front of the ring, into `job`. The copy is
    /// what is checked and what runs.
    fn take(&self, wire: &lb::lemnos_pisp_be_job, job: &mut LpbJob) -> Result {
        let mut range = [lb::lemnos_stream_range::default(); LPB_ADDRS];
        if wire.config >= self.config_slots {
            return Err(EINVAL);
        }
        // SAFETY: slot `config` is below the slots the config memory holds (checked just above).
        let slot = unsafe {
            self.configs
                .0
                .cast::<u8>()
                .add(wire.config as usize * LPB_CONFIG_BYTES)
        };
        // SAFETY: the slot is LPB_CONFIG_BYTES of the stream's own memory, and the copy is as many of
        // the job's own; they can't overlap. The consumer may be writing the slot: whatever bytes are
        // read, the copy is what is checked.
        unsafe {
            ptr::copy_nonoverlapping(slot, (&raw mut *job.cfg).cast::<u8>(), LPB_CONFIG_BYTES)
        };
        let checked = validate(&job.cfg, &wire.addr, &mut job.addr, &mut range)?;
        job.bayer_en = checked.bayer;
        job.rgb_en = checked.rgb;
        job.num_tiles = checked.num_tiles;
        self.handle().job_take(
            wire.buffer,
            wire.identity,
            &range[..checked.nrange],
            &mut job.slot,
        )?;
        // SAFETY: the copy's tiles are LPB_TILES_BYTES, and so is the coherent memory the block reads
        // them from; the job is on no list, so nothing else touches either.
        unsafe {
            ptr::copy_nonoverlapping(
                (&raw const job.cfg.tiles).cast::<u8>(),
                job.tiles.cast::<u8>(),
                LPB_TILES_BYTES,
            )
        };
        Ok(())
    }

    /// Copies, checks, takes and starts what is posted. `collect_lock` held (`stopped` is what it
    /// guards); process context. 0, or the errno that ends the stream.
    fn collect_locked(&self, stopped: bool) -> c_long {
        let l = &*self.l;
        let mut wire = lb::lemnos_pisp_be_job::default();

        if stopped {
            return ENODEV.to_errno() as c_long;
        }
        for _ in 0..=self.njobs {
            let job = {
                let st = l.state.lock();
                // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
                let j = unsafe { self.jobs() };
                if st.wedged || j.stopping {
                    let err = if st.wedged { EIO } else { ENODEV };
                    return err.to_errno() as c_long;
                }
                j.free.pop()
            };
            let Some(mut job) = job else {
                return 0;
            };

            let ret = self.handle().job_peek(&mut wire);
            if ret <= 0 {
                self.put_back(job, false);
                return ret;
            }
            if let Err(e) = self.take(&wire, &mut job) {
                self.put_back(job, true);
                return e.to_errno() as c_long;
            }

            let mut st = l.state.lock();
            // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
            let j = unsafe { self.jobs() };
            j.put_ready(job);
            l.schedule(&mut st, self, j);
        }
        0
    }

    fn kick(&self) -> c_long {
        let stopped = self.collect_lock.lock();
        self.collect_locked(*stopped)
    }
}

/// LEMNOS_STREAM_IOC_KICK. An errno ends the stream.
unsafe extern "C" fn lpb_kick(priv_: *mut c_void) -> c_long {
    // SAFETY: the producer's private data, alive while the stream's file can reach it.
    let ps = unsafe { &*(priv_ as *const LpbStream) };
    ps.kick()
}

/// After a job finishes: pick up what the consumer posted since, without a kick.
unsafe extern "C" fn lpb_collect_work(work: *mut bindings::work_struct) {
    // SAFETY: the work is the collect's (its delayed work begins with its work), and the stream is the
    // container of the collect; lpb_stop cancels it before the stream can be released.
    let ps = unsafe { work.cast::<u8>().sub(offset_of!(LpbStream, collect)) }.cast::<LpbStream>();
    // SAFETY: the container found above, which is live while its work runs.
    let ps = unsafe { &*ps };
    let ret = ps.kick();
    if ret != 0 && ret != ENODEV.to_errno() as c_long {
        ps.handle().fault(ret as c_int);
    }
}

// --- the stream's lifetime ---

/// Stops DMA. Everything on the block finishes, or it's wedged and the stream keeps its pins. The
/// stream's ctl is held and the stream is dead: nothing new can be taken.
unsafe extern "C" fn lpb_stop(priv_: *mut c_void, s: *mut Stream) {
    // SAFETY: the producer's private data, alive until release, which the core calls after stop.
    let ps = unsafe { &*(priv_ as *const LpbStream) };
    let ps_ptr = priv_ as *mut LpbStream;
    let l = &*ps.l;

    {
        let _st = l.state.lock();
        // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
        unsafe { ps.jobs() }.stopping = true;
    }
    ps.collect.cancel_sync();

    // A collection already running finishes first; none starts after.
    {
        let mut stopped = ps.collect_lock.lock();
        *stopped = true;
    }

    {
        let _st = l.state.lock();
        // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
        let j = unsafe { ps.jobs() };
        while let Some(job) = j.ready.pop_front() {
            Handle(s).cancel(&job.slot);
            j.put_free(job);
        }
    }

    // Let the block finish what it holds before anything goes.
    ps.idle
        .wait_until(msecs_to_jiffies(LPB_TIMEOUT_MS * 2), || {
            let st = l.state.lock();
            // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
            let j = unsafe { ps.jobs() };
            (j.queued.is_none() && j.running.is_none()) || st.wedged
        });

    let (busy, wedged) = {
        let mut st = l.state.lock();
        // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
        let j = unsafe { ps.jobs() };
        let busy = j.queued.is_some() || j.running.is_some();
        if busy && !st.wedged {
            let (status, batch) = (l.rd(BE_STATUS_REG), l.rd(BE_BATCH_STATUS_REG));
            l.wedge(&mut st, ps, j, ETIMEDOUT, status, batch);
        }
        // Wedged too: the stream's memory goes with its file, and nothing here may touch it.
        if st.attached == ps_ptr {
            st.attached = ptr::null_mut();
        }
        (busy, st.wedged)
    };

    if wedged || busy {
        dev_crit!(
            l.device(),
            "stream wedged: keeping its buffers until a reboot\n"
        );
        Handle(s).wedge();
    }
}

/// The stream's file is gone: nothing reaches the private data any more.
unsafe extern "C" fn lpb_release(priv_: *mut c_void) {
    let ps_ptr = priv_ as *mut LpbStream;
    let wedged = {
        // SAFETY: the private data made by lem_pb_create, not yet freed.
        let ps = unsafe { &*ps_ptr };
        let _st = ps.l.state.lock();
        // SAFETY: `state` is held, and this is the only use of the stream's jobs under it.
        unsafe { ps.jobs() }.wedged
    };
    // The block may still read the tiles: leave them, and everything else, for good.
    if wedged {
        return;
    }
    // SAFETY: the private data made by lem_pb_create, released once; its jobs, their tiles and the
    // config slots go with it, in process context with no lock held.
    drop(unsafe { KBox::from_raw(ps_ptr) });
}

/// The config slots, mapped by the consumer at LPB_CONFIG_PGOFF.
unsafe extern "C" fn lpb_mmap(priv_: *mut c_void, vma: *mut bindings::vm_area_struct) -> c_int {
    // SAFETY: the producer's private data, alive while the stream's file can reach it.
    let ps = unsafe { &*(priv_ as *const LpbStream) };
    // SAFETY: the stream's mmap passes the new area of its own f_ops->mmap, with the mmap lock held.
    let area = unsafe { VmaNew::from_raw(vma) };
    // SAFETY: as above; the offset is set by the mmap call and not changed under it.
    let pgoff = unsafe { (*vma).vm_pgoff };
    let len = area.end() - area.start();

    if area.flags() & bindings::VM_SHARED as bindings::vm_flags_t == 0
        || pgoff != LPB_CONFIG_PGOFF
        || Some(len) != page_align(ps.configs_bytes)
    {
        return EINVAL.to_errno();
    }
    if area.executable() {
        return EPERM.to_errno();
    }
    if let Err(e) = area.try_clear_mayexec() {
        return e.to_errno();
    }
    // SAFETY: the config slots are vmalloc_user memory of the page-aligned size checked above.
    unsafe { lb::remap_vmalloc_range(vma, ps.configs.0, 0) }
}

/// The stream's ioctl: the block's status.
unsafe extern "C" fn lpb_ioctl(priv_: *mut c_void, cmd: c_uint, arg: *mut c_void) -> c_long {
    // SAFETY: the producer's private data, alive while the stream's file can reach it.
    let ps = unsafe { &*(priv_ as *const LpbStream) };
    let l = &*ps.l;
    if cmd != IOC_STATUS {
        return ENOTTY.to_errno() as c_long;
    }
    let status = {
        let st = l.state.lock();
        lb::lemnos_pisp_be_status {
            hw_version: l.hw_version,
            wedged: u32::from(st.wedged),
            finished: st.finished,
            refused: st.refused,
        }
    };
    match UserSlice::new(
        UserPtr::from_addr(arg as usize),
        size_of::<lb::lemnos_pisp_be_status>(),
    )
    .writer()
    .write(&status)
    {
        Ok(()) => 0,
        Err(e) => e.to_errno() as c_long,
    }
}

/// LEMNOS_IOC_PISP_BE_VERSION: the version the probe read and accepted. The block is bound only when it
/// is a known version, so this is the version of the bound block, read once and never changed.
///
/// # Safety
/// `l` is a block from `lem_pb_probe` that has not been removed; `args` is a
/// `struct lemnos_pisp_be_version` the caller owns.
pub(crate) unsafe extern "C" fn lem_pb_version(l: *const LemPb, args: *mut c_void) -> c_long {
    // SAFETY: the caller's contract: a `struct lemnos_pisp_be_version` in memory the core owns.
    let a = unsafe { &mut *(args as *mut lb::lemnos_pisp_be_version) };
    // SAFETY: the block is alive while its probe reference is held (the caller's contract).
    a.hw_version = unsafe { &*l }.hw_version;
    0
}

/// The stream's private data, with every job slot and the config slots made. Process context.
fn new_stream(
    l: Arc<LemPb>,
    dev: *mut bindings::device,
    job_slots: u32,
) -> Result<Pin<KBox<LpbStream>>> {
    let njobs = job_slots as usize;
    let configs_bytes = njobs * LPB_CONFIG_BYTES;
    let mapped = page_align(configs_bytes).ok_or(ENOMEM)?;
    // SAFETY: an allocation of `mapped` bytes, zeroed and mappable to user space, or null.
    let configs = unsafe { lb::vmalloc_user_noprof(mapped as c_ulong) };
    if configs.is_null() {
        return Err(ENOMEM);
    }
    let configs = Configs(configs);
    let mut free = KVec::with_capacity(njobs, GFP_KERNEL)?;
    for _ in 0..njobs {
        free.push(LpbJob::new(dev)?, GFP_KERNEL)?;
    }
    let jobs = StreamJobs {
        free,
        ready: Ready::new(njobs)?,
        queued: None,
        running: None,
        stopping: false,
        wedged: false,
        sequence: 0,
    };
    KBox::pin_init(
        pin_init!(LpbStream {
            l,
            s: AtomicPtr::new(ptr::null_mut()),
            configs,
            configs_bytes,
            config_slots: job_slots,
            njobs: job_slots,
            collect_lock <- new_mutex!(false),
            jobs: UnsafeCell::new(jobs),
            idle <- WaitQueue::new(c"lem_pb.idle"),
            collect <- DelayedWork::new(c"lem_pb.collect", lpb_collect_work),
        }),
        GFP_KERNEL,
    )
}

/// LEMNOS_IOC_PISP_BE_CREATE: a stream on this block, or its errno. `args` is the core's copy.
///
/// # Safety
/// `l` is a block from `lem_pb_probe` that has not been removed; `dev` is its device; `args` is a
/// `struct lemnos_pisp_be_create` the caller owns.
pub(crate) unsafe extern "C" fn lem_pb_create(
    l: *const LemPb,
    dev: *mut bindings::device,
    args: *mut c_void,
) -> c_long {
    // SAFETY: the caller's contract: a `struct lemnos_pisp_be_create` in memory the core owns.
    let a = unsafe { &mut *(args as *mut lb::lemnos_pisp_be_create) };
    // SAFETY: the block is alive while its probe reference is held (the caller's contract).
    let block = unsafe { &*l };
    if a.stream.job_bytes as usize != size_of::<lb::lemnos_pisp_be_job>()
        || a.stream.job_slots == 0
        || a.stream.job_slots > lb::LEMNOS_STREAM_MAX_JOB_SLOTS
        || !a.stream.job_slots.is_power_of_two()
    {
        return EINVAL.to_errno() as c_long;
    }
    // The stream holds its own reference to the block, so the block outlives a wedged stream.
    // SAFETY: `l` is the probe's reference, live; it is re-leaked by into_raw below.
    let probe_ref = unsafe { Arc::from_raw(l) };
    let stream_ref = probe_ref.clone();
    let _ = Arc::into_raw(probe_ref);

    let ps = match new_stream(stream_ref, dev, a.stream.job_slots) {
        Ok(ps) => ps,
        Err(e) => return e.to_errno() as c_long,
    };
    let configs_bytes = ps.configs_bytes;
    // SAFETY: the stream is never moved out of the box; it is freed only through lpb_release.
    let ps = KBox::into_raw(unsafe { Pin::into_inner_unchecked(ps) });

    let _create = block.create_lock.lock();
    let refused = {
        let mut st = block.state.lock();
        // An idle block's counters agree; new jobs are counted from here.
        let batch = block.rd(BE_BATCH_STATUS_REG);
        if st.wedged {
            Some(EIO)
        } else if !st.attached.is_null()
            || batch as u8 != (batch >> 8) as u8
            || block.rd(BE_STATUS_REG) != 0
        {
            Some(EBUSY)
        } else {
            st.done = batch as u8;
            st.started = (batch >> 8) as u8;
            st.busy = false;
            // Nothing it can do until the stream has its first job.
            st.attached = ps;
            None
        }
    };
    if let Some(e) = refused {
        // SAFETY: the stream never existed, so nothing else holds the stream's private data.
        drop(unsafe { KBox::from_raw(ps) });
        return e.to_errno() as c_long;
    }

    // The stream's frame is a whole buffer; where a job writes is proved job by job.
    let producer = Producer {
        stop: Some(lpb_stop),
        private: ps.cast(),
        job_bytes: size_of::<lb::lemnos_pisp_be_job>() as u32,
        kick: Some(lpb_kick),
        ioctl: Some(lpb_ioctl),
        mmap: Some(lpb_mmap),
        release: Some(lpb_release),
    };
    // SAFETY: the stream's own pointer is set by the core before the file exists; `ps` is the private data.
    let s_slot = unsafe { &*ps }.s.as_ptr();
    // SAFETY: the create arguments are the core's copy; the producer is live for the call.
    let ret = unsafe {
        crate::stream::lem_stream_create_producer(dev.cast(), &raw mut a.stream, &producer, s_slot)
    };
    if ret != 0 {
        block.state.lock().attached = ptr::null_mut();
        // SAFETY: the stream was not made, so only this call holds the private data.
        drop(unsafe { KBox::from_raw(ps) });
        return ret;
    }

    a.config_slot_bytes = LPB_CONFIG_BYTES as u32;
    a.hw_version = block.hw_version;
    a.config_mmap_offset = (LPB_CONFIG_PGOFF as u64) << PAGE_SHIFT;
    a.config_bytes = page_align(configs_bytes).unwrap_or(configs_bytes) as u64;
    0
}

/// The core's remove: the interrupt's work is done and the streams are gone (or wedged).
///
/// # Safety
/// `l` is from `lem_pb_probe`, and the device is being unbound.
pub(crate) unsafe extern "C" fn lem_pb_remove(l: *const LemPb) {
    // SAFETY: the caller's contract: the block is alive until the device's release.
    let l = unsafe { &*l };
    {
        let mut st = l.state.lock();
        l.wr(BE_INTERRUPT_EN_REG, 0);
        st.attached = ptr::null_mut();
    }
    l.watchdog.cancel_sync();
}

/// devres release of the probe's reference to the block: after the interrupt and the queue.
unsafe extern "C" fn lpb_put(data: *mut c_void) {
    // SAFETY: the reference probe made, given up once here.
    drop(unsafe { Arc::from_raw(data as *const LemPb) });
}

/// devres release of the workqueue, after the work that may run on it.
unsafe extern "C" fn lpb_destroy_wq(data: *mut c_void) {
    // SAFETY: the queue probe made, destroyed once here.
    Wq(data.cast()).destroy();
}

/// pispbe_hw_init: a known version, and a block that is idle. The version, and the block's counters.
fn hw_init(dev: *mut bindings::device, regs: *mut c_void) -> Result<(u32, u8, u8)> {
    let version = rd_reg(regs, BE_VERSION_REG);
    if version & !BE_VERSION_MINOR_BITS != BE_VERSION_BLOCK {
        return Err(ENODEV);
    }
    wr_reg(regs, BE_INTERRUPT_STATUS_REG, 0xffff_ffff);
    let batch = rd_reg(regs, BE_BATCH_STATUS_REG);
    let (done, started) = (batch as u8, (batch >> 8) as u8);
    let status = rd_reg(regs, BE_STATUS_REG);
    if status != 0 || done != started {
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_err!(d, "block is stuck or busy (status 0x{:08x})\n", status);
        return Err(EBUSY);
    }
    wr_reg(regs, BE_AXI_REG, BE_AXI_VALUE);
    wr_reg(regs, BE_INTERRUPT_EN_REG, 0x3);
    Ok((version, done, started))
}

/// Maps the block, checks it is one this file knows and idle, and requests its interrupt. The devres
/// order is the C's: the block (released last), the queue, the interrupt.
fn probe(pdev: *mut bindings::platform_device) -> Result<*const LemPb> {
    // SAFETY: the device being probed.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: as above.
    let res = unsafe { bindings::platform_get_resource(pdev, bindings::IORESOURCE_MEM, 0) };
    // SAFETY: a resource of the device, checked non-null first.
    if res.is_null() || (unsafe { bindings::resource_size(res) } as usize) < LPB_WINDOW {
        return Err(EINVAL);
    }
    // SAFETY: as above.
    let start = unsafe { (*res).start };
    // SAFETY: as above.
    let size = unsafe { bindings::resource_size(res) };
    let regs: *mut c_void = if FAKE_REGS.get() {
        // SAFETY: the device's own resource; devres unmaps it at unbind.
        let mem = from_err_ptr(unsafe {
            bindings::devm_memremap(dev, start, size as usize, bindings::MEMREMAP_WB as c_ulong)
        })?;
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "test_pb_fake_regs: registers are ordinary memory\n");
        mem
    } else {
        // SAFETY: as above.
        from_err_ptr(unsafe { bindings::devm_ioremap_resource(dev, res) })?
    };
    // SAFETY: the device being probed; its interrupt number.
    let irq = unsafe { bindings::platform_get_irq(pdev, 0) };
    if irq <= 0 {
        return Err(EINVAL);
    }
    // SAFETY: the device being probed; DMA_BIT_MASK(36).
    to_result(unsafe { bindings::dma_set_mask_and_coherent(dev, (1 << 36) - 1) })?;
    // SAFETY: the device being probed; its one optional clock, enabled and released with the device.
    from_err_ptr(unsafe { bindings::devm_clk_get_optional_enabled(dev, ptr::null()) })?;
    let (hw_version, done, started) = hw_init(dev, regs)?;

    let wq = match Wq::new_ordered(c"lemnos-pisp-be") {
        Ok(wq) => wq,
        Err(e) => {
            wr_reg(regs, BE_INTERRUPT_EN_REG, 0);
            return Err(e);
        }
    };
    let block = match Arc::pin_init(
        pin_init!(LemPb {
            dev,
            regs,
            wq,
            hw_version,
            create_lock <- new_mutex!(()),
            state <- new_spinlock_irq!(PbState {
                attached: ptr::null_mut(),
                done,
                started,
                busy: false,
                wedged: false,
                module_held: false,
                finished: 0,
                refused: 0,
            }),
            watchdog <- DelayedWork::new(c"lem_pb.watchdog", lpb_watchdog),
        }),
        GFP_KERNEL,
    ) {
        Ok(b) => b,
        Err(e) => {
            wr_reg(regs, BE_INTERRUPT_EN_REG, 0);
            wq.destroy();
            return Err(e);
        }
    };
    let raw = Arc::into_raw(block);
    // devm_add_action does not run an action that fails to register, so each failure is undone here.
    // SAFETY: the device being probed; `raw` is the probe's reference, given up by lpb_put.
    if let Err(e) = to_result(unsafe {
        bindings::__devm_add_action(
            dev,
            Some(lpb_put),
            raw as *mut c_void,
            c"lem_pb".as_char_ptr(),
        )
    }) {
        wr_reg(regs, BE_INTERRUPT_EN_REG, 0);
        // SAFETY: the reference made above, not yet owned by devres.
        drop(unsafe { Arc::from_raw(raw) });
        wq.destroy();
        return Err(e);
    }
    // SAFETY: the device being probed; the queue is destroyed by its devres, after the work.
    if let Err(e) = to_result(unsafe {
        bindings::__devm_add_action(
            dev,
            Some(lpb_destroy_wq),
            wq.0.cast(),
            c"lem_pb".as_char_ptr(),
        )
    }) {
        wr_reg(regs, BE_INTERRUPT_EN_REG, 0);
        wq.destroy();
        return Err(e);
    }
    // SAFETY: the device being probed, whose name lives as long as it.
    let name = unsafe { bindings::dev_name(dev) };
    // SAFETY: the device being probed; the block is the handler's data, released after the handler.
    if let Err(e) = to_result(unsafe {
        bindings::devm_request_threaded_irq(
            dev,
            irq as c_uint,
            Some(lpb_isr),
            None,
            0,
            name,
            raw as *mut c_void,
        )
    }) {
        wr_reg(regs, BE_INTERRUPT_EN_REG, 0);
        return Err(e);
    }
    Ok(raw)
}

/// The core's probe: NULL is never returned; an error pointer when the block could not be made.
///
/// # Safety
/// `pdev` is the platform device being probed.
pub(crate) unsafe extern "C" fn lem_pb_probe(pdev: *mut bindings::platform_device) -> *const LemPb {
    match probe(pdev) {
        Ok(l) => l,
        Err(e) => e.to_ptr(),
    }
}
