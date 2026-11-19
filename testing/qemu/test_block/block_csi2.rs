// SPDX-License-Identifier: GPL-2.0
//! The fake camera receiver, /dev/lemnos-test-csi2: the requests and ioctl numbers of
//! lemnos_test_csi2.h, which are this fixture's own interface, and the device lemnos_test_block.c
//! drives with them. The registers are memory the guest set aside (memmap=): four 4 KiB windows at
//! csi2_phys (the receiver's, the embedded channel's, the MIPI config and the PiSP front end's). The
//! receiver latches, writes frames and raises interrupts when told to.
//!
//! The in-flight state is read and written from a hard timer, so its lock is a raw spinlock (the hard
//! timer's context is not allowed a regular one). The timer runs at a line time, far below the driver's
//! stillness window, and it is pinned to the CPU that started it: the check pins its own thread to that
//! CPU, so the driver's reads and the timer share one vCPU. When the host takes that vCPU away, both
//! stop, and a late timer counts every line it missed (the overrun) at once, before the driver's next
//! read.
//!
//! Copyright (c) 2026 the Lemnos authors.

use core::{
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering},
};

use super::fixture::{
    self, lb, property_end, property_string, res_irq, res_mem, this_module, Fixed, Shared,
};
use super::{
    copy_in, copy_out, fill_phys, init_delayed_work, jiffies_of, rd, schedule_on, wr, SZ_4K,
    WORK_CPU_UNBOUND,
};
use kernel::{
    bindings,
    error::{code::*, from_err_ptr},
    ffi::{c_int, c_long, c_short, c_uint, c_ulong},
    ioctl::{_IOR, _IOW, _IOWR},
    prelude::*,
};

/// The fake receiver's request for the front end's peek: a register of its window, and its value.
#[repr(C)]
struct TcsiFePeek {
    offset: u32,
    value: u32,
}

/// The receiver's registers, as TCSI_IOC_PEEK and TCSI_IOC_PEEK_EMB read them.
#[repr(C)]
struct TcsiRegs {
    addr0: u32,
    addr1: u32,
    length: u32,
    stride: u32,
    ctrl: u32,
    frame_size: u32,
    debug: u32,
    reserved: u32,
}

// The status word's bits: a frame start and end on the image channel and the embedded one, and the
// acknowledgements of each.
const TCSI_FS: u32 = 0x1;
const TCSI_FE_ACK: u32 = 0x100;
const TCSI_EMB_FS: u32 = 0x2;
const TCSI_EMB_FE_ACK: u32 = 0x200;

const TCSI_TYPE: u32 = b'T' as u32;
/// FIRE: sets the status word the handler will read, and raises the interrupt.
const TCSI_IOC_FIRE: u32 = _IOW::<u32>(TCSI_TYPE, 1);
const TCSI_IOC_PEEK: u32 = _IOR::<TcsiRegs>(TCSI_TYPE, 2);
/// After FORCE, the channel's line counter moves once per line, a line every 50 us, for this many lines.
/// ~0: never stops.
const TCSI_IOC_BUSY: u32 = _IOW::<u32>(TCSI_TYPE, 3);
/// The lines still to come after FORCE (what TCSI_IOC_BUSY set, less those written).
const TCSI_IOC_BUSY_LEFT: u32 = _IOR::<u32>(TCSI_TYPE, 13);
/// Freezes the line counter for that many lines of the busy window: the DMA is still busy, the counter
/// is not moving.
const TCSI_IOC_BLANK: u32 = _IOW::<u32>(TCSI_TYPE, 14);
/// Arms one late FE_ACK: delivered when a wedged window (busy forever) is released with FORCE still set.
const TCSI_IOC_LATE_ACK: u32 = _IOW::<u32>(TCSI_TYPE, 15);
/// The embedded channel's registers, read as the image channel's.
const TCSI_IOC_PEEK_EMB: u32 = _IOR::<TcsiRegs>(TCSI_TYPE, 11);
/// How many embedded lines the next embedded frame end adds to the channel's line count (one when not
/// set). 0: the frame had no embedded line, and its buffer is not written.
const TCSI_IOC_EMB_LINES: u32 = _IOW::<u32>(TCSI_TYPE, 10);
/// The front end's interrupt: a start of frame latches the addresses the configuration registers hold,
/// an end of frame writes them, bounded by the configuration's own sizes, as the block would.
const TCSI_IOC_FE_FIRE: u32 = _IOW::<u32>(TCSI_TYPE, 4);
const TCSI_IOC_FE_PEEK: u32 = _IOWR::<TcsiFePeek>(TCSI_TYPE, 5);
/// Sets the front end's STATUS register directly.
const TCSI_IOC_FE_STATUS: u32 = _IOW::<u32>(TCSI_TYPE, 6);
/// STATUS reads busy for this many ms. ~0: never idle.
const TCSI_IOC_FE_BUSY: u32 = _IOW::<u32>(TCSI_TYPE, 7);
/// A frame end adds the lines the channel wrote to the line count, the frame size's height when armed
/// and none when not. LINES makes the next armed frame end add this many instead.
const TCSI_IOC_LINES: u32 = _IOW::<u32>(TCSI_TYPE, 8);
/// The front end caught mid-frame: STATUS reads queued and active until a RESET is written to CONTROL,
/// or, with TFE_ABORT_WHILE_FED, until an ABORT is written while the channel isn't forced. 0 ends it.
const TCSI_IOC_FE_MIDFRAME: u32 = _IOW::<u32>(TCSI_TYPE, 9);
const TFE_ABORT_WHILE_FED: u32 = 0x2;

/// The line time: after FORCE the counter moves once per this many nanoseconds.
const TCSI_LINE_NS: u64 = 50000;
/// The front end's registers, as the fake block shows them.
const R_STATUS: usize = 0x000;
const R_CTRL: usize = 0x028;
const R_ADDR0: usize = 0x02c;
const R_STRIDE: usize = 0x030;
/// The MIPI config's interrupt status: the same offset as the length, in its window.
const R_LENGTH: usize = 0x034;
const R_INTS: usize = 0x034;
const R_DEBUG: usize = 0x038;
const R_ADDR1: usize = 0x03c;
const R_FRAME_SIZE: usize = 0x040;
const R_FORCE: u32 = 1 << 3;
const R_INT_DMA: u32 = 1 << 0;
const R_INT_FE: u32 = 1 << 4;
/// The embedded channel's registers sit one channel stride (0x40) above the image's.
const fn r_emb(r: usize) -> usize {
    r + 0x40
}
/// The front end's registers.
const FE_R_CONTROL: usize = 0x004;
const FE_R_STATUS: usize = 0x008;
const FE_R_INT_STATUS: usize = 0x01c;
const FE_R_CONFIG: usize = 0x040;
const FE_R_INT_EOF: u32 = 1 << 0;
const FE_R_INT_SOF: u32 = 1 << 1;
const FE_R_BUSY: u32 = 0x4;
const FE_R_MIDFRAME: u32 = 0x5;
const FE_R_ABORT: u32 = 1 << 1;
const FE_R_RESET: u32 = 1 << 2;
/// CH_CTRL: the channel's FE_ACK interrupt enable, as the driver writes it.
const TCSI_IRQ_EN_FE_ACK: u32 = 1 << 15;
/// MIPI cfg INTS: the DMA's interrupt, which the driver reads.
const TCSI_R_INT_CSI_DMA: u32 = 1 << 0;

/// The four windows: the receiver's, the embedded channel's, the MIPI config and the front end's.
static TCSI_REGS: [AtomicPtr<u8>; 4] = [const { AtomicPtr::new(ptr::null_mut()) }; 4];
static TCSI_IRQ: AtomicU32 = AtomicU32::new(0);
static TCSI_PDEV: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
/// The raw spinlock of the state the timer reads and writes (DEFINE_RAW_SPINLOCK).
static TCSI_LOCK: Shared<bindings::raw_spinlock_t> =
    // SAFETY: a raw spinlock of zeros is unlocked; it is initialised by `init_statics`.
    Shared::new(unsafe { core::mem::zeroed() });
/// The hard timer that counts the lines (hrtimer_setup in `init_statics`).
static TCSI_TIMER: Shared<bindings::hrtimer> =
    // SAFETY: a timer of zeros is set up by `init_statics` before it is started.
    Shared::new(unsafe { core::mem::zeroed() });
static TCSI_CUR_ADDR: AtomicU64 = AtomicU64::new(0);
static TCSI_CUR_LEN: AtomicU32 = AtomicU32::new(0);
/// Lines the channel still writes after FORCE; ~0: forever.
static TCSI_BUSY: AtomicU32 = AtomicU32::new(0);
/// The last frame's pattern: one step per frame produced.
static TCSI_PATTERN: AtomicU8 = AtomicU8::new(0);
/// The current frame's pattern, set at its start.
static TCSI_FRAME_PATTERN: AtomicU8 = AtomicU8::new(0);
/// Of the busy lines, those the counter does not move for (TCSI_IOC_BLANK).
static TCSI_BLANK: AtomicU32 = AtomicU32::new(0);
/// This FORCE has had its frame end; cleared when FORCE is seen low.
static TCSI_ACKED: AtomicBool = AtomicBool::new(false);
/// The control register at the last tick: a write since then is seen.
static TCSI_LAST_CTRL: AtomicU32 = AtomicU32::new(0);
/// The next armed frame end's lines; 0: the height.
static TCSI_LINES: AtomicU32 = AtomicU32::new(0);
/// TCSI_IOC_LATE_ACK: one FE_ACK when a wedged DMA stops.
static TCSI_LATE: AtomicU32 = AtomicU32::new(0);
static TCSI_EMB_ADDR: AtomicU64 = AtomicU64::new(0);
static TCSI_EMB_LEN: AtomicU32 = AtomicU32::new(0);
/// The next embedded frame end's lines; 0: no line.
static TCSI_EMB_LINES: AtomicU32 = AtomicU32::new(1);

/// The front end's state: its statistics and two output buffers it latched at a start of frame.
static TFE_STATS: AtomicU64 = AtomicU64::new(0);
static TFE_OUT: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static TFE_OUT_BYTES: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];
static TFE_PATTERN: AtomicU8 = AtomicU8::new(0);
/// TCSI_IOC_FE_MIDFRAME's mode; 0: off.
static TFE_MIDFRAME: AtomicU32 = AtomicU32::new(0);

/// The front end's busy and mid-frame works (DECLARE_DELAYED_WORK in the C, set up by `init_statics`).
static TFE_BUSY_WORK: Shared<bindings::delayed_work> =
    // SAFETY: a delayed work of zeros is initialised by `init_statics` before it is queued.
    Shared::new(unsafe { core::mem::zeroed() });
static TFE_MIDFRAME_WORK: Shared<bindings::delayed_work> =
    // SAFETY: as above.
    Shared::new(unsafe { core::mem::zeroed() });

static TCSI_PROPS: Fixed<[bindings::property_entry; 2]> = Fixed([
    property_string(c"compatible", c"lemnos,test-csi2"),
    property_end(),
]);

/// `struct file_operations` of the receiver: its ioctls, with compat handled by the kernel's own.
static TCSI_FOPS: Fixed<bindings::file_operations> = Fixed(bindings::file_operations {
    owner: this_module(),
    unlocked_ioctl: Some(tcsi_ioctl),
    compat_ioctl: bindings::compat_ptr_ioctl,
    llseek: Some(bindings::noop_llseek),
    // SAFETY: every other operation absent (null), as C's designated initialiser leaves them.
    ..unsafe { core::mem::zeroed() }
});

/// The receiver's node: /dev/lemnos-test-csi2, with any free minor.
static TCSI_MISC: Shared<bindings::miscdevice> = Shared::new(bindings::miscdevice {
    minor: bindings::MISC_DYNAMIC_MINOR as c_int,
    name: c"lemnos-test-csi2".as_ptr().cast(),
    fops: TCSI_FOPS.get(),
    // SAFETY: the rest of a struct miscdevice is pointers, a list head and a mode, zero by default.
    ..unsafe { core::mem::zeroed() }
});

/// The raw spinlock of the state the timer shares, for its own interrupt context.
///
/// # Safety
///
/// `init_statics` has initialised the lock.
unsafe fn tcsi_lock() -> c_ulong {
    // SAFETY: the caller's: the lock is initialised; the flags it saved are returned to `tcsi_unlock`.
    unsafe { bindings::_raw_spin_lock_irqsave(TCSI_LOCK.get()) }
}

/// The unlock of `tcsi_lock`, with the flags it saved.
///
/// # Safety
///
/// `flags` is the value `tcsi_lock` returned, and the lock is held.
unsafe fn tcsi_unlock(flags: c_ulong) {
    // SAFETY: the caller's: the lock is held, with those flags.
    unsafe { bindings::_raw_spin_unlock_irqrestore(TCSI_LOCK.get(), flags) };
}

/// Sets up the statics that need it: the timer, the lock and the front end's works, before any device.
pub(crate) fn init_statics() {
    // SAFETY: the timer is a static this module owns, set up once before anything starts it.
    unsafe {
        bindings::hrtimer_setup(
            TCSI_TIMER.get(),
            Some(tcsi_tick),
            bindings::CLOCK_MONOTONIC as bindings::clockid_t,
            bindings::hrtimer_mode_HRTIMER_MODE_REL_PINNED_HARD,
        )
    };
    // SAFETY: the lock is a static this module owns, initialised once with its own lockdep class.
    unsafe {
        bindings::__raw_spin_lock_init(
            TCSI_LOCK.get(),
            c"tcsi_lock".as_ptr().cast(),
            kernel::static_lock_class!().as_ptr(),
            bindings::lockdep_wait_type_LD_WAIT_SPIN as c_short,
        )
    };
    // SAFETY: the works are statics this module owns, initialised once before anything queues them.
    unsafe { init_delayed_work(TFE_BUSY_WORK.get(), tfe_busy_tick, c"tfe_busy_work") };
    // SAFETY: as above.
    unsafe {
        init_delayed_work(
            TFE_MIDFRAME_WORK.get(),
            tfe_midframe_tick,
            c"tfe_midframe_work",
        )
    };
}

/// hrtimer_start(&tcsi_timer, ns_to_ktime(TCSI_LINE_NS), HRTIMER_MODE_REL_PINNED_HARD).
fn tcsi_start_timer() {
    // SAFETY: the timer is set up by `init_statics`; starting it arms it.
    unsafe {
        bindings::hrtimer_start_range_ns(
            TCSI_TIMER.get(),
            TCSI_LINE_NS as bindings::ktime_t,
            0,
            bindings::hrtimer_mode_HRTIMER_MODE_REL_PINNED_HARD,
        )
    };
}

/// Maps the receiver's four windows at `phys`, clears them, starts the timer and registers the device
/// and its node. `irq` is the receiver's interrupt. Called by init when csi2_phys names memory.
pub(crate) fn setup(phys: u64, irq: u32) -> Result {
    let mut cr: [bindings::resource; 5] =
        // SAFETY: resources of zeros, each filled in below before the device is registered.
        unsafe { core::mem::zeroed() };
    for i in 0..4 {
        let at = phys + (i as u64) * (SZ_4K as u64);
        cr[i] = res_mem(at, SZ_4K as u64);
        // SAFETY: a window of the memory the guest set aside (memmap=), mapped uncached as ioremap does.
        let regs = unsafe { bindings::ioremap(at, SZ_4K as c_ulong) };
        TCSI_REGS[i].store(regs.cast(), Ordering::Relaxed);
        if regs.is_null() {
            return Err(ENOMEM);
        }
        // SAFETY: the window just mapped is zeroed, as memset_io does.
        unsafe { bindings::memset_io(regs, 0, SZ_4K) };
    }
    // The tick reads the windows, so it starts only once they are mapped.
    tcsi_start_timer();
    TCSI_IRQ.store(irq, Ordering::Relaxed);
    cr[4] = res_irq(irq);
    let info = bindings::platform_device_info {
        name: c"lemnos-test-csi2".as_ptr().cast(),
        id: bindings::PLATFORM_DEVID_NONE,
        res: cr.as_ptr(),
        num_res: cr.len() as c_uint,
        dma_mask: u64::MAX,
        properties: TCSI_PROPS.get().cast(),
        // SAFETY: no parent, no firmware node and no data: zero.
        ..unsafe { core::mem::zeroed() }
    };
    // SAFETY: the information, its resources and its properties live for the call, which copies them.
    let pdev = from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const info) })?;
    TCSI_PDEV.store(pdev, Ordering::Relaxed);
    // SAFETY: the node is a static; misc_register is its one registration.
    let ret = unsafe { bindings::misc_register(TCSI_MISC.get()) };
    if ret != 0 {
        // SAFETY: the node is a static, not registered: its minor goes back to dynamic, as the C does.
        unsafe { (*TCSI_MISC.get()).minor = bindings::MISC_DYNAMIC_MINOR as c_int };
        return Err(Error::from_errno(ret));
    }
    Ok(())
}

/// Tears the receiver down, in the C's order: the node, the timer, the front end's works, the device,
/// and the windows. Each step is skipped when setup never reached it.
pub(crate) fn teardown() {
    let misc = TCSI_MISC.get();
    // SAFETY: the node is a static: its minor is read.
    if unsafe { (*misc).minor } != bindings::MISC_DYNAMIC_MINOR as c_int {
        // SAFETY: the node was registered by setup, and this is its one deregistration.
        unsafe { bindings::misc_deregister(misc) };
    }
    // SAFETY: the timer is set up by init: cancelling it is valid, even when never started.
    unsafe { bindings::hrtimer_cancel(TCSI_TIMER.get()) };
    // SAFETY: the work is initialised by init: cancelling it is valid, even when never queued.
    unsafe { bindings::cancel_delayed_work_sync(TFE_BUSY_WORK.get()) };
    // SAFETY: as above.
    unsafe { bindings::cancel_delayed_work_sync(TFE_MIDFRAME_WORK.get()) };
    let pdev = TCSI_PDEV.swap(ptr::null_mut(), Ordering::Relaxed);
    if !pdev.is_null() {
        // SAFETY: the device was registered by setup, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(pdev) };
    }
    for r in &TCSI_REGS {
        let regs = r.swap(ptr::null_mut(), Ordering::Relaxed);
        if !regs.is_null() {
            // SAFETY: the window was mapped by setup, and this is its one unmapping.
            unsafe { bindings::iounmap(regs.cast()) };
        }
    }
}

/// hrtimer_forward_now(t, ns_to_ktime(TCSI_LINE_NS)): the periods the timer ran past its expiry, moved on.
///
/// # Safety
///
/// `t` is the receiver's timer, set up by `init_statics`.
unsafe fn forward_now(t: *mut bindings::hrtimer) -> u64 {
    // SAFETY: the caller's: `t` is the timer, whose clock is read.
    let now = unsafe { bindings::hrtimer_cb_get_time(t.cast_const()) };
    // SAFETY: as above: the timer is forwarded by whole lines.
    unsafe { bindings::hrtimer_forward(t, now, TCSI_LINE_NS as bindings::ktime_t) }
}

/// Delivers an interrupt to the driver, in the fake's own context, as the hardware's would be taken.
///
/// FAKE-ONLY ASSUMPTION (CZ-MOD FE_ACK): the bits go into status and the DMA's interrupt into the MIPI
/// cfg, the driver's handler (devm_request_irq, a hard handler) runs to completion here, and then the fake
/// clears what it set, plus every frame bit the driver left behind. On hardware the driver's
/// write-1-to-clear does that; this fake's status is ordinary memory, so the clear is the fake's. A status
/// bit therefore exists only while the handler runs: no stale bit outlives it, and the stop can't count an
/// old one.
///
/// # Safety
///
/// The receiver is set up (its windows are mapped).
unsafe fn tcsi_deliver(status_bits: u32, ints_bits: u32) {
    let dma = TCSI_REGS[0].load(Ordering::Relaxed);
    let cfg = TCSI_REGS[2].load(Ordering::Relaxed);
    // The status starts empty: the driver's write-to-clear (START, the stop, the handler's write-back) is a
    // no-op on this memory and leaves what it wrote behind, so whatever is there is cleared first.
    // SAFETY: the caller's: the DMA window is mapped.
    unsafe { wr(dma, R_STATUS, status_bits) };
    // The handler sees only this delivery's bits: a PiSP front-end interrupt pending from TCSI_IOC_FE_FIRE
    // is taken by its own IRQ, once, and is put back as it was.
    // SAFETY: the caller's: the MIPI cfg window is mapped.
    let saved = unsafe { rd(cfg, R_INTS) };
    // SAFETY: as above.
    unsafe { wr(cfg, R_INTS, ints_bits) };
    // SAFETY: local_irq_save: the flags it saves are restored below, as the C does.
    let flags = unsafe { fixture::lemnos_test_local_irq_save() };
    // SAFETY: the receiver's interrupt is mapped by setup; the handler runs with local interrupts off.
    unsafe { bindings::generic_handle_irq(TCSI_IRQ.load(Ordering::Relaxed)) };
    // SAFETY: restores the flags saved above.
    unsafe { fixture::lemnos_test_local_irq_restore(flags) };
    // SAFETY: as above: the DMA window is mapped.
    unsafe { wr(dma, R_STATUS, 0) };
    // SAFETY: as above.
    unsafe { wr(cfg, R_INTS, saved) };
}

/// The hard timer: counts the lines a FORCE has left to write, delivers a FORCE's frame end once its busy
/// window is over, and keeps the lines the counter does not move for.
///
/// # Safety
///
/// `t` is the receiver's timer, as the hard timer calls it.
unsafe extern "C" fn tcsi_tick(t: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
    // SAFETY: the timer's own callback: `t` is the timer set up by init_statics.
    let lines = 1 + unsafe { forward_now(t) };
    let dma = TCSI_REGS[0].load(Ordering::Relaxed);
    if dma.is_null() {
        return bindings::hrtimer_restart::HRTIMER_NORESTART;
    }
    // SAFETY: the lock is initialised by init_statics; the flags are returned to the unlock below.
    let flags = unsafe { tcsi_lock() };
    // SAFETY: the DMA window is mapped by setup, and the offset is its control register.
    let ctrl = unsafe { rd(dma, R_CTRL) };
    let busy = TCSI_BUSY.load(Ordering::Relaxed);
    // SAFETY: as above.
    if busy != 0 && (unsafe { rd(dma, R_CTRL) } & R_FORCE) != 0 {
        let step = if busy == u32::MAX {
            lines.min(u64::from(u32::MAX)) as u32
        } else {
            lines.min(u64::from(busy)) as u32
        };
        let blank = step.min(TCSI_BLANK.load(Ordering::Relaxed));
        TCSI_BLANK.store(
            TCSI_BLANK.load(Ordering::Relaxed).wrapping_sub(blank),
            Ordering::Relaxed,
        );
        // The counter holds still for the blanking lines; the DMA is busy through them all.
        // SAFETY: as above: the debug register of the DMA window.
        let dbg = unsafe { rd(dma, R_DEBUG) };
        // SAFETY: as above.
        unsafe { wr(dma, R_DEBUG, dbg.wrapping_add(step).wrapping_sub(blank)) };
        if TCSI_BUSY.load(Ordering::Relaxed) != u32::MAX {
            TCSI_BUSY.store(
                TCSI_BUSY.load(Ordering::Relaxed).wrapping_sub(step),
                Ordering::Relaxed,
            );
        }
    }
    // The stop's frame end: once the busy window is over (or there was none), a FORCE with the FE_ACK
    // interrupt enabled gets its frame end, once. A FORCE with it disabled gets none, as the board showed.
    //
    // Every FORCE gets its frame end once the busy window is over, with or without a frame started: the
    // stop after a START with no frame is acknowledged too. The tick runs for the module's life, so a
    // FORCE is seen.
    //
    // The latch is per acknowledgement, not per FORCE state. A control write that changed the register since
    // the last tick (a START clears FORCE, then a stop raises it) starts a new one, so a stale latch from a
    // stop that got no acknowledgement (FORCE alone drops the enable) can't swallow the next stop's. A stop
    // with nothing in flight is acknowledged; the latch is set only when an acknowledgement is actually
    // delivered. FAKE-ONLY ASSUMPTION.
    let mut ack = false;
    if ctrl != TCSI_LAST_CTRL.load(Ordering::Relaxed) {
        TCSI_LAST_CTRL.store(ctrl, Ordering::Relaxed);
        TCSI_ACKED.store(false, Ordering::Relaxed);
    }
    if (ctrl & R_FORCE) == 0 {
        TCSI_ACKED.store(false, Ordering::Relaxed);
    } else if TCSI_BUSY.load(Ordering::Relaxed) == 0
        && !TCSI_ACKED.load(Ordering::Relaxed)
        && (ctrl & TCSI_IRQ_EN_FE_ACK) != 0
    {
        TCSI_ACKED.store(true, Ordering::Relaxed);
        ack = true;
    }
    // SAFETY: the lock taken above, with the flags it saved.
    unsafe { tcsi_unlock(flags) };
    // The stop's frame end, delivered with the lock released: the driver's handler may run to completion.
    if ack {
        let mut emb = 0;
        // SAFETY: the DMA window is mapped; the embedded channel's control register is in it.
        if (unsafe { rd(dma, r_emb(R_CTRL)) } & (R_FORCE | TCSI_IRQ_EN_FE_ACK))
            == (R_FORCE | TCSI_IRQ_EN_FE_ACK)
        {
            emb = TCSI_EMB_FE_ACK;
        }
        // SAFETY: as above: the delivery runs in the timer's own context, with the lock released.
        unsafe { tcsi_deliver(TCSI_FE_ACK | emb, TCSI_R_INT_CSI_DMA) };
    }
    bindings::hrtimer_restart::HRTIMER_RESTART
}

/// The front end's window: its config's register at `offset` from `FE_R_CONFIG`, as a 64-bit address.
///
/// # Safety
///
/// The front end's window is mapped by setup.
unsafe fn tfe_read_addr(fe: *mut u8, offset: usize) -> u64 {
    // SAFETY: the caller's: the high word, inside the window's config.
    let hi = unsafe { rd(fe, FE_R_CONFIG + offset + 4) };
    // SAFETY: the caller's: the low word, inside the window's config.
    let lo = unsafe { rd(fe, FE_R_CONFIG + offset) };
    (u64::from(hi) << 32) | u64::from(lo)
}

/// The image's bytes for output `i`: its format's stride times its height, or none when the stride is not
/// positive.
///
/// # Safety
///
/// The front end's window is mapped by setup.
unsafe fn tfe_read_out_bytes(fe: *mut u8, i: usize) -> u32 {
    let at = offset_of!(lb::pisp_fe_config, ch)
        + i * size_of::<lb::pisp_fe_output_branch_config>()
        + offset_of!(lb::pisp_fe_output_branch_config, output)
        + offset_of!(lb::pisp_fe_output_config, format);
    let mut f = core::mem::MaybeUninit::<lb::pisp_image_format_config>::uninit();
    // SAFETY: memcpy_fromio(&f, fe + FE_R_CONFIG + at, sizeof f): the format is inside the config, which is in the window.
    unsafe {
        bindings::memcpy_fromio(
            f.as_mut_ptr().cast(),
            fe.wrapping_add(FE_R_CONFIG + at).cast(),
            size_of::<lb::pisp_image_format_config>(),
        )
    };
    // SAFETY: the copy above wrote every byte of `f`.
    let f = unsafe { f.assume_init() };
    if f.stride <= 0 {
        return 0;
    }
    (f.stride as u32).wrapping_mul(u32::from(f.height))
}

/// The front end's interrupt: a start of frame latches the buffers it was given, an end of frame fills
/// them with the frame's pattern (the statistics too, when they were latched).
///
/// # Safety
///
/// The front end's window and the receiver's MIPI config are mapped by setup.
unsafe fn tfe_fire(status: u32) -> c_long {
    let fe = TCSI_REGS[3].load(Ordering::Relaxed);
    let cfg = TCSI_REGS[2].load(Ordering::Relaxed);
    if (status & FE_R_INT_SOF) != 0 {
        // SAFETY: the caller's: the front end's window is mapped.
        TFE_STATS.store(
            unsafe { tfe_read_addr(fe, offset_of!(lb::pisp_fe_config, stats_buffer)) },
            Ordering::Relaxed,
        );
        // SAFETY: as above.
        TFE_OUT[0].store(
            unsafe { tfe_read_addr(fe, offset_of!(lb::pisp_fe_config, output_buffer)) },
            Ordering::Relaxed,
        );
        // SAFETY: as above: the second buffer's entry follows the first.
        TFE_OUT[1].store(
            unsafe {
                tfe_read_addr(
                    fe,
                    offset_of!(lb::pisp_fe_config, output_buffer)
                        + size_of::<lb::pisp_fe_output_buffer_config>(),
                )
            },
            Ordering::Relaxed,
        );
        // SAFETY: as above.
        TFE_OUT_BYTES[0].store(unsafe { tfe_read_out_bytes(fe, 0) }, Ordering::Relaxed);
        // SAFETY: as above.
        TFE_OUT_BYTES[1].store(unsafe { tfe_read_out_bytes(fe, 1) }, Ordering::Relaxed);
    }
    if (status & FE_R_INT_EOF) != 0 {
        let pat = TFE_PATTERN.load(Ordering::Relaxed).wrapping_add(1);
        TFE_PATTERN.store(pat, Ordering::Relaxed);
        for i in 0..2 {
            let out = TFE_OUT[i].load(Ordering::Relaxed);
            let bytes = TFE_OUT_BYTES[i].load(Ordering::Relaxed);
            if out != 0 && bytes != 0 {
                // SAFETY: an address the front end latched at its start of frame: a page the kernel has,
                // with `bytes` in it, as the block would have been given it.
                unsafe { fill_phys(out, pat, bytes as usize) };
            }
        }
        let stats = TFE_STATS.load(Ordering::Relaxed);
        if stats != 0 {
            // SAFETY: as above: the statistics buffer, a whole struct in its page.
            unsafe { fill_phys(stats, pat, size_of::<lb::pisp_statistics>()) };
        }
        TFE_STATS.store(0, Ordering::Relaxed);
        TFE_OUT[0].store(0, Ordering::Relaxed);
        TFE_OUT[1].store(0, Ordering::Relaxed);
    }
    // SAFETY: the front end's window is mapped.
    unsafe { wr(fe, FE_R_INT_STATUS, status) };
    // SAFETY: the MIPI config is mapped.
    unsafe { wr(cfg, R_INTS, R_INT_FE) };
    // SAFETY: the receiver's interrupt is mapped by setup.
    let ret = unsafe {
        bindings::irq_set_irqchip_state(
            TCSI_IRQ.load(Ordering::Relaxed),
            bindings::irqchip_irq_state_IRQCHIP_STATE_PENDING,
            true,
        )
    };
    ret as c_long
}

/// The receiver's ioctls, as the C's chain answers them: each command's copy and its result.
///
/// # Safety
///
/// The receiver is set up (its windows are mapped), as the node is open only then.
unsafe fn tcsi_cmd(cmd: c_uint, arg: c_ulong) -> kernel::error::Result<c_long> {
    let dma = TCSI_REGS[0].load(Ordering::Relaxed);
    let fe = TCSI_REGS[3].load(Ordering::Relaxed);
    // The DMA window's registers: the caller's windows are mapped (the caller's promise).
    let dr = |off: usize| -> u32 {
        // SAFETY: the DMA window is mapped by setup, and `off` is a register of it.
        unsafe { rd(dma, off) }
    };
    let dw = |off: usize, v: u32| {
        // SAFETY: as above.
        unsafe { wr(dma, off, v) };
    };
    let fw = |off: usize, v: u32| {
        // SAFETY: the front end's window is mapped by setup, and `off` is a register of it.
        unsafe { wr(fe, off, v) };
    };
    match cmd {
        TCSI_IOC_PEEK_EMB => {
            let r = TcsiRegs {
                addr0: dr(r_emb(R_ADDR0)),
                addr1: dr(r_emb(R_ADDR1)),
                length: dr(r_emb(R_LENGTH)),
                stride: dr(r_emb(R_STRIDE)),
                ctrl: dr(r_emb(R_CTRL)),
                frame_size: dr(r_emb(R_FRAME_SIZE)),
                debug: dr(r_emb(R_DEBUG)),
                reserved: 0,
            };
            // SAFETY: `arg` is the caller's user address.
            unsafe { copy_out(arg, &r)? };
            Ok(0)
        }
        TCSI_IOC_PEEK => {
            let r = TcsiRegs {
                addr0: dr(R_ADDR0),
                addr1: dr(R_ADDR1),
                length: dr(R_LENGTH),
                stride: dr(R_STRIDE),
                ctrl: dr(R_CTRL),
                frame_size: dr(R_FRAME_SIZE),
                debug: dr(R_DEBUG),
                reserved: 0,
            };
            // SAFETY: `arg` is the caller's user address.
            unsafe { copy_out(arg, &r)? };
            Ok(0)
        }
        TCSI_IOC_FE_PEEK => {
            if fe.is_null() {
                return Err(ENODEV);
            }
            // SAFETY: `arg` is the caller's user address.
            let mut p: TcsiFePeek = unsafe { copy_in(arg)? };
            if p.offset > (SZ_4K as u32) - 4 {
                return Err(EINVAL);
            }
            // SAFETY: the front end's window is mapped; `p.offset` is inside it (checked above).
            p.value = unsafe { rd(fe, p.offset as usize) };
            // SAFETY: as above: the caller's user address.
            unsafe { copy_out(arg, &p)? };
            Ok(0)
        }
        TCSI_IOC_FE_STATUS => {
            // SAFETY: `arg` is the caller's user address.
            let v: u32 = unsafe { copy_in(arg)? };
            fw(FE_R_STATUS, v);
            Ok(0)
        }
        TCSI_IOC_FE_BUSY => {
            // SAFETY: `arg` is the caller's user address.
            let ms: u32 = unsafe { copy_in(arg)? };
            fw(FE_R_STATUS, FE_R_BUSY);
            // SAFETY: the work is initialised by init_statics.
            unsafe { bindings::cancel_delayed_work(TFE_BUSY_WORK.get()) };
            if ms != u32::MAX {
                // SAFETY: as above; the work is queued on the system workqueue.
                unsafe { schedule_on(WORK_CPU_UNBOUND, TFE_BUSY_WORK.get(), jiffies_of(ms)) };
            }
            Ok(0)
        }
        TCSI_IOC_FE_MIDFRAME => {
            // SAFETY: `arg` is the caller's user address.
            let mode: u32 = unsafe { copy_in(arg)? };
            // SAFETY: the work is initialised by init_statics.
            unsafe { bindings::cancel_delayed_work_sync(TFE_MIDFRAME_WORK.get()) };
            TFE_MIDFRAME.store(mode, Ordering::Relaxed);
            // Whatever CONTROL held before this frame is history.
            fw(FE_R_CONTROL, 0);
            fw(FE_R_STATUS, if mode != 0 { FE_R_MIDFRAME } else { 0 });
            if mode != 0 {
                // SAFETY: as above.
                unsafe { schedule_on(WORK_CPU_UNBOUND, TFE_MIDFRAME_WORK.get(), jiffies_of(1)) };
            }
            Ok(0)
        }
        TCSI_IOC_FE_FIRE => {
            // SAFETY: `arg` is the caller's user address.
            let status: u32 = unsafe { copy_in(arg)? };
            // SAFETY: the front end and the MIPI config are mapped by setup.
            Ok(unsafe { tfe_fire(status) })
        }
        TCSI_IOC_LATE_ACK => {
            // SAFETY: `arg` is the caller's user address.
            let v: u32 = unsafe { copy_in(arg)? };
            TCSI_LATE.store(v, Ordering::Relaxed);
            Ok(0)
        }
        TCSI_IOC_LINES => {
            // SAFETY: `arg` is the caller's user address.
            let v: u32 = unsafe { copy_in(arg)? };
            TCSI_LINES.store(v, Ordering::Relaxed);
            Ok(0)
        }
        TCSI_IOC_EMB_LINES => {
            // SAFETY: `arg` is the caller's user address.
            let v: u32 = unsafe { copy_in(arg)? };
            TCSI_EMB_LINES.store(v, Ordering::Relaxed);
            Ok(0)
        }
        TCSI_IOC_BUSY => {
            // SAFETY: `arg` is the caller's user address.
            let lines: u32 = unsafe { copy_in(arg)? };
            // SAFETY: the lock is initialised by init_statics.
            let flags = unsafe { tcsi_lock() };
            let mut late_ack = false;
            // A release of a wedged window (busy forever) ends a stop that already returned wedged, with no
            // frame end: the FORCE it left set is settled here, so the tick never acks it. FAKE-ONLY ASSUMPTION.
            if TCSI_BUSY.load(Ordering::Relaxed) == u32::MAX
                && lines == 0
                && (dr(R_CTRL) & R_FORCE) != 0
            {
                if TCSI_LATE.load(Ordering::Relaxed) != 0 {
                    // LATE: the wedged DMA stops only now, after its stop gave up. One FE_ACK is delivered,
                    // as the hardware would raise it for the FORCE that is still set. FAKE-ONLY ASSUMPTION.
                    TCSI_LATE.store(0, Ordering::Relaxed);
                    late_ack = true;
                }
                TCSI_ACKED.store(true, Ordering::Relaxed);
            }
            TCSI_BUSY.store(lines, Ordering::Relaxed);
            // SAFETY: the lock taken above, with the flags it saved.
            unsafe { tcsi_unlock(flags) };
            if late_ack {
                // SAFETY: the receiver is set up.
                unsafe { tcsi_deliver(TCSI_FE_ACK, TCSI_R_INT_CSI_DMA) };
            }
            if lines != 0 {
                tcsi_start_timer();
            }
            Ok(0)
        }
        TCSI_IOC_BLANK => {
            // SAFETY: `arg` is the caller's user address.
            let lines: u32 = unsafe { copy_in(arg)? };
            // SAFETY: the lock is initialised by init_statics.
            let flags = unsafe { tcsi_lock() };
            TCSI_BLANK.store(lines, Ordering::Relaxed);
            // SAFETY: the lock taken above, with the flags it saved.
            unsafe { tcsi_unlock(flags) };
            Ok(0)
        }
        TCSI_IOC_BUSY_LEFT => {
            // SAFETY: the lock is initialised by init_statics.
            let flags = unsafe { tcsi_lock() };
            let left = TCSI_BUSY.load(Ordering::Relaxed);
            // SAFETY: the lock taken above, with the flags it saved.
            unsafe { tcsi_unlock(flags) };
            // SAFETY: `arg` is the caller's user address.
            unsafe { copy_out(arg, &left)? };
            Ok(0)
        }
        TCSI_IOC_FIRE => {
            // SAFETY: `arg` is the caller's user address.
            let status: u32 = unsafe { copy_in(arg)? };
            if (status & TCSI_FS) != 0 {
                // A real sensor makes a frame whether or not a buffer is armed: the pattern is the frame's,
                // taken here, so a frame with no buffer still counts (the driver's sequence number counts it too).
                let pat = TCSI_PATTERN.load(Ordering::Relaxed).wrapping_add(1);
                TCSI_PATTERN.store(pat, Ordering::Relaxed);
                TCSI_FRAME_PATTERN.store(pat, Ordering::Relaxed);
                let hi = dr(R_ADDR1);
                let lo = dr(R_ADDR0);
                TCSI_CUR_ADDR.store(
                    (u64::from(hi) << 36) | (u64::from(lo) << 4),
                    Ordering::Relaxed,
                );
                TCSI_CUR_LEN.store(dr(R_LENGTH) << 4, Ordering::Relaxed);
                // SAFETY: the lock is initialised by init_statics.
                let flags = unsafe { tcsi_lock() };
                // SAFETY: the lock taken above, with the flags it saved.
                unsafe { tcsi_unlock(flags) };
            }
            if (status & TCSI_EMB_FS) != 0 {
                let hi = dr(r_emb(R_ADDR1));
                let lo = dr(r_emb(R_ADDR0));
                TCSI_EMB_ADDR.store(
                    (u64::from(hi) << 36) | (u64::from(lo) << 4),
                    Ordering::Relaxed,
                );
                TCSI_EMB_LEN.store(dr(r_emb(R_LENGTH)) << 4, Ordering::Relaxed);
            }
            let emb_addr = TCSI_EMB_ADDR.load(Ordering::Relaxed);
            if (status & TCSI_EMB_FE_ACK) != 0 && emb_addr != 0 {
                let dbg = dr(r_emb(R_DEBUG));
                let lines = TCSI_EMB_LINES.load(Ordering::Relaxed);
                if lines != 0 {
                    // SAFETY: the embedded buffer the channel latched at its start of frame.
                    unsafe {
                        fill_phys(
                            emb_addr,
                            TCSI_FRAME_PATTERN.load(Ordering::Relaxed),
                            TCSI_EMB_LEN.load(Ordering::Relaxed) as usize,
                        )
                    };
                }
                TCSI_EMB_ADDR.store(0, Ordering::Relaxed);
                TCSI_EMB_LINES.store(1, Ordering::Relaxed);
                dw(
                    r_emb(R_DEBUG),
                    (((dbg >> 16).wrapping_add(1)) << 16) | (dbg.wrapping_add(lines) & 0xffff),
                );
            }
            let cur_addr = TCSI_CUR_ADDR.load(Ordering::Relaxed);
            if (status & TCSI_FE_ACK) != 0 && cur_addr != 0 {
                let dbg = dr(R_DEBUG);
                let armed = TCSI_LINES.load(Ordering::Relaxed);
                let lines = if armed != 0 {
                    armed
                } else {
                    dr(R_FRAME_SIZE) >> 16
                };
                // SAFETY: the buffer the channel latched at its start of frame, with its length.
                unsafe {
                    fill_phys(
                        cur_addr,
                        TCSI_FRAME_PATTERN.load(Ordering::Relaxed),
                        TCSI_CUR_LEN.load(Ordering::Relaxed) as usize,
                    )
                };
                TCSI_CUR_ADDR.store(0, Ordering::Relaxed);
                TCSI_LINES.store(0, Ordering::Relaxed);
                // Frames in the high half, lines in the low, each wrapping.
                dw(
                    R_DEBUG,
                    (((dbg >> 16).wrapping_add(1)) << 16) | (dbg.wrapping_add(lines) & 0xffff),
                );
            }
            // SAFETY: the receiver is set up.
            unsafe { tcsi_deliver(status, R_INT_DMA) };
            Ok(0)
        }
        _ => Err(ENOTTY),
    }
}

/// The receiver's ioctl entry: the chain's answer, or its errno.
///
/// # Safety
///
/// Reached only through the node, which setup registers after the windows are mapped.
unsafe extern "C" fn tcsi_ioctl(_file: *mut bindings::file, cmd: c_uint, arg: c_ulong) -> c_long {
    // SAFETY: the node is registered only once the receiver's windows are mapped.
    match unsafe { tcsi_cmd(cmd, arg) } {
        Ok(v) => v,
        Err(e) => e.to_errno() as c_long,
    }
}

/// The front end's busy window ends: its status reads idle again (TCSI_IOC_FE_BUSY).
unsafe extern "C" fn tfe_busy_tick(_work: *mut bindings::work_struct) {
    let fe = TCSI_REGS[3].load(Ordering::Relaxed);
    // SAFETY: the front end's window is mapped by setup, and its status is cleared.
    unsafe { wr(fe, FE_R_STATUS, 0) };
}

/// The mid-frame state: its CONTROL is looked at every ms, and a reset or an abort (while fed) ends it.
unsafe extern "C" fn tfe_midframe_tick(_work: *mut bindings::work_struct) {
    let fe = TCSI_REGS[3].load(Ordering::Relaxed);
    let dma = TCSI_REGS[0].load(Ordering::Relaxed);
    // SAFETY: the front end's window is mapped by setup, and CONTROL is its register.
    let ctrl = unsafe { rd(fe, FE_R_CONTROL) };
    // SAFETY: the DMA window is mapped by setup, and its control register is read.
    let fed = (unsafe { rd(dma, R_CTRL) } & R_FORCE) == 0;
    let mode = TFE_MIDFRAME.load(Ordering::Relaxed);
    if (ctrl & FE_R_RESET) != 0
        || ((mode & TFE_ABORT_WHILE_FED) != 0 && (ctrl & FE_R_ABORT) != 0 && fed)
    {
        // SAFETY: as above: the status is cleared.
        unsafe { wr(fe, FE_R_STATUS, 0) };
        TFE_MIDFRAME.store(0, Ordering::Relaxed);
        return;
    }
    if mode != 0 {
        // SAFETY: the work is initialised by init_statics; it is queued again for the next ms.
        unsafe { schedule_on(WORK_CPU_UNBOUND, TFE_MIDFRAME_WORK.get(), jiffies_of(1)) };
    }
}
