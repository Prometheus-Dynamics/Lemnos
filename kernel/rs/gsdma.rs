// SPDX-License-Identifier: GPL-2.0
//! The K230 graphics DMA engine (GDMA), memory to memory, as a frame stream (stage 1k of the port;
//! replaces lemnos_gsdma.c). Jobs are posted on the stream's ring (lemnos_gsdma.h).
//!
//! The engine has no MMU in front of it: every address it is given is memory it will read or write. So
//! a job is read once into kernel memory; the bytes it touches on each plane are worked out from the
//! format, size and rotation, and every span is proved inside a dma-buf registered on the stream and
//! below the end of the memory the engine reaches, with arithmetic that cannot wrap; only then are the
//! descriptors built, in coherent memory of the module's own, from the device addresses the stream
//! computed. Nothing is clamped: a job that fails a check ends the stream.
//!
//! Interface facts are the vendor driver's (see lemnos_gsdma.h and the C original's notes, recorded in
//! the git history); the stopping rules are the module's own. Locking as in the C: `state` (the job on
//! the engine, the registers while it starts, the attached stream), the collector's `collect_lock`, and
//! never the stream's lock then `state`.

use crate::bindings as lb;
use crate::param::param;
use crate::stream::{
    module_hold, ktime_add_ms, ktime_now, msecs_to_jiffies, usleep_range, DelayedWork, Handle, Producer,
    Slot, Stream, WaitQueue, Wq,
};
use core::{
    ffi::{c_int, c_long, c_void},
    mem::offset_of,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use kernel::{
    alloc::KBox,
    bindings,
    device::Device,
    error::{code::*, from_err_ptr, to_result, Error, Result},
    ffi::{c_uint, c_ulong},
    ioctl::_IOR,
    new_mutex, new_spinlock_irq,
    prelude::*,
    sync::{Arc, Mutex, SpinLockIrq},
    transmute::AsBytes,
    uaccess::{UserPtr, UserSlice},
};

// Registers (struct k230_gdma_regs).
const GD_CH_EN: usize = 0x00;
const GD_CH_EN_BIT: u32 = 1 << 4;
const GD_INT_MASK: usize = 0x04;
const GD_INT_STAT: usize = 0x08;
const GD_CFG: usize = 0x0c;
const GD_CTRL: usize = 0x10;
const GD_LLI_BASE: usize = 0x14;
const GD_CURRENT_LLT: usize = 0x38;
const GD_CURRENT_SIZE: usize = 0x3c;
const GD_CURRENT_SADDR: usize = 0x40;
const GD_CURRENT_DADDR: usize = 0x44;
const GD_WEIGHT: usize = 0x48;
const GD_WINDOW: u64 = 0x50;
const GD_DONE_INT: u32 = 1 << 18;
const GD_INT_BITS: u32 = 0x7 << 16;
const GD_CFG_VALUE: u32 = (7 << 8) | 0xff;
const GD_WEIGHT_VALUE: u32 = 0x0044_4210;

// The descriptor (struct k230_gdma_llt): six little-endian words.
const GD_DESC_WORDS: usize = 6;
const GD_DESC_BYTES: u64 = (GD_DESC_WORDS * 4) as u64;
const GD_W0_MIRROR_X: u32 = 1 << 2;
const GD_W0_MIRROR_Y: u32 = 1 << 3;
const GD_W0_NODE_INTR: u32 = 1 << 30;
const GD_PIXEL_8: u32 = 0;
const GD_PIXEL_16: u32 = 1;
const GD_PIXEL_24: u32 = 2;
const GD_PIXEL_32: u32 = 3;

/// K230_DDR_PHYS_END: DDR from 0, and what a 32-bit address reaches with it.
const GD_DDR_END: u64 = 0x8000_0000;
const GD_MAX_FIELD: u32 = 0xffff;
const GD_PLANES: usize = lb::LEMNOS_GSDMA_MAX_PLANES as usize;

// The vendor driver's timeout, plus 1 ms per 256 KiB the job writes.
const GD_TIMEOUT_MS: u32 = 100;
const GD_TIMEOUT_SHIFT: u32 = 18;
// Stillness: the progress registers unchanged for HOLD, read every GAP, given up after STILL.
const GD_STILL_HOLD_MS: i64 = 10;
const GD_STILL_GAP_US: u64 = 500;
const GD_STILL_MS: u64 = 100;
/// The longest a job can be given, with its stopping, and a margin.
const GD_STOP_WAIT_MS: u64 = GD_TIMEOUT_MS as u64 + ((2 * GD_DDR_END) >> GD_TIMEOUT_SHIFT) + 2 * GD_STILL_MS + 1000;

// Errnos the checks return that the kernel crate does not name.
const ENOTUNIQ: i32 = 76;

const IOC_STATUS: u32 = _IOR::<lb::lemnos_gsdma_status>(b'L' as u32, 0xe9);

const _: () = assert!(core::mem::size_of::<lb::lemnos_gsdma_job>() == 104);
const _: () = assert!(core::mem::size_of::<lb::lemnos_gsdma_create>() == 64);

param!(
    FAKE_REGS,
    test_gsdma_fake_regs: bool = false,
    param_ops_bool,
    "bool",
    "Test only: the GDMA's register window is ordinary memory (the test harness)"
);
param!(
    REACH,
    test_gsdma_reach: u64 = 0,
    param_ops_ullong,
    "ullong",
    "Test only: lowers the end of the memory a job's images may lie in (bytes; 0 leaves it)"
);

/// One format the engine takes: per plane, the bytes of an element, its descriptor code, and the
/// subsampling across and down (k230_gdma_build_format_desc's ten formats).
struct Format {
    fourcc: u32,
    planes: u32,
    bytes: [u8; 2],
    code: [u32; 2],
    across: [u32; 2],
    down: [u32; 2],
}

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    a as u32 | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
}

const FORMATS: [Format; 10] = [
    Format { fourcc: fourcc(b'A', b'R', b'1', b'5'), planes: 1, bytes: [2, 0], code: [GD_PIXEL_16, 0], across: [1, 0], down: [1, 0] },
    Format { fourcc: fourcc(b'A', b'R', b'1', b'2'), planes: 1, bytes: [2, 0], code: [GD_PIXEL_16, 0], across: [1, 0], down: [1, 0] },
    Format { fourcc: fourcc(b'R', b'G', b'1', b'6'), planes: 1, bytes: [2, 0], code: [GD_PIXEL_16, 0], across: [1, 0], down: [1, 0] },
    Format { fourcc: fourcc(b'R', b'G', b'2', b'4'), planes: 1, bytes: [3, 0], code: [GD_PIXEL_24, 0], across: [1, 0], down: [1, 0] },
    Format { fourcc: fourcc(b'B', b'G', b'2', b'4'), planes: 1, bytes: [3, 0], code: [GD_PIXEL_24, 0], across: [1, 0], down: [1, 0] },
    Format { fourcc: fourcc(b'A', b'R', b'2', b'4'), planes: 1, bytes: [4, 0], code: [GD_PIXEL_32, 0], across: [1, 0], down: [1, 0] },
    Format { fourcc: fourcc(b'N', b'V', b'1', b'2'), planes: 2, bytes: [1, 2], code: [GD_PIXEL_8, GD_PIXEL_16], across: [1, 2], down: [1, 2] },
    Format { fourcc: fourcc(b'N', b'V', b'2', b'1'), planes: 2, bytes: [1, 2], code: [GD_PIXEL_8, GD_PIXEL_16], across: [1, 2], down: [1, 2] },
    Format { fourcc: fourcc(b'N', b'V', b'1', b'6'), planes: 2, bytes: [1, 2], code: [GD_PIXEL_8, GD_PIXEL_16], across: [1, 2], down: [1, 1] },
    Format { fourcc: fourcc(b'N', b'V', b'6', b'1'), planes: 2, bytes: [1, 2], code: [GD_PIXEL_8, GD_PIXEL_16], across: [1, 2], down: [1, 1] },
];

/// A job, checked: what is built into descriptors.
#[derive(Default)]
struct Plan {
    planes: usize,
    word0: [u32; GD_PLANES],
    width: [u32; GD_PLANES],
    height: [u32; GD_PLANES],
    src_pitch: [u32; GD_PLANES],
    dst_pitch: [u32; GD_PLANES],
    /// Ranges for job_take: source then destination, per plane.
    range: [lb::lemnos_stream_range; 2 * GD_PLANES],
    /// Written, for the timeout.
    bytes: u64,
}

/// The state under `LemGd::state`: the job on the engine, the attached stream's flags, the counts.
#[derive(Default)]
struct GdState {
    /// The attached stream, or null. Its flags below belong to it while it is attached.
    attached: *mut LgdStream,
    stopping: bool,
    stream_wedged: bool,
    sequence: u64,
    /// The engine has `slot`.
    running: bool,
    /// The watchdog is stopping it.
    aborting: bool,
    wedged: bool,
    module_held: bool,
    slot: Slot,
    finished: u64,
    refused: u64,
    timeouts: u64,
}

/// One block (`struct lem_gd`). Made by `lem_gd_probe`, and shared by the probe's reference (given up
/// when the device is unbound) and each stream's.
#[pin_data]
pub(crate) struct LemGd {
    dev: *mut bindings::device,
    regs: *mut c_void,
    wq: Wq,
    /// The descriptors: coherent memory the engine walks, below its reach.
    chain: *mut u32,
    chain_dma: bindings::dma_addr_t,
    /// One past the last address an image may use.
    reach: u64,
    irq: c_int,
    #[pin]
    create_lock: Mutex<()>,
    #[pin]
    state: SpinLockIrq<GdState>,
    #[pin]
    watchdog: DelayedWork,
}

// SAFETY: the raw pointers are the device's own, valid for the block's life. The engine's registers are
// accessed under `state` or from the collector; the descriptors are written only by the collector while
// the engine is not running; the rest is immutable after probe.
unsafe impl Send for LemGd {}
// SAFETY: as Send: every shared access is made under `state`, or is an immutable field.
unsafe impl Sync for LemGd {}

/// One stream on the block (`struct lgd_stream`): the producer's private data, freed at release.
#[pin_data]
struct LgdStream {
    g: Arc<LemGd>,
    /// The stream the core made for this producer, set before any job can run.
    s: AtomicPtr<Stream>,
    /// One collector at a time; `true` once no collection may touch the block.
    #[pin]
    collect_lock: Mutex<bool>,
    #[pin]
    idle: WaitQueue,
    #[pin]
    collect: DelayedWork,
}

impl LemGd {
    fn rd(&self, off: usize) -> u32 {
        // SAFETY: the register window is mapped for GD_WINDOW bytes (probe checked the resource's size),
        // and every offset read is below it.
        unsafe { bindings::readl(self.regs.cast::<u8>().add(off).cast()) }
    }

    fn wr(&self, off: usize, val: u32) {
        // SAFETY: as rd.
        unsafe { bindings::writel(val, self.regs.cast::<u8>().add(off).cast()) };
    }

    fn channel(&self, on: bool) {
        let v = self.rd(GD_CH_EN);
        self.wr(GD_CH_EN, if on { v | GD_CH_EN_BIT } else { v & !GD_CH_EN_BIT });
    }

    /// The descriptor word at `i`, written once it is complete (WRITE_ONCE in the C).
    fn put_desc_word(&self, i: usize, v: u32) {
        // SAFETY: the chain is GD_PLANES * GD_DESC_WORDS words of coherent memory, `i` is below that.
        unsafe { ptr::write_volatile(self.chain.add(i), v.to_le()) };
    }

    /// The block can't be given up safely. `state` held.
    fn wedge(&self, st: &mut GdState, ps: *mut LgdStream, err: c_int) {
        if !st.wedged {
            // SAFETY: the device is live while the block is.
            let d: &Device = unsafe { Device::from_raw(self.dev) };
            dev_crit!(d, "engine did not stop: its job's buffers stay held until a reboot\n");
        }
        st.wedged = true;
        if !st.module_held {
            st.module_held = true;
            module_hold();
        }
        if !ps.is_null() {
            // SAFETY: the attached stream lives until it is detached, which this lock orders.
            let p = unsafe { &*ps };
            st.stream_wedged = true;
            Handle(p.s.load(Ordering::Relaxed)).fault(err);
            p.idle.wake_all();
        }
    }

    /// The job on the engine is over: publish its frame. `state` held.
    fn finish(&self, st: &mut GdState, ps: *mut LgdStream, bytes: u32) {
        st.running = false;
        st.aborting = false;
        // SAFETY: the attached stream, as in wedge.
        let p = unsafe { &*ps };
        let seq = st.sequence;
        st.sequence += 1;
        Handle(p.s.load(Ordering::Relaxed)).complete(&st.slot, bytes, seq, ktime_now() as u64);
        p.idle.wake_all();
        if !st.stopping {
            p.collect.queue(self.wq, 0);
        }
    }

    /// Whether the engine's progress holds still, read over a bounded time. Sleeps.
    fn still(&self) -> bool {
        let start = ktime_now();
        let until = ktime_add_ms(start, GD_STILL_MS);
        let mut since = start;
        let mut last = [0u32; 4];
        let mut first = true;
        loop {
            let t = ktime_now();
            let now = [
                self.rd(GD_CURRENT_LLT),
                self.rd(GD_CURRENT_SIZE),
                self.rd(GD_CURRENT_SADDR),
                self.rd(GD_CURRENT_DADDR),
            ];
            if first || now != last {
                since = t;
            } else if (t - since) / 1_000_000 >= GD_STILL_HOLD_MS {
                return true;
            }
            if t >= until {
                return false;
            }
            first = false;
            last = now;
            usleep_range(GD_STILL_GAP_US, 2 * GD_STILL_GAP_US);
        }
    }

}

/// The ISR: a done interrupt on the job's stream ends it (the watchdog's end, when it stops the engine,
/// is its own).
unsafe extern "C" fn lgd_isr(_irq: c_int, data: *mut c_void) -> bindings::irqreturn_t {
    // SAFETY: the interrupt is freed before the block's reference is given up (probe's order).
    let g = unsafe { &*(data as *const LemGd) };
    let stat = g.rd(GD_INT_STAT);
    if stat == 0 {
        return bindings::irqreturn::IRQ_NONE;
    }
    g.wr(GD_INT_STAT, stat);
    let mut st = g.state.lock();
    let ps = st.attached;
    // While the watchdog stops it, the job's end is the watchdog's to call.
    if !ps.is_null() && st.running && !st.aborting && !st.wedged && (stat & GD_DONE_INT) != 0 {
        st.finished += 1;
        g.watchdog.cancel();
        let bytes = st.slot.bytes;
        g.finish(&mut st, ps, bytes);
    }
    bindings::irqreturn::IRQ_HANDLED
}

/// A job outstanding too long: stop the channel, and only let go once it is seen stopped.
unsafe extern "C" fn lgd_watchdog(work: *mut bindings::work_struct) {
    // SAFETY: the work is the watchdog's, and the block is the container of the watchdog.
    let g = unsafe { &*(work.cast::<u8>().sub(offset_of!(LemGd, watchdog)).cast::<LemGd>()) };
    let mut st = g.state.lock();
    if !st.running || st.aborting || st.wedged {
        return;
    }
    st.aborting = true;
    g.channel(false);
    g.wr(GD_INT_STAT, 0xffff_ffff);
    drop(st);

    let still = g.still();

    let mut st = g.state.lock();
    let ps = st.attached;
    if still {
        g.channel(true);
        st.timeouts += 1;
        // The C rate-limits this warning; the kernel crate has no ratelimit, so it is printed each time.
        // SAFETY: the device is live while the block is.
        let d: &Device = unsafe { Device::from_raw(g.dev) };
        dev_warn!(d, "job timed out; engine stopped and ready again\n");
        if !ps.is_null() {
            g.finish(&mut st, ps, 0);
        } else {
            st.running = false;
            st.aborting = false;
        }
    } else {
        g.wedge(&mut st, ps, EIO.to_errno());
    }
}

/// The checks of one span on one side of one plane: `rows` rows of `row` bytes, `pitch` apart, from
/// `addr`. Proves the pitch and that the span neither wraps nor passes `reach`.
fn span(addr: u64, pitch: u32, rows: u64, row: u64, reach: u64) -> Result<lb::lemnos_stream_range> {
    if pitch == 0 || pitch > GD_MAX_FIELD || (pitch as u64) < row {
        return Err(EINVAL);
    }
    let extent = pitch as u64 * (rows - 1) + row;
    let end = addr.checked_add(extent).ok_or(EOVERFLOW)?;
    if end > reach {
        return Err(ENXIO);
    }
    Ok(lb::lemnos_stream_range { addr, len: extent })
}

fn overlap(a: &lb::lemnos_stream_range, b: &lb::lemnos_stream_range) -> bool {
    // Both were proved not to wrap.
    a.addr < b.addr + b.len && b.addr < a.addr + a.len
}

/// Checks the kernel's copy of a job. Touches nothing.
fn validate(reach: u64, job: &lb::lemnos_gsdma_job) -> Result<Plan> {
    let f = FORMATS.iter().find(|f| f.fourcc == job.fourcc).ok_or(EINVAL)?;
    if job.planes != f.planes {
        return Err(EINVAL);
    }
    if job.width == 0 || job.width > GD_MAX_FIELD || job.height == 0 || job.height > GD_MAX_FIELD {
        return Err(EINVAL);
    }
    // Exactly one turn, mirrors, nothing else.
    let rotate = job.rotation & 0xf;
    if job.rotation & !0x3f != 0 || rotate.count_ones() != 1 {
        return Err(EINVAL);
    }
    let turn = rotate.trailing_zeros();
    let swaps = turn & 1 == 1;
    // Planes the format doesn't have are all zero.
    for p in &job.plane[f.planes as usize..GD_PLANES] {
        if p.src_addr != 0 || p.dst_addr != 0 || p.src_pitch != 0 || p.dst_pitch != 0 {
            return Err(EINVAL);
        }
    }

    let mut plan = Plan { planes: f.planes as usize, ..Plan::default() };
    for i in 0..f.planes as usize {
        let p = &job.plane[i];
        let width = (job.width as u64).div_ceil(f.across[i] as u64);
        let height = (job.height as u64).div_ceil(f.down[i] as u64);
        let bytes = f.bytes[i] as u64;

        let src = span(p.src_addr, p.src_pitch, height, width * bytes, reach)?;
        let dst = if swaps {
            span(p.dst_addr, p.dst_pitch, width, height * bytes, reach)?
        } else {
            span(p.dst_addr, p.dst_pitch, height, width * bytes, reach)?
        };
        let mut w0 = turn | (f.code[i] << 8);
        if job.rotation & lb::LEMNOS_GSDMA_REFLECT_X != 0 {
            w0 |= GD_W0_MIRROR_X;
        }
        if job.rotation & lb::LEMNOS_GSDMA_REFLECT_Y != 0 {
            w0 |= GD_W0_MIRROR_Y;
        }
        plan.word0[i] = w0;
        plan.width[i] = width as u32;
        plan.height[i] = height as u32;
        plan.src_pitch[i] = p.src_pitch;
        plan.dst_pitch[i] = p.dst_pitch;
        plan.bytes += dst.len;
        plan.range[2 * i] = src;
        plan.range[2 * i + 1] = dst;
    }
    // The engine would read what it already wrote.
    for i in 0..f.planes as usize {
        let dst = &plan.range[2 * i + 1];
        for j in 0..f.planes as usize {
            if overlap(dst, &plan.range[2 * j]) || (j < i && overlap(dst, &plan.range[2 * j + 1])) {
                return Err(Error::from_errno(-ENOTUNIQ));
            }
        }
    }
    Ok(plan)
}

/// The descriptors, from the device addresses the stream computed for the ranges it holds. Each is
/// checked again against the reach, so nothing outside the memory the engine may use can reach a
/// descriptor whatever happened between.
fn build(g: &LemGd, s: Handle, plan: &Plan, slot: &Slot) -> Result {
    let mut desc = [0u32; GD_PLANES * GD_DESC_WORDS];
    for i in 0..plan.planes {
        let last = i + 1 == plan.planes;
        let src = s.range_addr(&slot.job.range[2 * i])?;
        let dst = s.range_addr(&slot.job.range[2 * i + 1])?;
        let src_end = src.checked_add(slot.job.range[2 * i].len).ok_or(ENXIO)?;
        let dst_end = dst.checked_add(slot.job.range[2 * i + 1].len).ok_or(ENXIO)?;
        if src_end > g.reach || dst_end > g.reach {
            return Err(ENXIO);
        }
        let d = &mut desc[i * GD_DESC_WORDS..(i + 1) * GD_DESC_WORDS];
        d[0] = plan.word0[i] | if last { GD_W0_NODE_INTR } else { 0 };
        d[1] = src as u32;
        d[2] = plan.width[i] | plan.height[i] << 16;
        d[3] = plan.src_pitch[i] | plan.dst_pitch[i] << 16;
        d[4] = dst as u32;
        // The chain was proved inside the engine's reach at probe.
        d[5] = if last { 0 } else { (g.chain_dma as u64 + (i as u64 + 1) * GD_DESC_BYTES) as u32 };
    }
    for (i, v) in desc.iter().enumerate() {
        g.put_desc_word(i, *v);
    }
    Ok(())
}

impl LgdStream {
    /// Takes the job at the front of the ring, if any, and starts it. `stopped` is the collector's lock
    /// held: nothing starts once the stream is stopping.
    fn collect_locked(&self, stopped: bool) -> c_long {
        let g = &*self.g;
        if stopped {
            return ENODEV.to_errno() as c_long;
        }
        {
            let st = g.state.lock();
            if st.wedged || st.stopping {
                return if st.wedged { EIO.to_errno() as c_long } else { ENODEV.to_errno() as c_long };
            }
            // One job at a time; the next is collected when it ends.
            if st.running {
                return 0;
            }
        }
        let s = Handle(self.s.load(Ordering::Relaxed));
        let mut job = lb::lemnos_gsdma_job::default();
        let ret = s.job_peek(&mut job);
        if ret <= 0 {
            return ret;
        }
        let plan = match validate(g.reach, &job) {
            Ok(p) => p,
            Err(e) => return refuse(g, e),
        };
        let mut slot = Slot::default();
        if let Err(e) = s.job_take(job.buffer, job.identity, &plan.range[..2 * plan.planes], &mut slot) {
            return refuse(g, e);
        }
        // Nothing runs on the engine: the chain is the collector's to write.
        if let Err(e) = build(g, s, &plan, &slot) {
            // A span in the pool resolves, but is no image: never started, dropped.
            s.cancel(&slot);
            return refuse(g, e);
        }
        let timeout = GD_TIMEOUT_MS as u64 + (plan.bytes >> GD_TIMEOUT_SHIFT);

        let mut st = g.state.lock();
        if st.wedged || st.stopping {
            let e = if st.wedged { EIO.to_errno() as c_long } else { ENODEV.to_errno() as c_long };
            drop(st);
            s.cancel(&slot);
            return e;
        }
        st.slot = slot;
        st.running = true;
        g.wr(GD_INT_STAT, 0xffff_ffff);
        g.wr(GD_LLI_BASE, g.chain_dma as u32);
        // The chain and the base before the start.
        core::sync::atomic::fence(Ordering::Release);
        g.wr(GD_CTRL, 1);
        g.watchdog.modify(g.wq, msecs_to_jiffies(timeout as u32));
        0
    }
}

/// A refused job: counted, and the stream ends with its errno (the C's refuse path).
fn refuse(g: &LemGd, e: Error) -> c_long {
    g.state.lock().refused += 1;
    e.to_errno() as c_long
}


// SAFETY: the status is a plain struct of integers, with no padding the kernel reads.
unsafe impl AsBytes for lb::lemnos_gsdma_status {}

/// LEMNOS_STREAM_IOC_KICK. An errno ends the stream.
unsafe extern "C" fn lgd_kick(priv_: *mut c_void) -> c_long {
    // SAFETY: the producer's private data: the stream's, alive until its release.
    let ps = unsafe { &*(priv_ as *const LgdStream) };
    let stopped = ps.collect_lock.lock();
    let ret = ps.collect_locked(*stopped);
    drop(stopped);
    ret
}

/// After a job ends: pick up what the consumer posted since, without a kick.
unsafe extern "C" fn lgd_collect_work(work: *mut bindings::work_struct) {
    // SAFETY: the work is the stream's collect work, which lives in its stream.
    let ps = unsafe { &*(work.cast::<u8>().sub(offset_of!(LgdStream, collect)).cast::<LgdStream>()) };
    let stopped = ps.collect_lock.lock();
    let ret = ps.collect_locked(*stopped);
    drop(stopped);
    if ret != 0 && ret != ENODEV.to_errno() as c_long {
        Handle(ps.s.load(Ordering::Relaxed)).fault(ret as c_int);
    }
}

/// Stops DMA. The job on the engine finishes or is stopped by the watchdog, or the engine is wedged and
/// the stream keeps its pins. The stream's lock is held by the core: it must not return until the block
/// can no longer touch any buffer.
unsafe extern "C" fn lgd_stop(priv_: *mut c_void, s: *mut Stream) {
    // SAFETY: the producer's private data, alive until release, which the core calls after stop.
    let ps = unsafe { &*(priv_ as *const LgdStream) };
    let g = &*ps.g;
    let ps_ptr = priv_ as *mut LgdStream;

    g.state.lock().stopping = true;
    ps.collect.cancel_sync();

    // A collection already running finishes first; none starts after.
    {
        let mut stopped = ps.collect_lock.lock();
        *stopped = true;
    }

    // The stream's last job, if it is on the engine, is ended by the engine or the watchdog.
    ps.idle.wait_until(msecs_to_jiffies(GD_STOP_WAIT_MS as u32), || {
        let st = g.state.lock();
        !st.running || st.wedged
    });

    let mut st = g.state.lock();
    let busy = st.running && !st.wedged;
    if busy {
        g.wedge(&mut st, ps_ptr, ETIMEDOUT.to_errno());
    }
    let wedged = st.wedged && st.stream_wedged;
    if st.attached == ps_ptr {
        st.attached = ptr::null_mut();
    }
    drop(st);

    if wedged {
        // SAFETY: the device is live while the block is.
        let d: &Device = unsafe { Device::from_raw(g.dev) };
        dev_crit!(d, "stream wedged: keeping its buffers until a reboot\n");
        Handle(s).wedge();
    }
}

/// The stream's file is gone and nothing can reach the private data any more.
unsafe extern "C" fn lgd_release(priv_: *mut c_void) {
    // SAFETY: the private data made by lem_gd_create, released once.
    drop(unsafe { KBox::from_raw(priv_ as *mut LgdStream) });
}

/// The stream's ioctls: the block's status.
unsafe extern "C" fn lgd_ioctl(priv_: *mut c_void, cmd: c_uint, arg: *mut c_void) -> c_long {
    // SAFETY: the producer's private data, alive while the stream's file can reach it.
    let ps = unsafe { &*(priv_ as *const LgdStream) };
    if cmd != IOC_STATUS {
        return ENOTTY.to_errno() as c_long;
    }
    let st = ps.g.state.lock();
    let status = lb::lemnos_gsdma_status {
        wedged: st.wedged as u32,
        reserved: 0,
        finished: st.finished,
        refused: st.refused,
        timeouts: st.timeouts,
    };
    drop(st);
    match UserSlice::new(UserPtr::from_addr(arg as usize), core::mem::size_of::<lb::lemnos_gsdma_status>())
        .writer()
        .write_slice(status.as_bytes())
    {
        Ok(()) => 0,
        Err(_) => EFAULT.to_errno() as c_long,
    }
}

impl LgdStream {
    fn new(g: Arc<LemGd>) -> impl PinInit<Self> {
        pin_init!(Self {
            g,
            s: AtomicPtr::new(ptr::null_mut()),
            collect_lock <- new_mutex!(false),
            idle <- WaitQueue::new(c"lem_gd.idle"),
            collect <- DelayedWork::new(c"lem_gd.collect", lgd_collect_work),
        })
    }
}

/// LEMNOS_IOC_GSDMA_CREATE: a stream on this block, or its errno. `args` is the core's copy.
///
/// # Safety
/// `g` is a block from `lem_gd_probe` that has not been removed; `dev` is its device; `args` is a
/// `struct lemnos_gsdma_create` the caller owns.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_gd_create(g: *const LemGd, dev: *mut bindings::device, args: *mut c_void) -> c_long {
    // SAFETY: the caller's contract: a `struct lemnos_gsdma_create` in memory the core owns.
    let a = unsafe { &mut *(args as *mut lb::lemnos_gsdma_create) };
    // SAFETY: the block is alive while its probe reference is held (the caller's contract).
    let block = unsafe { &*g };
    if a.stream.job_bytes as usize != core::mem::size_of::<lb::lemnos_gsdma_job>() {
        return EINVAL.to_errno() as c_long;
    }
    // The stream holds its own reference to the block, so the block outlives a wedged stream.
    // SAFETY: `g` is the probe's reference, live; it is re-leaked by into_raw below.
    let probe_ref = unsafe { Arc::from_raw(g) };
    let stream_ref = probe_ref.clone();
    let _ = Arc::into_raw(probe_ref);

    let _create = block.create_lock.lock();
    let ps = match KBox::pin_init(LgdStream::new(stream_ref), GFP_KERNEL) {
        Ok(p) => p,
        Err(_) => return ENOMEM.to_errno() as c_long,
    };
    // SAFETY: the stream is never moved out of the box; it is freed only through lgd_release.
    let ps = KBox::into_raw(unsafe { core::pin::Pin::into_inner_unchecked(ps) });

    let ret = {
        let mut st = block.state.lock();
        if st.wedged {
            Some(EIO)
        } else if !st.attached.is_null() {
            Some(EBUSY)
        } else {
            st.attached = ps;
            st.stopping = false;
            st.stream_wedged = false;
            st.sequence = 0;
            None
        }
    };
    if let Some(e) = ret {
        // SAFETY: the stream never existed, so nothing else holds the stream's private data.
        drop(unsafe { KBox::from_raw(ps) });
        return e.to_errno() as c_long;
    }

    let producer = Producer {
        stop: Some(lgd_stop),
        private: ps.cast(),
        job_bytes: core::mem::size_of::<lb::lemnos_gsdma_job>() as u32,
        kick: Some(lgd_kick),
        ioctl: Some(lgd_ioctl),
        mmap: None,
        release: Some(lgd_release),
    };
    // SAFETY: the stream's own pointer is set by the core before the file exists; `ps` is the private data.
    let s_slot = unsafe { &*ps }.s.as_ptr();
    // SAFETY: the create arguments are the core's copy; the producer is live for the call.
    let ret = unsafe { crate::stream::lem_stream_create_producer(dev.cast(), &raw mut a.stream, &producer, s_slot) };
    if ret != 0 {
        block.state.lock().attached = ptr::null_mut();
        // SAFETY: the stream was not made, so only this call holds the private data.
        drop(unsafe { KBox::from_raw(ps) });
        return ret;
    }
    a.reach = block.reach;
    0
}

/// The core's remove: the streams are gone (or wedged). Masks the interrupts, stops the watchdog, and
/// frees the descriptors unless the engine may still read them.
///
/// # Safety
/// `g` is from `lem_gd_probe`, and the device is being unbound.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_gd_remove(g: *const LemGd) {
    // SAFETY: the caller's contract: the block is alive until the device's release.
    let g = unsafe { &*g };
    let wedged = {
        let mut st = g.state.lock();
        g.wr(GD_INT_MASK, g.rd(GD_INT_MASK) | GD_INT_BITS);
        st.attached = ptr::null_mut();
        st.wedged
    };
    g.watchdog.cancel_sync();
    // A wedged engine may still read its chain.
    if !wedged {
        // SAFETY: the chain made by probe, freed once here; the engine is masked and not running.
        unsafe { bindings::dma_free_attrs(g.dev, GD_PLANES * GD_DESC_BYTES as usize, g.chain.cast(), g.chain_dma, 0) };
    }
}

/// devres release of the probe's reference to the block: after the interrupt and the workqueue.
unsafe extern "C" fn lgd_put(data: *mut c_void) {
    // SAFETY: the reference probe made, given up once here.
    drop(unsafe { Arc::from_raw(data as *const LemGd) });
}

/// devres release of the workqueue, after the work that may run on it.
unsafe extern "C" fn lgd_destroy_wq(data: *mut c_void) {
    // SAFETY: the queue probe made, destroyed once here.
    Wq(data.cast()).destroy();
}

/// Maps the block, makes its descriptors, and starts it. The interrupt and its devres follow the C's order.
fn probe(pdev: *mut bindings::platform_device) -> Result<*const LemGd> {
    // SAFETY: the device being probed.
    let dev = unsafe { &raw mut (*pdev).dev };
    let mut reach = GD_DDR_END;
    if REACH.get() != 0 {
        reach = reach.min(REACH.get());
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "test_gsdma_reach: images must end below 0x{:x}\n", reach);
    }

    // SAFETY: the device being probed.
    let res = unsafe { bindings::platform_get_resource(pdev, bindings::IORESOURCE_MEM, 0) };
    // SAFETY: a resource of the device, checked non-null first.
    if res.is_null() || unsafe { bindings::resource_size(res) } < GD_WINDOW {
        return Err(EINVAL);
    }
    // SAFETY: as above.
    let (start, size) = unsafe { ((*res).start, bindings::resource_size(res)) };
    let regs: *mut c_void = if FAKE_REGS.get() {
        // SAFETY: the device's own resource; devres unmaps it at unbind.
        let mem = from_err_ptr(unsafe { bindings::devm_memremap(dev, start, size as usize, bindings::MEMREMAP_WB as c_ulong) })?;
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "test_gsdma_fake_regs: registers are ordinary memory\n");
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
    // 32-bit bus addresses: the dma-bufs mapped for it, and the chain, below 4 GiB.
    // SAFETY: the device being probed; the mask is 32 bits.
    to_result(unsafe { bindings::dma_set_mask_and_coherent(dev, 0xffff_ffff) })?;
    // SAFETY: the device being probed; the optional clock, enabled if present.
    // SAFETY: as above; NULL when there is no clock, an error pointer when it can't be had.
    let _clk = from_err_ptr(unsafe { bindings::devm_clk_get_optional_enabled(dev, ptr::null()) })?;

    let mut chain_dma: bindings::dma_addr_t = 0;
    // SAFETY: the device being probed; the chain's size and handle are the C's.
    let chain = unsafe {
        bindings::dma_alloc_attrs(dev, GD_PLANES * GD_DESC_BYTES as usize, &raw mut chain_dma, bindings::GFP_KERNEL, 0)
    };
    if chain.is_null() {
        return Err(ENOMEM);
    }
    let free_chain = || {
        // SAFETY: the chain allocated above, freed once on the error path.
        unsafe { bindings::dma_free_attrs(dev, GD_PLANES * GD_DESC_BYTES as usize, chain, chain_dma, 0) };
    };
    if chain_dma as u64 + GD_PLANES as u64 * GD_DESC_BYTES > GD_DDR_END {
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_err!(d, "descriptor memory past the engine's reach\n");
        free_chain();
        return Err(ENOMEM);
    }
    let wq = match Wq::new_ordered(c"lemnos-gsdma") {
        Ok(w) => w,
        Err(e) => {
            free_chain();
            return Err(e);
        }
    };

    let block = match Arc::pin_init(
        pin_init!(LemGd {
            dev,
            regs,
            wq,
            chain: chain.cast(),
            chain_dma,
            reach,
            irq,
            create_lock <- new_mutex!(()),
            state <- new_spinlock_irq!(GdState::default()),
            watchdog <- DelayedWork::new(c"lem_gd.watchdog", lgd_watchdog),
        }),
        GFP_KERNEL,
    ) {
        Ok(b) => b,
        Err(e) => {
            wq.destroy();
            free_chain();
            return Err(e);
        }
    };
    let raw = Arc::into_raw(block);
    // The reference's devres comes first, so it is released after the queue (registered below) and the interrupt.
    // SAFETY: the device being probed; `raw` is the probe's reference, given up by lgd_put.
    if let Err(e) = to_result(unsafe { bindings::devm_add_action_or_reset(dev, Some(lgd_put), raw as *mut c_void) }) {
        free_chain();
        return Err(e);
    }
    // SAFETY: the device being probed; the queue is destroyed by its devres, after the work.
    if let Err(e) = to_result(unsafe { bindings::devm_add_action_or_reset(dev, Some(lgd_destroy_wq), wq.0.cast()) }) {
        free_chain();
        return Err(e);
    }

    // Probe's sequence: interrupts clear and masked, channel on, the three unmasked.
    // SAFETY: the registers are the device's, mapped above.
    let block_ref = unsafe { &*raw };
    block_ref.wr(GD_INT_STAT, 0xffff_ffff);
    block_ref.wr(GD_INT_MASK, 0xffff_ffff);
    block_ref.channel(true);
    block_ref.wr(GD_INT_MASK, block_ref.rd(GD_INT_MASK) & !GD_INT_BITS);
    block_ref.wr(GD_CFG, GD_CFG_VALUE);
    block_ref.wr(GD_WEIGHT, GD_WEIGHT_VALUE);

    // SAFETY: the device being probed; the block is the handler's data, released after the handler.
    if let Err(e) = to_result(unsafe {
        bindings::devm_request_threaded_irq(dev, irq as c_uint, Some(lgd_isr), None, 0, bindings::dev_name(dev), raw as *mut c_void)
    }) {
        free_chain();
        return Err(e);
    }
    Ok(raw)
}

/// The core's probe: NULL when the device is not listed, else the block or an error pointer.
///
/// # Safety
/// `pdev` is the platform device being probed.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_gd_probe(pdev: *mut bindings::platform_device) -> *const LemGd {
    match probe(pdev) {
        Ok(g) => g,
        Err(e) => e.to_ptr(),
    }
}
