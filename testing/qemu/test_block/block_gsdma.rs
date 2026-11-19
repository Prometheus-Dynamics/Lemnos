// SPDX-License-Identifier: GPL-2.0
//! The fake K230 graphics DMA engine. Its registers are a page of ordinary memory, looked at every
//! millisecond while a driver has the channel on. A 1 in CTRL starts the chain at LLI_BASE, which is
//! walked as the engine would, copying each plane through the kernel's mapping of the bus addresses it
//! names (no IOMMU in this guest: a bus address is physical), then DONE is set and the interrupt
//! raised. With `hang` set, a started chain never finishes and its progress register keeps moving, for
//! `hang` ms after the channel is turned off (~0: forever).
//!
//! The tick runs on CPU 0, where the checks pin themselves, so the engine's progress and the driver's
//! reads share a vCPU (see block_csi2.rs). It counts the milliseconds that really passed since the last
//! tick, so a late tick catches up.
//!
//! Copyright (c) 2026 the Lemnos authors.

use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, Ordering},
};

use super::fixture::{self, dev_attr, property_end, property_string, Fixed, Shared};
use super::{
    alloc_block_page, device_info, free_block_page, init_delayed_work, jiffies_of, lock, mem_copy,
    mem_ld32, rd, schedule_on, unlock, wr, PAGE_SIZE, TB_GSDMA_IRQ, TB_IRQ,
};
use kernel::{
    bindings,
    error::{code::*, from_err_ptr, to_result},
    ffi::{c_int, c_uint, c_ulong},
    prelude::*,
};

const TGD_CH_EN: usize = 0x00;
const TGD_CH_EN_BIT: u32 = 1 << 4;
const TGD_INT_STAT: usize = 0x08;
const TGD_CTRL: usize = 0x10;
const TGD_LLI_BASE: usize = 0x14;
const TGD_CUR_DADDR: usize = 0x44;
const TGD_DONE: u32 = 1 << 18;
/// The CPU the tick runs on: the checks pin themselves there.
const TGD_CPU: c_int = 0;

/// The engine's register window, and the page it is in (freed at teardown).
static TGD_REGS: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
static TGD_PAGE: AtomicPtr<bindings::page> = AtomicPtr::new(ptr::null_mut());
static TGD_PDEV: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
static TGD_MUTEX: Shared<bindings::mutex> =
    // SAFETY: a mutex of zeros is unlocked; it is initialised by `init_statics`.
    Shared::new(unsafe { core::mem::zeroed() });
/// The tick (DECLARE_DELAYED_WORK in the C, set up by `init_statics`).
static TGD_WORK: Shared<bindings::delayed_work> =
    // SAFETY: a delayed work of zeros is initialised by `init_statics` before it is queued.
    Shared::new(unsafe { core::mem::zeroed() });
/// `hang`: jobs from now on hang, for this many ms after the channel is off; ~0: forever.
static TGD_HANG: AtomicU32 = AtomicU32::new(0);
/// The ms a hung chain still moves for, once the channel is off.
static TGD_LEFT: AtomicU32 = AtomicU32::new(0);
static TGD_JOBS: AtomicU32 = AtomicU32::new(0);
/// The first words of the last descriptors the chain ran.
static TGD_WORD0: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
/// Addresses the engine could not reach.
static TGD_ERRORS: AtomicU32 = AtomicU32::new(0);
static TGD_HANGING: AtomicBool = AtomicBool::new(false);
/// The time of the last tick, in ns.
static TGD_LAST: AtomicI64 = AtomicI64::new(0);

static TGD_PROPS: Fixed<[bindings::property_entry; 2]> = Fixed([
    property_string(c"compatible", c"lemnos,test-gsdma"),
    property_end(),
]);

static DEV_ATTR_HANG: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"hang", 0o200, None, Some(hang_store)));
static DEV_ATTR_RAN: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"ran", 0o444, Some(ran_show), None));

static TGD_ATTRS: Fixed<[*mut bindings::attribute; 3]> = Fixed([
    // The address of each attribute's `attr`, its first field.
    DEV_ATTR_HANG.get().cast(),
    DEV_ATTR_RAN.get().cast(),
    ptr::null_mut(),
]);
static TGD_GROUP: Fixed<bindings::attribute_group> = Fixed(bindings::attribute_group {
    __bindgen_anon_2: bindings::attribute_group__bindgen_ty_2 {
        attrs: TGD_ATTRS.get().cast_mut().cast(),
    },
    // SAFETY: no name and no visibility callbacks: zero.
    ..unsafe { core::mem::zeroed() }
});

/// Sets up the mutex and the tick, before any device.
pub(crate) fn init_statics() {
    // SAFETY: the mutex is a static this module owns, initialised once with its own lockdep class.
    unsafe {
        bindings::__mutex_init(
            TGD_MUTEX.get(),
            c"tgd_lock".as_ptr().cast(),
            kernel::static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: the work is a static this module owns, initialised once before anything queues it.
    unsafe { init_delayed_work(TGD_WORK.get(), tgd_tick, c"tgd_work") };
}

/// The engine's register at `off`.
///
/// # Safety
///
/// The window is mapped by `setup`, until teardown.
unsafe fn tgd_rd(off: usize) -> u32 {
    // SAFETY: the caller's: the window is mapped, and `off` is one of its registers.
    unsafe { rd(TGD_REGS.load(Ordering::Relaxed), off) }
}

/// The engine's register at `off`, written.
///
/// # Safety
///
/// As `tgd_rd`.
unsafe fn tgd_wr(off: usize, v: u32) {
    // SAFETY: the caller's: the window is mapped, and `off` is one of its registers.
    unsafe { wr(TGD_REGS.load(Ordering::Relaxed), off, v) };
}

/// The kernel's pointer for a bus address the engine reads, or NULL when it cannot be reached. A page
/// that is free has no references, so one whose count is zero was freed (a stale descriptor, the bug
/// KASAN found): the fake refuses it, as the block would refuse an unmapped address, and says so in the log.
///
/// # Safety
///
/// Nothing: the address is only looked at, and the caller reads the memory it names only when non-NULL.
unsafe fn tgd_at(bus: u64) -> *mut u8 {
    let pfn = bus >> bindings::PAGE_SHIFT;
    // SAFETY: pfn_valid is any number's question.
    if unsafe { fixture::lemnos_test_pfn_valid(pfn as c_ulong) } == 0 {
        return ptr::null_mut();
    }
    // SAFETY: the page number is valid (checked above): its count is read.
    if unsafe { fixture::page_ref_count(pfn as c_ulong) } == 0 {
        pr_err!(
            "lemnos-test-block: gsdma descriptor reads a freed page at {:x}\n",
            bus
        );
        return ptr::null_mut();
    }
    // SAFETY: the page is one the kernel has (checked above): its kernel mapping is the address.
    unsafe { fixture::lemnos_test_phys_to_virt(bus) }.cast()
}

/// One descriptor: its plane, mirrored, then turned clockwise.
///
/// # Safety
///
/// `d` is a descriptor `tgd_at` returned: six words of readable memory.
unsafe fn tgd_plane(d: *const u8) {
    const BPE_OF: [u32; 4] = [1, 2, 3, 4];
    // SAFETY: the caller's: `d` is six readable words, and each read is one of them.
    let ld = |off: usize| unsafe { mem_ld32(d, off) };
    let (d0, d1, d2, d3, d4) = (ld(0), ld(4), ld(8), ld(12), ld(16));
    let turn = d0 & 3;
    let bpe = BPE_OF[((d0 >> 8) & 3) as usize];
    let mx = (d0 & (1 << 2)) != 0;
    let my = (d0 & (1 << 3)) != 0;
    let w = d2 & 0xffff;
    let h = d2 >> 16;
    let sp = d3 & 0xffff;
    let dp = d3 >> 16;
    // SAFETY: the bus addresses are the descriptor's; each is checked by `tgd_at`.
    let src = unsafe { tgd_at(u64::from(d1)) };
    // SAFETY: as above.
    let dst = unsafe { tgd_at(u64::from(d4)) };
    if src.is_null() || dst.is_null() {
        TGD_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let ax = if mx { w - 1 - x } else { x };
            let ay = if my { h - 1 - y } else { y };
            let (dx, dy) = match turn {
                0 => (ax, ay),
                1 => (h - 1 - ay, ax),
                2 => (w - 1 - ax, h - 1 - ay),
                _ => (ay, w - 1 - ax),
            };
            let to = (u64::from(dy) * u64::from(dp) + u64::from(dx) * u64::from(bpe)) as usize;
            let from = (u64::from(y) * u64::from(sp) + u64::from(x) * u64::from(bpe)) as usize;
            // SAFETY: both planes are inside the pages the descriptor names (checked by `tgd_at`), and
            // the two pages are different pages the chain made of distinct buffers.
            unsafe { mem_copy(src.wrapping_add(from), dst.wrapping_add(to), bpe as usize) };
        }
    }
}

/// Runs the chain at LLI_BASE: up to three descriptors, each a plane, until one ends it.
///
/// # Safety
///
/// Called under `TGD_MUTEX`, with the window mapped.
unsafe fn tgd_run() {
    // SAFETY: the caller's: the window is mapped.
    let mut at = u64::from(unsafe { tgd_rd(TGD_LLI_BASE) });
    let mut i = 0usize;
    while i < 3 && at != 0 {
        // SAFETY: the address is the chain's; `tgd_at` checks it.
        let d = unsafe { tgd_at(at) };
        if d.is_null() {
            TGD_ERRORS.fetch_add(1, Ordering::Relaxed);
            break;
        }
        // SAFETY: `d` is a descriptor `tgd_at` returned: six words, read as the engine reads them.
        let d0 = unsafe { mem_ld32(d, 0) };
        TGD_WORD0[i].store(d0, Ordering::Relaxed);
        // SAFETY: as above.
        unsafe { tgd_plane(d) };
        if (d0 & (1 << 30)) != 0 {
            break;
        }
        // SAFETY: as above: the word after the plane is the next descriptor's address.
        at = u64::from(unsafe { mem_ld32(d, 20) });
        i += 1;
    }
    TGD_JOBS.fetch_add(1, Ordering::Relaxed);
}

/// The tick: starts a chain when the channel is on, moves a hung one's progress, and says whether the
/// interrupt is due. It runs every ms while a driver has the channel on, every 50 ms otherwise.
///
/// # Safety
///
/// Called by the kernel's workqueue, for the work `init_statics` set up.
unsafe extern "C" fn tgd_tick(_work: *mut bindings::work_struct) {
    // SAFETY: the kernel's clock, read.
    let now = unsafe { bindings::ktime_get() };
    // SAFETY: the mutex is initialised by init_statics; it is released below.
    unsafe { lock(TGD_MUTEX.get()) };
    // Whole milliseconds since the last tick, at least one: a tick that ran late counts what it missed.
    let last = TGD_LAST.load(Ordering::Relaxed);
    // SAFETY: a pure conversion of the difference of two times.
    let steps = unsafe { bindings::ktime_to_ms(now - last) }.max(1) as u32;
    TGD_LAST.store(now, Ordering::Relaxed);
    let mut raise = false;
    // SAFETY: the window is mapped by setup.
    if unsafe { tgd_rd(TGD_CTRL) } == 1 && !TGD_HANGING.load(Ordering::Relaxed) {
        // SAFETY: as above.
        unsafe { tgd_wr(TGD_CTRL, 0) };
        // SAFETY: as above.
        unsafe { tgd_wr(TGD_INT_STAT, 0) };
        let hang = TGD_HANG.load(Ordering::Relaxed);
        if hang != 0 {
            TGD_HANGING.store(true, Ordering::Relaxed);
            TGD_LEFT.store(hang, Ordering::Relaxed);
        } else {
            // SAFETY: called under the mutex, with the window mapped.
            unsafe { tgd_run() };
            // SAFETY: as above.
            unsafe { tgd_wr(TGD_INT_STAT, TGD_DONE) };
            raise = true;
        }
    }
    if TGD_HANGING.load(Ordering::Relaxed) {
        // SAFETY: as above.
        let en = unsafe { tgd_rd(TGD_CH_EN) } & TGD_CH_EN_BIT;
        let left = TGD_LEFT.load(Ordering::Relaxed);
        // It moves while on; once off, for `left` ms more.
        if en == 0 && left != u32::MAX {
            if left <= steps {
                TGD_LEFT.store(0, Ordering::Relaxed);
                TGD_HANGING.store(false, Ordering::Relaxed);
            } else {
                TGD_LEFT.store(left - steps, Ordering::Relaxed);
                // SAFETY: as above.
                let cur = unsafe { tgd_rd(TGD_CUR_DADDR) };
                // SAFETY: as above.
                unsafe { tgd_wr(TGD_CUR_DADDR, cur.wrapping_add(steps)) };
            }
        } else {
            // SAFETY: as above.
            let cur = unsafe { tgd_rd(TGD_CUR_DADDR) };
            // SAFETY: as above.
            unsafe { tgd_wr(TGD_CUR_DADDR, cur.wrapping_add(steps)) };
        }
    }
    // Looked at every ms only while a driver has the channel on, so it costs nothing otherwise.
    // SAFETY: as above.
    let on =
        TGD_HANGING.load(Ordering::Relaxed) || (unsafe { tgd_rd(TGD_CH_EN) } & TGD_CH_EN_BIT) != 0;
    // SAFETY: the mutex taken above.
    unsafe { unlock(TGD_MUTEX.get()) };
    if raise {
        // SAFETY: the interrupt is one of the simulated domain's, mapped until teardown.
        unsafe {
            bindings::irq_set_irqchip_state(
                TB_IRQ[TB_GSDMA_IRQ].load(Ordering::Relaxed),
                bindings::irqchip_irq_state_IRQCHIP_STATE_PENDING,
                true,
            )
        };
    }
    // SAFETY: the work is initialised by init_statics; it is queued again.
    unsafe { schedule_on(TGD_CPU, TGD_WORK.get(), jiffies_of(if on { 1 } else { 50 })) };
}

/// "N": jobs from now on hang, moving for N ms after the channel is off (4294967295: forever).
unsafe extern "C" fn hang_store(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *const kernel::ffi::c_char,
    count: usize,
) -> isize {
    let mut v: c_uint = 0;
    // SAFETY: sysfs passes the store's text, NUL-terminated; `v` is written on success.
    if unsafe { bindings::kstrtouint(buf, 0, &raw mut v) } != 0 {
        return EINVAL.to_errno() as isize;
    }
    // SAFETY: the mutex is initialised by init_statics.
    unsafe { lock(TGD_MUTEX.get()) };
    TGD_HANG.store(v, Ordering::Relaxed);
    // SAFETY: the mutex taken above.
    unsafe { unlock(TGD_MUTEX.get()) };
    count as isize
}

/// "jobs errors word0 word0 word0": chains run, addresses it couldn't reach, the last descriptors' first words.
unsafe extern "C" fn ran_show(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *mut kernel::ffi::c_char,
) -> isize {
    // SAFETY: the mutex is initialised by init_statics.
    unsafe { lock(TGD_MUTEX.get()) };
    // SAFETY: sysfs passes a page for the text.
    let n = unsafe {
        bindings::sysfs_emit(
            buf,
            c"%u %u %x %x %x\n".as_ptr().cast(),
            TGD_JOBS.load(Ordering::Relaxed) as c_uint,
            TGD_ERRORS.load(Ordering::Relaxed) as c_uint,
            TGD_WORD0[0].load(Ordering::Relaxed) as c_uint,
            TGD_WORD0[1].load(Ordering::Relaxed) as c_uint,
            TGD_WORD0[2].load(Ordering::Relaxed) as c_uint,
        )
    } as isize;
    // SAFETY: the mutex taken above.
    unsafe { unlock(TGD_MUTEX.get()) };
    n
}

/// Registers the engine: a page of its registers, its interrupt, the device and its node, and the first tick.
pub(crate) fn setup() -> Result {
    let page = alloc_block_page(0)?;
    TGD_PAGE.store(page, Ordering::Relaxed);
    // SAFETY: page_address of the page just allocated: the window of the engine's registers.
    TGD_REGS.store(
        unsafe { fixture::lemnos_test_page_address(page) }.cast(),
        Ordering::Relaxed,
    );
    // SAFETY: page_to_phys of the page just allocated.
    let phys = unsafe { fixture::lemnos_test_page_to_phys(page) };
    let res = [
        fixture::res_mem(phys, PAGE_SIZE as u64),
        fixture::res_irq(TB_IRQ[TB_GSDMA_IRQ].load(Ordering::Relaxed)),
    ];
    let info = device_info(
        c"lemnos-test-gsdma",
        res.as_ptr(),
        res.len(),
        TGD_PROPS.get().cast(),
    );
    // SAFETY: the information, its resources and its properties live for the call, which copies them.
    let pdev = from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const info) })?;
    TGD_PDEV.store(pdev, Ordering::Relaxed);
    // SAFETY: the device is registered; the group is a static.
    to_result(unsafe { bindings::sysfs_create_group(&raw mut (*pdev).dev.kobj, TGD_GROUP.get()) })?;
    TGD_LAST.store(
        // SAFETY: the kernel's clock, read.
        unsafe { bindings::ktime_get() },
        Ordering::Relaxed,
    );
    // SAFETY: the work is initialised by init_statics; the first tick is queued.
    unsafe { schedule_on(TGD_CPU, TGD_WORK.get(), jiffies_of(1)) };
    Ok(())
}

/// Tears the engine down: its device and group, the tick, and its page.
pub(crate) fn teardown() {
    let pdev = TGD_PDEV.swap(ptr::null_mut(), Ordering::Relaxed);
    if !pdev.is_null() {
        // SAFETY: the device is registered; removing a group that was not made does nothing.
        unsafe { bindings::sysfs_remove_group(&raw mut (*pdev).dev.kobj, TGD_GROUP.get()) };
        // SAFETY: the device was registered by setup, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(pdev) };
    }
    // SAFETY: the work is initialised by init_statics: cancelling it is valid, even when never queued.
    unsafe { bindings::cancel_delayed_work_sync(TGD_WORK.get()) };
    TGD_REGS.store(ptr::null_mut(), Ordering::Relaxed);
    let page = TGD_PAGE.swap(ptr::null_mut(), Ordering::Relaxed);
    if !page.is_null() {
        // SAFETY: the page was allocated by setup with order 0, and nothing maps it now.
        unsafe { free_block_page(page, 0) };
    }
}
