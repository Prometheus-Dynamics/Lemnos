// SPDX-License-Identifier: GPL-2.0
//! The fake VIP9000. Its registers are a page of ordinary memory, looked at every millisecond while a
//! driver has it. A 1 in the command fetcher's enable bit starts the commands at its address, which are
//! run as the block would, through its MMU when that is on: the root table from the page directory
//! register, or from the page table array a set-up command selected; each second-level entry present,
//! and writable for a write. A miss is the MMU's exception: interrupt status bit 30, the fetcher stays
//! busy until a reset. Commands: state loads, NOP, STALL and END; END makes it idle and raises the
//! interrupt with the events the commands asked for. Three states of its own copy memory (0x5000
//! source, 0x5001 destination, 0x5002 bytes: copies) and hang (0x5003). A reset is seen as the clock
//! register's idle bits coming back after the fake cleared them when the MMU went on; it turns the MMU
//! off and makes the block idle. With `stuck`, resets are ignored.
//!
//! Copyright (c) 2026 the Lemnos authors.

use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
};

use super::fixture::{self, dev_attr, property_end, property_string, Fixed, Shared};
use super::{
    alloc_block_page, device_info, free_block_page, init_delayed_work, jiffies_of, lock, mem_copy,
    mem_ld32, mem_ld32_once, rd, schedule_on, unlock, wr, PAGE_SIZE, TB_IRQ, TB_VIP_IRQ,
    WORK_CPU_UNBOUND,
};
use kernel::{
    bindings,
    error::{from_err_ptr, to_result},
    ffi::{c_uint, c_ulong},
    prelude::*,
};

const TVIP_CLOCK: usize = 0x000;
const TVIP_IDLE: usize = 0x004;
const TVIP_INT: usize = 0x010;
const TVIP_CID: usize = 0x030;
const TVIP_MMU: usize = 0x388;
const TVIP_PTA_LOW: usize = 0x38c;
const TVIP_PTA_HIGH: usize = 0x390;
const TVIP_FE_CTL: usize = 0x3a4;
const TVIP_PD: usize = 0x3b4;
const TVIP_FE_ADDR: usize = 0x654;
const TVIP_IDLE_ALL: u32 = 0x7fff_ffff;
const TVIP_CLOCK_BITS: u32 = 0x0003_0000;

/// The fake VIP's register window: its page's address, and the page (freed at teardown).
static TVIP_REGS: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
static TVIP_PAGE: AtomicPtr<bindings::page> = AtomicPtr::new(ptr::null_mut());
static TVIP_PDEV: AtomicPtr<bindings::platform_device> = AtomicPtr::new(ptr::null_mut());
static TVIP_MUTEX: Shared<bindings::mutex> =
    // SAFETY: a mutex of zeros is unlocked; it is initialised by `init_statics`.
    Shared::new(unsafe { core::mem::zeroed() });
/// The tick (DECLARE_DELAYED_WORK in the C, set up by `init_statics`).
static TVIP_WORK: Shared<bindings::delayed_work> =
    // SAFETY: a delayed work of zeros is initialised by `init_statics` before it is queued.
    Shared::new(unsafe { core::mem::zeroed() });
/// The customer id the block reports; set before the driver binds.
static TVIP_CID_VALUE: AtomicU32 = AtomicU32::new(0x1000_003b);
/// Resets are ignored (`stuck`); a reset is in progress (`marked`); the page table array is set up;
/// the fetcher is busy after a fault or a hang.
static TVIP_STUCK: AtomicBool = AtomicBool::new(false);
static TVIP_MARKED: AtomicBool = AtomicBool::new(false);
static TVIP_PTA: AtomicBool = AtomicBool::new(false);
static TVIP_BUSY: AtomicBool = AtomicBool::new(false);
static TVIP_JOBS: AtomicU32 = AtomicU32::new(0);
static TVIP_FAULTS: AtomicU32 = AtomicU32::new(0);
static TVIP_RESETS: AtomicU32 = AtomicU32::new(0);
static TVIP_FLUSHES: AtomicU32 = AtomicU32::new(0);
static TVIP_COPIED: AtomicU32 = AtomicU32::new(0);
static TVIP_ERRORS: AtomicU32 = AtomicU32::new(0);
/// The copy's addresses, set by the state loads.
static TVIP_SRC: AtomicU32 = AtomicU32::new(0);
static TVIP_DST: AtomicU32 = AtomicU32::new(0);

static TVIP_PROPS: Fixed<[bindings::property_entry; 2]> = Fixed([
    property_string(c"compatible", c"lemnos,test-vip"),
    property_end(),
]);

/// "1": resets are ignored from now on (until "0").
unsafe extern "C" fn stuck_store(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *const kernel::ffi::c_char,
    count: usize,
) -> isize {
    let mut v = false;
    // SAFETY: sysfs passes the store's text, NUL-terminated; `v` is written on success.
    if unsafe { bindings::kstrtobool(buf, &raw mut v) } != 0 {
        return EINVAL.to_errno() as isize;
    }
    // SAFETY: the mutex is initialised by init_statics.
    unsafe { lock(TVIP_MUTEX.get()) };
    TVIP_STUCK.store(v, Ordering::Relaxed);
    // SAFETY: the mutex taken above.
    unsafe { unlock(TVIP_MUTEX.get()) };
    count as isize
}

/// The customer id the block reports; set before the driver binds.
unsafe extern "C" fn cid_store(
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
    unsafe { lock(TVIP_MUTEX.get()) };
    TVIP_CID_VALUE.store(v, Ordering::Relaxed);
    // SAFETY: the window is mapped by setup.
    unsafe { vip_wr(TVIP_CID, v) };
    // SAFETY: the mutex taken above.
    unsafe { unlock(TVIP_MUTEX.get()) };
    count as isize
}

unsafe extern "C" fn cid_show(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *mut kernel::ffi::c_char,
) -> isize {
    // SAFETY: sysfs passes a page for the text.
    unsafe {
        bindings::sysfs_emit(
            buf,
            c"0x%08x\n".as_ptr().cast(),
            TVIP_CID_VALUE.load(Ordering::Relaxed) as c_uint,
        ) as isize
    }
}

/// "jobs faults resets flushes copied errors": since the module loaded.
unsafe extern "C" fn vip_ran_show(
    _dev: *mut bindings::device,
    _attr: *mut bindings::device_attribute,
    buf: *mut kernel::ffi::c_char,
) -> isize {
    // SAFETY: the mutex is initialised by init_statics.
    unsafe { lock(TVIP_MUTEX.get()) };
    // SAFETY: sysfs passes a page for the text.
    let n = unsafe {
        bindings::sysfs_emit(
            buf,
            c"%u %u %u %u %u %u\n".as_ptr().cast(),
            TVIP_JOBS.load(Ordering::Relaxed) as c_uint,
            TVIP_FAULTS.load(Ordering::Relaxed) as c_uint,
            TVIP_RESETS.load(Ordering::Relaxed) as c_uint,
            TVIP_FLUSHES.load(Ordering::Relaxed) as c_uint,
            TVIP_COPIED.load(Ordering::Relaxed) as c_uint,
            TVIP_ERRORS.load(Ordering::Relaxed) as c_uint,
        )
    } as isize;
    // SAFETY: the mutex taken above.
    unsafe { unlock(TVIP_MUTEX.get()) };
    n
}

static DEV_ATTR_CID: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"cid", 0o644, Some(cid_show), Some(cid_store)));
static DEV_ATTR_STUCK: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"stuck", 0o200, None, Some(stuck_store)));
static DEV_ATTR_RAN: Shared<bindings::device_attribute> =
    Shared::new(dev_attr(c"ran", 0o444, Some(vip_ran_show), None));

static TVIP_ATTRS: Fixed<[*mut bindings::attribute; 4]> = Fixed([
    // The address of each attribute's `attr`, its first field.
    DEV_ATTR_CID.get().cast(),
    DEV_ATTR_STUCK.get().cast(),
    DEV_ATTR_RAN.get().cast(),
    ptr::null_mut(),
]);
static TVIP_GROUP: Fixed<bindings::attribute_group> = Fixed(bindings::attribute_group {
    __bindgen_anon_2: bindings::attribute_group__bindgen_ty_2 {
        attrs: TVIP_ATTRS.get().cast_mut().cast(),
    },
    // SAFETY: no name and no visibility callbacks: zero.
    ..unsafe { core::mem::zeroed() }
});

/// Sets up the mutex and the tick, before any device.
pub(crate) fn init_statics() {
    // SAFETY: the mutex is a static this module owns, initialised once with its own lockdep class.
    unsafe {
        bindings::__mutex_init(
            TVIP_MUTEX.get(),
            c"tvip_lock".as_ptr().cast(),
            kernel::static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: the work is a static this module owns, initialised once before anything queues it.
    unsafe { init_delayed_work(TVIP_WORK.get(), tvip_tick, c"tvip_work") };
}

/// The block's register at `off`.
///
/// # Safety
///
/// The window is mapped by `setup`, until teardown.
unsafe fn vip_rd(off: usize) -> u32 {
    // SAFETY: the caller's: the window is mapped, and `off` is one of its registers.
    unsafe { rd(TVIP_REGS.load(Ordering::Relaxed), off) }
}

/// The block's register at `off`, written.
///
/// # Safety
///
/// As `vip_rd`.
unsafe fn vip_wr(off: usize, v: u32) {
    // SAFETY: the caller's: the window is mapped, and `off` is one of its registers.
    unsafe { wr(TVIP_REGS.load(Ordering::Relaxed), off, v) };
}

/// The kernel's pointer for a physical address the block reads, or NULL when it is not a page the kernel has.
///
/// # Safety
///
/// Nothing beyond what the caller does with the pointer (checked where it is read).
unsafe fn tvip_phys(pa: u64) -> *mut u8 {
    // SAFETY: pfn_valid is any number's question.
    if unsafe { fixture::lemnos_test_pfn_valid((pa >> bindings::PAGE_SHIFT) as c_ulong) } != 0 {
        // SAFETY: the page is one the kernel has (checked above): its kernel mapping is the address.
        unsafe { fixture::lemnos_test_phys_to_virt(pa) }.cast()
    } else {
        ptr::null_mut()
    }
}

/// The kernel's pointer for the block's address `va`, and the bytes to its page's end in `left`; NULL: a
/// fault. Through the MMU when it is on: the root table from the page directory register, or from the
/// page table array a set-up command selected; each second-level entry present, and writable for a write.
///
/// # Safety
///
/// The window is mapped by setup.
unsafe fn tvip_at(va: u32, write: bool, left: &mut u32) -> *mut u8 {
    // SAFETY: the caller's: the window is mapped.
    let mmu = unsafe { vip_rd(TVIP_MMU) };
    let mut pa = u64::from(va);
    if (mmu & 1) != 0 {
        let root: u64 = if (mmu & (1 << 5)) != 0 {
            // SAFETY: as above: the page directory register.
            let pd = unsafe { vip_rd(TVIP_PD) };
            u64::from(pd >> 4) << 12
        } else {
            if !TVIP_PTA.load(Ordering::Relaxed) {
                return ptr::null_mut();
            }
            // SAFETY: as above: the page table array's address, from its two registers.
            let lo = unsafe { vip_rd(TVIP_PTA_LOW) };
            // SAFETY: as above.
            let hi = unsafe { vip_rd(TVIP_PTA_HIGH) };
            // SAFETY: the array the command set up: a page the kernel has, or NULL.
            let pta = unsafe { tvip_phys(u64::from(lo) | (u64::from(hi) << 32)) };
            if pta.is_null() {
                return ptr::null_mut();
            }
            // SAFETY: the array's first word, as the C reads it.
            let w0 = unsafe { mem_ld32(pta, 0) };
            // SAFETY: the array's second word, as the C reads it.
            let w1 = unsafe { mem_ld32(pta, 4) };
            (u64::from(w0 & !0xfff)) | (u64::from(w1) << 32)
        };
        // SAFETY: the root table, a page the kernel has, or NULL.
        let mtlb = unsafe { tvip_phys(root) };
        if mtlb.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: READ_ONCE of the first-level entry for `va`, inside the table's page.
        let m = u32::from_le(unsafe { mem_ld32_once(mtlb, ((va >> 24) as usize) * 4) });
        if (m & 1) == 0 {
            return ptr::null_mut();
        }
        // SAFETY: the second-level table the entry names: a page the kernel has, or NULL.
        let stlb = unsafe { tvip_phys(u64::from(m & !0x3f)) };
        if stlb.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: READ_ONCE of the second-level entry for `va`, inside the table's page.
        let pte = u32::from_le(unsafe { mem_ld32_once(stlb, (((va >> 12) & 0xfff) as usize) * 4) });
        if (pte & 1) == 0 || (write && (pte & 4) == 0) {
            return ptr::null_mut();
        }
        pa = u64::from(pte & !0xfff) | (u64::from((pte >> 4) & 0xff) << 32) | u64::from(va & 0xfff);
    }
    *left = 4096 - (va & 0xfff);
    // SAFETY: as the rest of this function: a page the kernel has, or NULL.
    unsafe { tvip_phys(pa) }
}

/// Copies `len` bytes from the source address to the destination the state loads set, through the MMU.
/// False where either side faults.
///
/// # Safety
///
/// The window is mapped by setup.
unsafe fn tvip_copy(len: u32) -> bool {
    let mut src = TVIP_SRC.load(Ordering::Relaxed);
    let mut dst = TVIP_DST.load(Ordering::Relaxed);
    let mut len = len;
    while len != 0 {
        let mut ls = 0u32;
        let mut ld = 0u32;
        // SAFETY: the caller's: the window is mapped; each side is checked for NULL below.
        let s = unsafe { tvip_at(src, false, &mut ls) };
        // SAFETY: as above.
        let d = unsafe { tvip_at(dst, true, &mut ld) };
        if s.is_null() || d.is_null() {
            return false;
        }
        let n = len.min(ls).min(ld);
        // SAFETY: `n` bytes are inside the two pages, to each page's end (checked above by `tvip_at`).
        unsafe { mem_copy(s, d, n as usize) };
        src = src.wrapping_add(n);
        dst = dst.wrapping_add(n);
        len -= n;
        TVIP_COPIED.fetch_add(n, Ordering::Relaxed);
    }
    true
}

/// How the fetcher's run ended: at END, on a fault (the MMU's exception), or hung.
enum TvipRun {
    End,
    Fault,
    Hang,
}

/// Runs what the fetcher was started on: the commands at its address, the events they raise into `events`.
///
/// # Safety
///
/// The window is mapped by setup.
unsafe fn tvip_run(events: &mut u32) -> TvipRun {
    // SAFETY: the caller's: the window is mapped.
    let mut va = unsafe { vip_rd(TVIP_FE_ADDR) };
    // SAFETY: as above.
    let mut left = (unsafe { vip_rd(TVIP_FE_CTL) } & 0xffff) * 2;
    while left != 0 {
        let mut l = 0u32;
        // SAFETY: the caller's: the window is mapped.
        let p = unsafe { tvip_at(va, false, &mut l) };
        if p.is_null() {
            return TvipRun::Fault;
        }
        // SAFETY: the command word, at the address the fetcher is at (checked above).
        let w = u32::from_le(unsafe { mem_ld32(p, 0) });
        let op = w >> 27;
        if op == 2 {
            return TvipRun::End;
        }
        let n;
        if op == 3 || op == 9 {
            n = 2;
        } else if op == 1 {
            let addr = w & 0xffff;
            let mut count = (w >> 16) & 0x3ff;
            if count == 0 {
                count = 1024;
            }
            for i in 0..count {
                let mut vl = 0u32;
                // SAFETY: the caller's: the window is mapped.
                let q = unsafe { tvip_at(va.wrapping_add(4 + i * 4), false, &mut vl) };
                if q.is_null() {
                    return TvipRun::Fault;
                }
                // SAFETY: the value word, at the address checked above.
                let v = u32::from_le(unsafe { mem_ld32(q, 0) });
                match addr.wrapping_add(i) {
                    0x0061 => {
                        TVIP_FLUSHES.fetch_add(1, Ordering::Relaxed);
                    }
                    0x0e01 => *events |= 1 << (v & 0x1f),
                    0x0e12 => {
                        // SAFETY: as above.
                        if (unsafe { vip_rd(TVIP_MMU) } & 1) == 0 {
                            TVIP_PTA.store(true, Ordering::Relaxed);
                        }
                    }
                    0x5000 => TVIP_SRC.store(v, Ordering::Relaxed),
                    0x5001 => TVIP_DST.store(v, Ordering::Relaxed),
                    0x5002 => {
                        // SAFETY: as above.
                        if !unsafe { tvip_copy(v) } {
                            return TvipRun::Fault;
                        }
                    }
                    0x5003 => return TvipRun::Hang,
                    _ => {}
                }
            }
            n = (1 + count + 1) & !1;
        } else {
            TVIP_ERRORS.fetch_add(1, Ordering::Relaxed);
            return TvipRun::Hang;
        }
        if n > left {
            return TvipRun::Hang;
        }
        left -= n;
        va = va.wrapping_add(n * 4);
    }
    TvipRun::Hang
}

/// The tick: a reset seen in the clock register ends the MMU; a started fetcher runs its commands, and its
/// events or fault are raised. It runs every ms while a driver has the block, every 50 ms otherwise.
///
/// # Safety
///
/// Called by the kernel's workqueue, for the work `init_statics` set up.
unsafe extern "C" fn tvip_tick(_work: *mut bindings::work_struct) {
    let mut raise = false;
    // SAFETY: the mutex is initialised by init_statics; it is released below.
    unsafe { lock(TVIP_MUTEX.get()) };
    if !TVIP_STUCK.load(Ordering::Relaxed) {
        // SAFETY: the window is mapped by setup.
        let clock = unsafe { vip_rd(TVIP_CLOCK) };
        if TVIP_MARKED.load(Ordering::Relaxed) && (clock & TVIP_CLOCK_BITS) == TVIP_CLOCK_BITS {
            // SAFETY: as above.
            unsafe { vip_wr(TVIP_MMU, 0) };
            // SAFETY: as above.
            unsafe { vip_wr(TVIP_FE_CTL, 0) };
            // SAFETY: as above.
            unsafe { vip_wr(TVIP_IDLE, TVIP_IDLE_ALL) };
            TVIP_PTA.store(false, Ordering::Relaxed);
            TVIP_BUSY.store(false, Ordering::Relaxed);
            TVIP_MARKED.store(false, Ordering::Relaxed);
            TVIP_RESETS.fetch_add(1, Ordering::Relaxed);
        } else if !TVIP_MARKED.load(Ordering::Relaxed) {
            // SAFETY: as above.
            let mmu = unsafe { vip_rd(TVIP_MMU) };
            if (mmu & 1) != 0 {
                // SAFETY: as above.
                unsafe { vip_wr(TVIP_CLOCK, clock & !TVIP_CLOCK_BITS) };
                TVIP_MARKED.store(true, Ordering::Relaxed);
            }
        }
    }
    // SAFETY: as above.
    if !TVIP_BUSY.load(Ordering::Relaxed) && (unsafe { vip_rd(TVIP_FE_CTL) } & (1 << 16)) != 0 {
        let mut events = 0u32;
        // SAFETY: as above.
        unsafe { vip_wr(TVIP_IDLE, TVIP_IDLE_ALL & !1) };
        // SAFETY: as above: the fetcher is started, and its commands are run.
        let r = unsafe { tvip_run(&mut events) };
        // SAFETY: as above.
        unsafe { vip_wr(TVIP_FE_CTL, 0) };
        // SAFETY: as above.
        if (unsafe { vip_rd(TVIP_MMU) } & 1) != 0 {
            TVIP_JOBS.fetch_add(1, Ordering::Relaxed);
        }
        match r {
            TvipRun::End => {
                // SAFETY: as above.
                unsafe { vip_wr(TVIP_IDLE, TVIP_IDLE_ALL) };
                if events != 0 {
                    // SAFETY: as above.
                    let int = unsafe { vip_rd(TVIP_INT) };
                    // SAFETY: as above.
                    unsafe { vip_wr(TVIP_INT, int | events) };
                    raise = true;
                }
            }
            TvipRun::Fault | TvipRun::Hang => {
                TVIP_BUSY.store(true, Ordering::Relaxed);
                if let TvipRun::Fault = r {
                    TVIP_FAULTS.fetch_add(1, Ordering::Relaxed);
                    // SAFETY: as above.
                    let int = unsafe { vip_rd(TVIP_INT) };
                    // SAFETY: as above.
                    unsafe { vip_wr(TVIP_INT, int | (1 << 30)) };
                    raise = true;
                }
            }
        }
    }
    // SAFETY: as above.
    let on = unsafe { vip_rd(TVIP_CLOCK) } != 0 || TVIP_MARKED.load(Ordering::Relaxed);
    // SAFETY: the mutex taken above.
    unsafe { unlock(TVIP_MUTEX.get()) };
    if raise {
        // SAFETY: the interrupt is one of the simulated domain's, mapped until teardown.
        unsafe {
            bindings::irq_set_irqchip_state(
                TB_IRQ[TB_VIP_IRQ].load(Ordering::Relaxed),
                bindings::irqchip_irq_state_IRQCHIP_STATE_PENDING,
                true,
            )
        };
    }
    // SAFETY: the work is initialised by init_statics; it is queued again.
    unsafe {
        schedule_on(
            WORK_CPU_UNBOUND,
            TVIP_WORK.get(),
            jiffies_of(if on { 1 } else { 50 }),
        )
    };
}

/// Registers the block: a page of its registers, the CID it reports, its interrupt, the device and its
/// group, and the first tick.
pub(crate) fn setup() -> Result {
    let page = alloc_block_page(0)?;
    TVIP_PAGE.store(page, Ordering::Relaxed);
    // SAFETY: page_address of the page just allocated: the window of the block's registers.
    TVIP_REGS.store(
        unsafe { fixture::lemnos_test_page_address(page) }.cast(),
        Ordering::Relaxed,
    );
    // SAFETY: the window is mapped (just above): the idle register is set, as the block starts idle.
    unsafe { vip_wr(TVIP_IDLE, TVIP_IDLE_ALL) };
    // SAFETY: as above: the customer id is set before the driver binds.
    unsafe { vip_wr(TVIP_CID, TVIP_CID_VALUE.load(Ordering::Relaxed)) };
    // SAFETY: page_to_phys of the page just allocated.
    let phys = unsafe { fixture::lemnos_test_page_to_phys(page) };
    let res = [
        fixture::res_mem(phys, PAGE_SIZE as u64),
        fixture::res_irq(TB_IRQ[TB_VIP_IRQ].load(Ordering::Relaxed)),
    ];
    let info = device_info(
        c"lemnos-test-vip",
        res.as_ptr(),
        res.len(),
        TVIP_PROPS.get().cast(),
    );
    // SAFETY: the information, its resources and its properties live for the call, which copies them.
    let pdev = from_err_ptr(unsafe { bindings::platform_device_register_full(&raw const info) })?;
    TVIP_PDEV.store(pdev, Ordering::Relaxed);
    // SAFETY: the device is registered; the group is a static.
    to_result(unsafe {
        bindings::sysfs_create_group(&raw mut (*pdev).dev.kobj, TVIP_GROUP.get())
    })?;
    // SAFETY: the work is initialised by init_statics; the first tick is queued.
    unsafe { schedule_on(WORK_CPU_UNBOUND, TVIP_WORK.get(), jiffies_of(1)) };
    Ok(())
}

/// Tears the block down: its device and group, the tick, and its page.
pub(crate) fn teardown() {
    let pdev = TVIP_PDEV.swap(ptr::null_mut(), Ordering::Relaxed);
    if !pdev.is_null() {
        // SAFETY: the device is registered; removing a group that was not made does nothing.
        unsafe { bindings::sysfs_remove_group(&raw mut (*pdev).dev.kobj, TVIP_GROUP.get()) };
        // SAFETY: the device was registered by setup, and this is its one unregistration.
        unsafe { bindings::platform_device_unregister(pdev) };
    }
    // SAFETY: the work is initialised by init_statics: cancelling it is valid, even when never queued.
    unsafe { bindings::cancel_delayed_work_sync(TVIP_WORK.get()) };
    TVIP_REGS.store(ptr::null_mut(), Ordering::Relaxed);
    let page = TVIP_PAGE.swap(ptr::null_mut(), Ordering::Relaxed);
    if !page.is_null() {
        // SAFETY: the page was allocated by setup with order 0, and nothing maps it now.
        unsafe { free_block_page(page, 0) };
    }
}
