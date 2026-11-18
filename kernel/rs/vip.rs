// SPDX-License-Identifier: GPL-2.0
//! The VeriSilicon VIP9000 neural accelerator, as a frame stream (stage 1k of the port; replaces
//! lemnos_vip.c). Jobs are posted on the stream's ring (lemnos_vip.h).
//!
//! What runs on the block is a program of command words that Lemnos neither writes nor understands
//! beyond its framing. Such a program can name any address, so the only guarantee is the block's own
//! MMU, and this file keeps the MMU to itself: one root table per block, pointed at once per reset; its
//! entries change with every job, and while a job runs they lead to the module's command buffer and to
//! that job's slots, mapped page by page from the device addresses the stream computed. The block is
//! never given the caller's bytes: a program is copied into kernel memory at load and checked there,
//! and copied into the command buffer at each job.
//!
//! A fault or a job too long is answered by a reset and a fresh set-up; only a block that can't be
//! shown reset is wedged. Locking: `state` (the job on the block, the attached stream's flags, counts),
//! the collector's `collect_lock` (one collector; `stopped`), then the stream's `prog` lock (the
//! programs). The tables are the collector's while the block does not run and the workqueue's end's
//! while it does: the running flag orders them.

use crate::bindings as lb;
use crate::param::param;
use crate::stream::{
    ktime_add_ms, ktime_now, module_hold, msecs_to_jiffies, udelay, usleep_range, DelayedWork, Handle,
    Producer, Slot, Stream, WaitQueue, Work, Wq,
};
use core::{
    ffi::{c_int, c_long, c_uint, c_void},
    mem::offset_of,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};
use kernel::{
    alloc::{flags::GFP_KERNEL, KBox, KVVec},
    bindings,
    device::Device,
    error::{code::*, from_err_ptr, to_result, Error, Result},
    ffi::{c_ulong},
    ioctl::{_IOR, _IOW, _IOWR},
    new_mutex, new_spinlock_irq,
    prelude::*,
    sync::{Arc, Mutex, SpinLockIrq},
    transmute::{AsBytes, FromBytes},
    uaccess::{UserPtr, UserSlice},
};

// Registers (vip_drv_hardware.c).
const VR_CLOCK: usize = 0x00000;
const VR_CLOCK_INIT: u32 = 0x0007_0900;
const VR_CLOCK_FSCALE_LOAD: u32 = 1 << 9;
const VR_CLOCK_GPU_IDLE: u32 = 1 << 11;
const VR_CLOCK_SOFT_RESET: u32 = 1 << 12;
const VR_CLOCK_IDLE_BITS: u32 = 0x0003_0000;
const VR_CLOCK_ISOLATE: u32 = 1 << 19;
const VR_IDLE: usize = 0x0004;
const VR_IDLE_FE: u32 = 1 << 0;
const VR_IDLE_ALL: u32 = 0x7fff_ffff;
const VR_INT_ACK: usize = 0x0010;
const VR_INT_ENABLE: usize = 0x0014;
const VR_INT_AXI: u32 = 1 << 31;
const VR_INT_MMU: u32 = 1 << 30;
const VR_CID: usize = 0x0030;
const VR_MEM_COUNTERS: usize = 0x003c;
const VR_REORDER: usize = 0x0090;
const VR_GATING: usize = 0x0100;
const VR_GATING_INIT: u32 = 0x0014_0021;
const VR_GATING_MODULES: usize = 0x0104;
const VR_PULSE: usize = 0x010c;
const VR_PULSE_INIT: u32 = 0x0159_0880;
const VR_MMU_CONTROL: usize = 0x0388;
const VR_MMU_ENABLE: u32 = 1 << 0;
const VR_MMU_PD: u32 = 1 << 5;
const VR_PTA_LOW: usize = 0x038c;
const VR_PTA_HIGH: usize = 0x0390;
const VR_PTA_LOAD: usize = 0x0394;
const VR_FE_CONTROL: usize = 0x03a4;
const VR_FE_ENABLE: u32 = 1 << 16;
const VR_AHB_CONTROL: usize = 0x03a8;
const VR_AHB_RESET: u32 = 1 << 0;
const VR_AHB_INIT: u32 = 1 << 1;
const VR_PD_ENTRY: usize = 0x03b4;
const VR_OUTSTANDING: usize = 0x0414;
const VR_POWER: usize = 0x055c;
const VR_FE_ADDRESS: usize = 0x0654;
const VR_WINDOW: u64 = 0x700;

// Commands (vip_drv_hardware.c, task/vip_drv_task_schedule.c).
const VC_LOAD_STATE: u32 = 1;
const VC_END: u32 = 2;
const VC_NOP: u32 = 3;
const VC_STALL: u32 = 9;
const fn vc_state(addr: u32, n: u32) -> u32 {
    (VC_LOAD_STATE << 27) | (n << 16) | addr
}
const VS_MMU_CONFIG: u32 = 0x0061;
const VS_PTA_CONFIG: u32 = 0x006b;
const VS_EVENT: u32 = 0x0e01;
const VS_SEMAPHORE: u32 = 0x0e02;
const VS_FLUSH: u32 = 0x0e03;
const VS_MMU_SETUP: u32 = 0x0e12;
const VS_CONTROL_END: u32 = 0x0400;
const VC_TOKEN: u32 = 0x0000_0701;
const VC_TOKEN_ALL: u32 = 0x3000_0701;
const VC_EVENT_ID: u32 = 4;
const VC_EVENT_FROM_PE: u32 = 1 << 6;

/// The command buffer: the module's preamble, the program, the module's tail.
const LVIP_PRE_WORDS: usize = 6;
const LVIP_TAIL_WORDS: usize = 10;
const PAGE: u64 = 4096;
const LVIP_CMD_BYTES: u64 = (65535 * 8 + PAGE - 1) & !(PAGE - 1);
const LVIP_CMD_PAGES: u64 = LVIP_CMD_BYTES / PAGE;

// The MMU, 32-bit addresses, "1K mode" (memory/vip_drv_mmu.h).
const LVIP_MTLB_SHIFT: u32 = 24;
const LVIP_MTLB_ENTRIES: usize = 256;
const LVIP_STLB_ENTRIES: usize = 4096;
const LVIP_STLB_BYTES: usize = LVIP_STLB_ENTRIES * 4;
const LVIP_PER_SLOT: usize = (lb::LEMNOS_VIP_SLOT_SPAN >> LVIP_MTLB_SHIFT) as usize;
const LVIP_SLOTS: usize = lb::LEMNOS_VIP_SLOTS as usize;
const LVIP_PROGRAMS: usize = lb::LEMNOS_VIP_PROGRAMS as usize;
const LVIP_MTLB_PRESENT: u32 = 1 << 0;
const LVIP_PTE_PRESENT: u32 = 1 << 0;
const LVIP_PTE_EXCEPTION: u32 = 1 << 1;
const LVIP_PTE_WRITABLE: u32 = 1 << 2;
const LVIP_PHYS_LIMIT: u64 = 1 << 40;
const LVIP_PD_MODE_1K: u32 = 1;

// Bounds.
const LVIP_TIMEOUT_MS: u32 = 5000;
const LVIP_RESET_MS: u64 = 20;
const LVIP_IDLE_MS: u64 = 50;

/// RESET_CONTROL_OPTIONAL_EXCLUSIVE of reset.h (as rs/regjob.rs spells it).
const RESET_CONTROL_OPTIONAL_EXCLUSIVE: c_uint = (1 << 1) | (1 << 2);
/// rpm_flags of pm.h: the get that the resume and its put pair with, and the asynchronous idle.
const RPM_GET_PUT: c_int = bindings::RPM_GET_PUT as c_int;
const RPM_ASYNC: c_int = bindings::RPM_ASYNC as c_int;

const IOC_LOAD: u32 = _IOWR::<lb::lemnos_vip_load>(b'L' as u32, 0xe1);
const IOC_UNLOAD: u32 = _IOW::<lb::lemnos_vip_unload>(b'L' as u32, 0xe2);
const IOC_STATUS: u32 = _IOR::<lb::lemnos_vip_status>(b'L' as u32, 0xe3);

// SAFETY: plain structs of integers (the uAPI), with no padding the kernel reads.
unsafe impl AsBytes for lb::lemnos_vip_status {}
// SAFETY: as above; read in from the caller's memory.
unsafe impl FromBytes for lb::lemnos_vip_load {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_vip_unload {}
// SAFETY: as above; the load is written back as well.
unsafe impl AsBytes for lb::lemnos_vip_load {}

const _: () = assert!(core::mem::size_of::<lb::lemnos_vip_job>() == 232);
const _: () = assert!(core::mem::size_of::<lb::lemnos_vip_create>() == 64);
const _: () = assert!(core::mem::size_of::<lb::lemnos_vip_load>() == 24);
const _: () = assert!(core::mem::size_of::<lb::lemnos_vip_status>() == 48);
const _: () = assert!(LVIP_SLOTS <= crate::stream::JOB_HOLDS);
const _: () = assert!(lb::LEMNOS_VIP_PROGRAM_ADDR as u64 + LVIP_CMD_BYTES <= 1u64 << LVIP_MTLB_SHIFT);
const _: () = assert!(((LVIP_PRE_WORDS + LVIP_TAIL_WORDS) * 4) as u64 + lb::LEMNOS_VIP_MAX_PROGRAM_BYTES as u64 <= 65535 * 8);

/// The chips the vendor's feature table lists with an MMU, and how each is pointed at it.
const LVIP_CHIPS: [(u32, u32); 3] = [
    (0x1000_0016, lb::LEMNOS_VIP_MMU_COMMAND),
    (0x1000_0021, lb::LEMNOS_VIP_MMU_PD),
    (0x1000_003b, lb::LEMNOS_VIP_MMU_PD),
];

param!(
    FAKE_REGS,
    test_vip_fake_regs: bool = false,
    param_ops_bool,
    "bool",
    "Test only: the VIP9000's register window is ordinary memory (the test harness)"
);
param!(
    TIMEOUT_MS,
    test_vip_timeout_ms: u32 = 0,
    param_ops_uint,
    "uint",
    "Test only: a job's time on the VIP9000 (ms; 0 leaves it)"
);

/// The block's tables and command buffer: coherent, below 4 GiB. See the module's notes on who owns them.
struct Tables {
    mtlb: *mut u32,
    mtlb_dma: bindings::dma_addr_t,
    /// The command buffer's window.
    stlb0: *mut u32,
    stlb0_dma: bindings::dma_addr_t,
    cmd: *mut u32,
    cmd_dma: bindings::dma_addr_t,
    /// The page table array's one entry (command set-up).
    pta: *mut u32,
    pta_dma: bindings::dma_addr_t,
    /// Per slot, allocated as first needed and kept until unbind.
    stlb: [[*mut u32; LVIP_PER_SLOT]; LVIP_SLOTS],
    stlb_dma: [[bindings::dma_addr_t; LVIP_PER_SLOT]; LVIP_SLOTS],
    /// Pages mapped for the job on the block.
    mapped: [u32; LVIP_SLOTS],
}

impl Tables {
    const fn empty() -> Self {
        Self {
            mtlb: ptr::null_mut(),
            mtlb_dma: 0,
            stlb0: ptr::null_mut(),
            stlb0_dma: 0,
            cmd: ptr::null_mut(),
            cmd_dma: 0,
            pta: ptr::null_mut(),
            pta_dma: 0,
            stlb: [[ptr::null_mut(); LVIP_PER_SLOT]; LVIP_SLOTS],
            stlb_dma: [[0; LVIP_PER_SLOT]; LVIP_SLOTS],
            mapped: [0; LVIP_SLOTS],
        }
    }
}

/// The tables, behind an UnsafeCell: one owner at a time (see the module notes).
struct TablesCell(core::cell::UnsafeCell<Tables>);

// SAFETY: the tables are touched by the collector while the block is not running and by the end while it
// is, and the running flag (under `state`) orders the two; nothing else reads them.
unsafe impl Send for TablesCell {}
// SAFETY: as Send.
unsafe impl Sync for TablesCell {}

/// The state under `LemVip::state`.
#[derive(Default)]
struct VipState {
    attached: *mut LvipStream,
    stopping: bool,
    stream_wedged: bool,
    sequence: u64,
    /// The block has `slot`.
    running: bool,
    /// Its end is with the workqueue.
    ending: bool,
    fault: bool,
    timed_out: bool,
    wedged: bool,
    module_held: bool,
    int_status: u32,
    slot: Slot,
    finished: u64,
    refused: u64,
    faults: u64,
    timeouts: u64,
    last_fault: u32,
}

/// One block (`struct lem_vip`).
#[pin_data]
pub(crate) struct LemVip {
    dev: *mut bindings::device,
    regs: *mut c_void,
    wq: Wq,
    irq: c_int,
    cid: u32,
    mmu: u32,
    timeout_ms: u32,
    tables: TablesCell,
    #[pin]
    create_lock: Mutex<()>,
    #[pin]
    state: SpinLockIrq<VipState>,
    resets: AtomicU32,
    #[pin]
    end: Work,
    #[pin]
    watchdog: DelayedWork,
}

// SAFETY: the raw pointers are the device's own, valid for the block's life; the registers are accessed
// under `state` or by the one owner of the block at a time; the rest is immutable after probe.
unsafe impl Send for LemVip {}
// SAFETY: as Send: each shared access is made under `state`, by the tables' owner, or is immutable.
unsafe impl Sync for LemVip {}

/// One stream on the block: the producer's private data, freed at release.
#[pin_data]
struct LvipStream {
    l: Arc<LemVip>,
    s: AtomicPtr<Stream>,
    /// One collector at a time; `true` once no collection may touch the block.
    #[pin]
    collect_lock: Mutex<bool>,
    /// The programs, checked and copied at load, held until unload or release.
    #[pin]
    prog: Mutex<[Option<KVVec<u8>>; LVIP_PROGRAMS]>,
    #[pin]
    idle: WaitQueue,
    #[pin]
    collect: DelayedWork,
}

/// A register of the block's window (mapped for VR_WINDOW bytes, probe checked the resource's size).
fn rd_reg(regs: *mut c_void, off: usize) -> u32 {
    // SAFETY: the register window is mapped for VR_WINDOW bytes, and every offset read is below it.
    unsafe { bindings::readl(regs.cast::<u8>().add(off).cast()) }
}

fn wr_reg(regs: *mut c_void, off: usize, val: u32) {
    // SAFETY: as rd_reg.
    unsafe { bindings::writel(val, regs.cast::<u8>().add(off).cast()) };
}

impl LemVip {
    fn rd(&self, off: usize) -> u32 {
        rd_reg(self.regs, off)
    }

    fn wr(&self, off: usize, val: u32) {
        wr_reg(self.regs, off, val);
    }

    /// The tables, for their owner (see TablesCell).
    #[allow(clippy::mut_from_ref)]
    fn tables(&self) -> &mut Tables {
        // SAFETY: the caller is the tables' owner: the collector while the block is not running, or the
        // end while it is (see TablesCell); the two never run together.
        unsafe { &mut *self.tables.0.get() }
    }

    /// The device, for the logs.
    fn device(&self) -> &Device {
        // SAFETY: the device is live while the block is.
        unsafe { Device::from_raw(self.dev) }
    }

    /// Whether `ok` holds within `ms`, read every 100 us. Sleeps.
    fn poll(&self, ms: u64, ok: impl Fn(&LemVip) -> bool) -> bool {
        let until = ktime_add_ms(ktime_now(), ms);
        loop {
            if ok(self) {
                return true;
            }
            if ktime_now() >= until {
                return false;
            }
            usleep_range(100, 200);
        }
    }

    /// The block can't be given up safely. `state` held.
    fn wedge(&self, st: &mut VipState, ps: *mut LvipStream, err: c_int) {
        if !st.wedged {
            dev_crit!(self.device(), "block did not reset: its job's buffers stay held until a reboot\n");
        }
        st.wedged = true;
        if !st.module_held {
            st.module_held = true;
            module_hold();
        }
        if !ps.is_null() {
            // SAFETY: the attached stream lives until it is detached, which `state` orders.
            let p = unsafe { &*ps };
            st.stream_wedged = true;
            Handle(p.s.load(Ordering::Relaxed)).fault(err);
            p.idle.wake_all();
        }
    }
}

/// LEMNOS_VIP_SLOT_ADDR(n): where the block sees slot `n` while a job runs.
fn slot_addr(n: usize) -> u32 {
    ((n as u32) + 1) << 28
}

/// The page-table entry for `addr`, writable or read-only.
fn pte(addr: u64, writable: bool) -> u32 {
    (addr as u32 & !(PAGE as u32 - 1))
        | (((addr >> 32) as u32 & 0xff) << 4)
        | if writable { LVIP_PTE_WRITABLE } else { 0 }
        | LVIP_PTE_EXCEPTION
        | LVIP_PTE_PRESENT
}

/// Copies one word of a program: native order, as the C copied the user's bytes.
fn word(words: &[u8], i: usize) -> u32 {
    u32::from_ne_bytes([words[4 * i], words[4 * i + 1], words[4 * i + 2], words[4 * i + 3]])
}

/// Whether a program may run: whole commands to its end, each one of those the header names. `Err`
/// carries the errno and the byte offset of the refused command. Touches nothing but `words`.
fn scan(words: &[u8]) -> core::result::Result<(), (Error, u32)> {
    let n = words.len() / 4;
    let mut i = 0usize;
    while i < n {
        let w = word(words, i);
        let op = w >> 27;
        let at = (i * 4) as u32;
        let len = match op {
            VC_LOAD_STATE => {
                let addr = w & 0xffff;
                let mut count = (w >> 16) & 0x3ff;
                if count == 0 {
                    count = 1024;
                }
                // At most 0x10000 + 1023: no wrap in u32.
                let end = addr + count;
                if addr < VS_CONTROL_END
                    || end > 0x10000
                    || (addr <= VS_EVENT && VS_EVENT < end)
                    || (addr <= VS_MMU_SETUP && VS_MMU_SETUP < end)
                {
                    return Err((EPERM, at));
                }
                // The header and the values, padded to a whole command word.
                (1 + count as usize).div_ceil(2) * 2
            }
            VC_NOP | VC_STALL => 2,
            _ => return Err((EPERM, at)),
        };
        if len > n - i {
            return Err((EINVAL, at));
        }
        i += len;
    }
    Ok(())
}

fn copy_in_user<T: FromBytes>(arg: *mut c_void) -> Result<T> {
    UserSlice::new(UserPtr::from_addr(arg as usize), core::mem::size_of::<T>()).reader().read::<T>()
}

fn copy_out_user<T: AsBytes>(arg: *mut c_void, v: &T) -> Result {
    UserSlice::new(UserPtr::from_addr(arg as usize), core::mem::size_of::<T>()).writer().write(v)
}

/// LEMNOS_VIP_IOC_LOAD: copies the program, checks it once, and keeps the copy the stream holds.
fn load(ps: &LvipStream, arg: *mut c_void) -> Result<c_long> {
    let mut a: lb::lemnos_vip_load = copy_in_user(arg)?;
    if a.reserved != 0 || a.bytes == 0 || a.bytes % 8 != 0 || a.bytes > lb::LEMNOS_VIP_MAX_PROGRAM_BYTES {
        return Err(EINVAL);
    }
    let mut words = KVVec::<u8>::with_capacity(a.bytes as usize, GFP_KERNEL)?;
    words.resize(a.bytes as usize, 0, GFP_KERNEL)?;
    // The copy is what is checked, and what runs.
    UserSlice::new(UserPtr::from_addr(a.data as usize), a.bytes as usize).reader().read_slice(&mut words)?;
    if let Err((e, at)) = scan(&words) {
        let at_addr = arg as usize + offset_of!(lb::lemnos_vip_load, refused_at);
        UserSlice::new(UserPtr::from_addr(at_addr), 4).writer().write_slice(&at.to_ne_bytes())?;
        return Err(e);
    }
    let mut prog = ps.prog.lock();
    let slot = prog.iter().position(|p| p.is_none()).ok_or(EBUSY)?;
    prog[slot] = Some(words);
    drop(prog);
    a.program = slot as u32 + 1;
    a.refused_at = 0;
    copy_out_user(arg, &a)?;
    Ok(0)
}

/// LEMNOS_VIP_IOC_UNLOAD: a job holds no reference to its program; the copy is released here.
fn unload(ps: &LvipStream, arg: *mut c_void) -> Result<c_long> {
    let a: lb::lemnos_vip_unload = copy_in_user(arg)?;
    if a.reserved != 0 || a.program == 0 || a.program as usize > LVIP_PROGRAMS {
        return Err(EINVAL);
    }
    let mut prog = ps.prog.lock();
    let words = prog[a.program as usize - 1].take();
    drop(prog);
    match words {
        Some(w) => {
            drop(w);
            Ok(0)
        }
        None => Err(ENOENT),
    }
}

/// The root as it is between jobs: the command buffer's window and nothing else.
fn tables_idle(l: &LemVip) {
    let t = l.tables();
    for s in 0..LVIP_SLOTS {
        let mut k = 0;
        while k < LVIP_PER_SLOT && t.mapped[s] != 0 {
            let pages = (t.mapped[s] as usize).min(LVIP_STLB_ENTRIES);
            // SAFETY: the slot's table was allocated when these pages were mapped, and holds `pages` entries.
            unsafe { ptr::write_bytes(t.stlb[s][k], 0, pages) };
            t.mapped[s] -= pages as u32;
            k += 1;
        }
    }
    for i in 1..LVIP_MTLB_ENTRIES {
        // SAFETY: the root table holds LVIP_MTLB_ENTRIES entries.
        unsafe { ptr::write_volatile(t.mtlb.add(i), 0) };
    }
    // SAFETY: the root table's first entry, as above.
    unsafe { ptr::write_volatile(t.mtlb, (t.stlb0_dma as u32 | LVIP_MTLB_PRESENT).to_le()) };
    // What the block reads next is what was written here.
    core::sync::atomic::fence(Ordering::Release);
}

/// The tables for slot `s` of the job, from its memory at `addr`. Nothing runs.
fn map_slot(l: &LemVip, s: usize, addr: u64, len: u64) -> Result {
    let t = l.tables();
    let pages = (len / PAGE) as usize;
    let first = (slot_addr(s) >> LVIP_MTLB_SHIFT) as usize;
    // The entries hold 40 bits of address.
    let end = addr.checked_add(len).ok_or(ENXIO)?;
    if end > LVIP_PHYS_LIMIT {
        return Err(ENXIO);
    }
    for k in 0..pages.div_ceil(LVIP_STLB_ENTRIES) {
        if t.stlb[s][k].is_null() {
            let mut dma: bindings::dma_addr_t = 0;
            // SAFETY: the device being probed; the table's size, and its handle written to `dma`.
            let p = unsafe { bindings::dma_alloc_attrs(l.dev, LVIP_STLB_BYTES, &raw mut dma, bindings::GFP_KERNEL, 0) };
            if p.is_null() {
                return Err(ENOMEM);
            }
            t.stlb[s][k] = p.cast();
            t.stlb_dma[s][k] = dma;
        }
        // SAFETY: the root table's entry for this table; `first + k` is below the root's size, since the
        // slot's span fits its MTLB range (checked by the build).
        unsafe {
            ptr::write_volatile(t.mtlb.add(first + k), (t.stlb_dma[s][k] as u32 | LVIP_MTLB_PRESENT).to_le())
        };
    }
    for p in 0..pages {
        // SAFETY: the table holds one entry per page the slot maps (`p` is below `pages`, which fits).
        unsafe {
            ptr::write_volatile(
                t.stlb[s][p / LVIP_STLB_ENTRIES].add(p % LVIP_STLB_ENTRIES),
                pte(addr + p as u64 * PAGE, true).to_le(),
            )
        };
    }
    t.mapped[s] = pages as u32;
    Ok(())
}

/// The command buffer for `prog`: flush the TLB, the program, then the module's end. Returns its words.
fn fill_command(l: &LemVip, prog: &[u8]) -> u32 {
    let pre: [u32; LVIP_PRE_WORDS] = [
        vc_state(VS_MMU_CONFIG, 1),
        0xffff_ff7f,
        vc_state(VS_SEMAPHORE, 1),
        VC_TOKEN_ALL,
        VC_STALL << 27,
        VC_TOKEN_ALL,
    ];
    let tail: [u32; LVIP_TAIL_WORDS] = [
        vc_state(VS_SEMAPHORE, 1),
        VC_TOKEN_ALL,
        VC_STALL << 27,
        VC_TOKEN_ALL,
        vc_state(VS_FLUSH, 1),
        0x00000c23,
        vc_state(VS_EVENT, 1),
        VC_EVENT_ID | VC_EVENT_FROM_PE,
        VC_END << 27,
        0,
    ];
    let t = l.tables();
    let n = prog.len() / 4;
    let mut i = 0;
    for w in pre {
        // SAFETY: the command buffer holds LVIP_CMD_BYTES / 4 words; the preamble is first, and the program
        // and the tail are checked to fit (the build's assertion and load's limit).
        unsafe { ptr::write_volatile(t.cmd.add(i), w.to_le()) };
        i += 1;
    }
    for j in 0..n {
        // SAFETY: as above.
        unsafe { ptr::write_volatile(t.cmd.add(i + j), word(prog, j).to_le()) };
    }
    i += n;
    for w in tail {
        // SAFETY: as above.
        unsafe { ptr::write_volatile(t.cmd.add(i), w.to_le()) };
        i += 1;
    }
    i as u32
}

/// Whether the block is idle and its clocks are in the idle state.
fn reset_done(l: &LemVip) -> bool {
    l.rd(VR_IDLE) == VR_IDLE_ALL
        && (l.rd(VR_CLOCK) & VR_CLOCK_IDLE_BITS) == VR_CLOCK_IDLE_BITS
        && (l.rd(VR_MMU_CONTROL) & VR_MMU_ENABLE) == 0
}

fn all_idle(l: &LemVip) -> bool {
    l.rd(VR_IDLE) == VR_IDLE_ALL
}

/// The vendor's software reset, seen to work twice in a row. Sleeps.
fn reset(l: &LemVip) -> Result {
    let mut good = 0;
    l.wr(VR_INT_ENABLE, 0);
    l.wr(VR_CLOCK, VR_CLOCK_INIT);
    let mut tries = 0;
    while good < 2 && tries < 10 {
        let pulse = (VR_PULSE_INIT & !(1 << 16)) | (1 << 17) | 1;
        let ctl = VR_CLOCK_INIT | VR_CLOCK_ISOLATE;

        l.wr(VR_GATING_MODULES, 0);
        l.wr(VR_PULSE, pulse);
        l.wr(VR_PULSE, pulse & !1);
        l.wr(VR_CLOCK, VR_CLOCK_INIT | VR_CLOCK_FSCALE_LOAD);
        udelay(5);
        l.wr(VR_CLOCK, VR_CLOCK_INIT);
        udelay(20);
        l.wr(VR_CLOCK, ctl);
        l.wr(VR_AHB_CONTROL, VR_AHB_RESET);
        udelay(20);
        l.wr(VR_AHB_CONTROL, 0);
        udelay(40);
        l.wr(VR_CLOCK, ctl & !VR_CLOCK_SOFT_RESET);
        l.wr(VR_CLOCK, ctl & !VR_CLOCK_SOFT_RESET & !VR_CLOCK_ISOLATE);
        udelay(50);
        if l.poll(LVIP_RESET_MS, reset_done) {
            good += 1;
        } else {
            good = 0;
        }
        tries += 1;
    }
    if good < 2 {
        return Err(EIO);
    }
    l.wr(VR_POWER, 0x00ff_ffff);
    l.wr(VR_REORDER, l.rd(VR_REORDER) & !(1 << 6));
    l.resets.fetch_add(1, Ordering::Relaxed);
    if all_idle(l) {
        Ok(())
    } else {
        Err(EIO)
    }
}

/// Whether the front end has taken its start (the fake's test form), and whether it is idle.
fn fake_started(l: &LemVip) -> bool {
    (l.rd(VR_FE_CONTROL) & VR_FE_ENABLE) == 0
}

fn fe_idle(l: &LemVip) -> bool {
    (l.rd(VR_IDLE) & VR_IDLE_FE) != 0
}

/// Points the block at the root, as the chip takes it. The MMU is off. Sleeps.
fn mmu_on(l: &LemVip) -> Result {
    let t = l.tables();
    if l.mmu == lb::LEMNOS_VIP_MMU_PD {
        l.wr(VR_PD_ENTRY, ((t.mtlb_dma >> 12) as u32) << 4 | LVIP_PD_MODE_1K);
        l.wr(VR_MMU_CONTROL, VR_MMU_PD | VR_MMU_ENABLE);
        return Ok(());
    }
    // The array's entry, then a command (run with the MMU off: addresses physical) selecting it.
    // SAFETY: the page table array's one entry, in its coherent page.
    unsafe {
        ptr::write_volatile(t.pta, (t.mtlb_dma as u32 | LVIP_PD_MODE_1K).to_le());
        ptr::write_volatile(t.pta.add(1), ((t.mtlb_dma >> 32) as u32).to_le());
    }
    l.wr(VR_PTA_LOW, t.pta_dma as u32);
    l.wr(VR_PTA_HIGH, (t.pta_dma >> 32) as u32);
    l.wr(VR_PTA_LOAD, 1);
    let setup: [u32; 10] = [
        vc_state(VS_PTA_CONFIG, 1),
        0xfffe_0000,
        vc_state(VS_MMU_SETUP, 1),
        0x0049_0000,
        vc_state(VS_SEMAPHORE, 1),
        VC_TOKEN,
        VC_STALL << 27,
        VC_TOKEN,
        VC_END << 27,
        0,
    ];
    for (i, w) in setup.iter().enumerate() {
        // SAFETY: the command buffer's first words, which hold the set-up (ten words of LVIP_CMD_BYTES).
        unsafe { ptr::write_volatile(t.cmd.add(i), w.to_le()) };
    }
    core::sync::atomic::fence(Ordering::Release);
    l.wr(VR_FE_ADDRESS, t.cmd_dma as u32);
    l.wr(VR_FE_CONTROL, VR_FE_ENABLE | (setup.len() / 2) as u32);
    if FAKE_REGS.get() && !l.poll(LVIP_RESET_MS, |l| fake_started(l)) {
        return Err(EIO);
    }
    if !l.poll(LVIP_RESET_MS, fe_idle) {
        return Err(EIO);
    }
    udelay(10);
    l.wr(VR_MMU_CONTROL, VR_MMU_ENABLE);
    Ok(())
}

/// Reset, set up, tables empty, interrupts on. Nothing may be running on it. Sleeps.
fn up(l: &LemVip) -> Result {
    reset(l)?;
    l.wr(VR_CLOCK, VR_CLOCK_INIT);
    l.wr(VR_CLOCK, VR_CLOCK_INIT & !VR_CLOCK_GPU_IDLE);
    l.wr(VR_AHB_CONTROL, VR_AHB_INIT);
    l.wr(VR_MEM_COUNTERS, !0);
    l.wr(VR_MEM_COUNTERS, 0);
    l.wr(VR_GATING, VR_GATING_INIT);
    tables_idle(l);
    mmu_on(l)?;
    l.wr(VR_GATING_MODULES, 0);
    l.wr(VR_PULSE, (l.rd(VR_PULSE) & !(1 << 16)) | (1 << 17));
    l.wr(VR_OUTSTANDING, l.rd(VR_OUTSTANDING) & !1);
    l.rd(VR_INT_ACK);
    if FAKE_REGS.get() {
        l.wr(VR_INT_ACK, 0);
    }
    l.wr(VR_INT_ENABLE, !0);
    Ok(())
}

/// The block's interrupt: an event ends the job (the end checks the block is idle), a fault does too.
unsafe extern "C" fn lvip_isr(_irq: c_int, data: *mut c_void) -> bindings::irqreturn_t {
    // SAFETY: the interrupt is freed before the block's reference is given up (probe's order).
    let l = unsafe { &*(data as *const LemVip) };
    let st = l.rd(VR_INT_ACK);
    if FAKE_REGS.get() {
        l.wr(VR_INT_ACK, 0);
    }
    if st == 0 {
        return bindings::irqreturn::IRQ_NONE;
    }
    let mut s = l.state.lock();
    if s.running && !s.ending && !s.wedged {
        s.int_status |= st;
        if st & (VR_INT_AXI | VR_INT_MMU) != 0 {
            s.fault = true;
            s.ending = true;
        } else if st & (1 << VC_EVENT_ID) != 0 {
            s.ending = true;
        }
        if s.ending {
            l.end.queue(l.wq);
        }
    }
    bindings::irqreturn::IRQ_HANDLED
}

/// A job outstanding too long, or a stream ending under one: its end is queued, which stops the block.
unsafe extern "C" fn lvip_watchdog(work: *mut bindings::work_struct) {
    // SAFETY: the work is the watchdog's, and the block is the container of the watchdog.
    let l = unsafe { &*(work.cast::<u8>().sub(offset_of!(LemVip, watchdog)).cast::<LemVip>()) };
    let mut s = l.state.lock();
    if s.running && !s.ending && !s.wedged {
        s.timed_out = true;
        s.ending = true;
        l.end.queue(l.wq);
    }
}

/// The end of the job on the block, on the ordered workqueue. An event with the block idle is a finished
/// job. Anything else (a fault, the watchdog, an event without the block going idle) resets the block,
/// and only a block seen reset lets go of the job's memory.
unsafe extern "C" fn lvip_end(work: *mut bindings::work_struct) {
    // SAFETY: the work is the block's end, and the block is its container.
    let l = unsafe { &*(work.cast::<u8>().sub(offset_of!(LemVip, end)).cast::<LemVip>()) };
    let s = l.state.lock();
    if !s.running || !s.ending || s.wedged {
        return;
    }
    l.watchdog.cancel();
    let fault = s.fault;
    let timed_out = s.timed_out;
    let status = s.int_status;
    drop(s);

    let clean = !fault && !timed_out && l.poll(LVIP_IDLE_MS, all_idle);
    let ret = if clean {
        tables_idle(l);
        Ok(())
    } else {
        up(l)
    };

    let mut s = l.state.lock();
    let ps = s.attached;
    if let Err(e) = ret {
        l.wedge(&mut s, ps, e.to_errno());
        return;
    }
    if clean {
        s.finished += 1;
    } else if fault {
        s.faults += 1;
        s.last_fault = status;
        dev_warn!(l.device(), "job faulted (0x{:08x}); block reset\n", status);
    } else {
        s.timeouts += 1;
        dev_warn!(l.device(), "job stopped after {} ms; block reset\n", l.timeout_ms);
    }
    s.running = false;
    s.ending = false;
    s.fault = false;
    s.timed_out = false;
    s.int_status = 0;
    if !ps.is_null() {
        // SAFETY: the attached stream lives until it is detached, which `state` orders.
        let p = unsafe { &*ps };
        let seq = s.sequence;
        s.sequence += 1;
        let bytes = if clean { s.slot.bytes } else { 0 };
        Handle(p.s.load(Ordering::Relaxed)).complete(&s.slot, bytes, seq, ktime_now() as u64);
        p.idle.wake_all();
        if !s.stopping {
            p.collect.queue(l.wq, 0);
        }
    }
}

/// The kernel's copy of a job: what it names, before anything is held.
fn validate(job: &lb::lemnos_vip_job) -> Result {
    if job.reserved != 0 || job.slots as usize > LVIP_SLOTS || job.program == 0 || job.program as usize > LVIP_PROGRAMS {
        return Err(EINVAL);
    }
    for i in 0..LVIP_SLOTS {
        let r = &job.slot[i];
        if i >= job.slots as usize {
            if r.addr != 0 || r.len != 0 {
                return Err(EINVAL);
            }
            continue;
        }
        if r.len == 0 || r.len > lb::LEMNOS_VIP_SLOT_SPAN as u64 || r.len & (PAGE - 1) != 0 || r.addr & (PAGE - 1) != 0 {
            return Err(EINVAL);
        }
    }
    Ok(())
}

/// Copies, checks, takes, maps and starts what is posted. The collector's lock held: nothing starts once
/// the stream is stopping. Process context.
fn collect_locked(ps: &LvipStream, stopped: bool) -> c_long {
    let l = &*ps.l;
    if stopped {
        return ENODEV.to_errno() as c_long;
    }
    {
        let st = l.state.lock();
        if st.wedged || st.stopping {
            return if st.wedged { EIO.to_errno() as c_long } else { ENODEV.to_errno() as c_long };
        }
        // One job at a time; the next is collected when it ends.
        if st.running {
            return 0;
        }
    }
    let s = Handle(ps.s.load(Ordering::Relaxed));
    let mut job = lb::lemnos_vip_job::default();
    let ret = s.job_peek(&mut job);
    if ret <= 0 {
        return ret;
    }
    if let Err(e) = validate(&job) {
        return refuse(l, e);
    }
    let prog = ps.prog.lock();
    if prog[job.program as usize - 1].is_none() {
        drop(prog);
        return refuse(l, EINVAL);
    }
    let mut slot = Slot::default();
    if let Err(e) = s.job_take(job.buffer, job.identity, &job.slot[..job.slots as usize], &mut slot) {
        drop(prog);
        return refuse(l, e);
    }
    // Nothing runs on the block, and the tables are as between jobs: they are the collector's.
    for i in 0..job.slots as usize {
        let r = s.range_addr(&slot.job.range[i]);
        let mapped = r.and_then(|addr| map_slot(l, i, addr, slot.job.range[i].len));
        if let Err(e) = mapped {
            drop(prog);
            // A slot in the pool resolves, but is no data: never started, dropped.
            tables_idle(l);
            s.cancel(&slot);
            return refuse(l, e);
        }
    }
    let words = fill_command(l, prog[job.program as usize - 1].as_deref().unwrap_or(&[]));
    drop(prog);
    // The tables and the commands before the start.
    core::sync::atomic::fence(Ordering::Release);

    let timeout = l.timeout_ms;
    let mut st = l.state.lock();
    if st.wedged || st.stopping {
        let e = if st.wedged { EIO } else { ENODEV };
        drop(st);
        tables_idle(l);
        s.cancel(&slot);
        return e.to_errno() as c_long;
    }
    st.slot = slot;
    st.running = true;
    st.int_status = 0;
    l.wr(VR_FE_ADDRESS, lb::LEMNOS_VIP_PROGRAM_ADDR);
    l.wr(VR_FE_CONTROL, VR_FE_ENABLE | (words / 2));
    l.watchdog.modify(l.wq, msecs_to_jiffies(timeout));
    0
}

/// A refused job: counted, and the stream ends with its errno (the C's refuse path).
fn refuse(l: &LemVip, e: Error) -> c_long {
    l.state.lock().refused += 1;
    e.to_errno() as c_long
}

/// LEMNOS_STREAM_IOC_KICK. An errno ends the stream.
unsafe extern "C" fn lvip_kick(priv_: *mut c_void) -> c_long {
    // SAFETY: the producer's private data: the stream's, alive until its release.
    let ps = unsafe { &*(priv_ as *const LvipStream) };
    let stopped = ps.collect_lock.lock();
    let ret = collect_locked(ps, *stopped);
    drop(stopped);
    ret
}

/// After a job ends: pick up what the consumer posted since, without a kick.
unsafe extern "C" fn lvip_collect_work(work: *mut bindings::work_struct) {
    // SAFETY: the work is the stream's collect work, which lives in its stream.
    let ps = unsafe { &*(work.cast::<u8>().sub(offset_of!(LvipStream, collect)).cast::<LvipStream>()) };
    let stopped = ps.collect_lock.lock();
    let ret = collect_locked(ps, *stopped);
    drop(stopped);
    if ret != 0 && ret != ENODEV.to_errno() as c_long {
        Handle(ps.s.load(Ordering::Relaxed)).fault(ret as c_int);
    }
}

/// Stops DMA: a job on the block is cut short by a reset, or the block is wedged and the stream keeps its
/// pins. The stream's lock is held by the core: it must not return until the block can't touch a buffer.
unsafe extern "C" fn lvip_stop(priv_: *mut c_void, s: *mut Stream) {
    // SAFETY: the producer's private data, alive until release, which the core calls after stop.
    let ps = unsafe { &*(priv_ as *const LvipStream) };
    let l = &*ps.l;
    let ps_ptr = priv_ as *mut LvipStream;

    {
        let mut st = l.state.lock();
        st.stopping = true;
        if st.running {
            l.watchdog.modify(l.wq, 0);
        }
    }
    ps.collect.cancel_sync();

    // A collection already running finishes first; none starts after.
    {
        let mut stopped = ps.collect_lock.lock();
        *stopped = true;
    }
    // One that started a job just now: the watchdog is set short.
    {
        let st = l.state.lock();
        if st.running {
            l.watchdog.modify(l.wq, 0);
        }
    }

    ps.idle.wait_until(msecs_to_jiffies(LVIP_TIMEOUT_MS), || {
        let st = l.state.lock();
        !st.running || st.wedged
    });

    let mut st = l.state.lock();
    let busy = st.running && !st.wedged;
    if busy {
        l.wedge(&mut st, ps_ptr, ETIMEDOUT.to_errno());
    }
    let wedged = st.wedged && st.stream_wedged;
    if st.attached == ps_ptr {
        st.attached = ptr::null_mut();
    }
    drop(st);

    if wedged {
        dev_crit!(l.device(), "stream wedged: keeping its buffers until a reboot\n");
        Handle(s).wedge();
    }
}

/// The stream's file is gone: nothing reaches the private data any more.
unsafe extern "C" fn lvip_release(priv_: *mut c_void) {
    // SAFETY: the private data made by lem_vip_create, released once; its programs go with it.
    drop(unsafe { KBox::from_raw(priv_ as *mut LvipStream) });
}

/// The stream's ioctls: the programs, and the block's status.
unsafe extern "C" fn lvip_ioctl(priv_: *mut c_void, cmd: c_uint, arg: *mut c_void) -> c_long {
    // SAFETY: the producer's private data, alive while the stream's file can reach it.
    let ps = unsafe { &*(priv_ as *const LvipStream) };
    let l = &*ps.l;
    let r = match cmd {
        IOC_LOAD => load(ps, arg),
        IOC_UNLOAD => unload(ps, arg),
        IOC_STATUS => {
            let st = l.state.lock();
            let status = lb::lemnos_vip_status {
                wedged: st.wedged as u32,
                chip: l.cid,
                finished: st.finished,
                refused: st.refused,
                faults: st.faults,
                timeouts: st.timeouts,
                last_fault: st.last_fault,
                resets: l.resets.load(Ordering::Relaxed),
            };
            drop(st);
            copy_out_user(arg, &status).map(|()| 0)
        }
        _ => Err(ENOTTY),
    };
    match r {
        Ok(v) => v as c_long,
        Err(e) => e.to_errno() as c_long,
    }
}

impl LvipStream {
    fn new(l: Arc<LemVip>) -> impl PinInit<Self> {
        pin_init!(Self {
            l,
            s: AtomicPtr::new(ptr::null_mut()),
            collect_lock <- new_mutex!(false),
            prog <- new_mutex!([const { None }; LVIP_PROGRAMS]),
            idle <- WaitQueue::new(c"lem_vip.idle"),
            collect <- DelayedWork::new(c"lem_vip.collect", lvip_collect_work),
        })
    }
}

/// LEMNOS_IOC_VIP_CREATE: a stream on this block, or its errno. `args` is the core's copy.
///
/// # Safety
/// `l` is a block from `lem_vip_probe` that has not been removed; `dev` is its device; `args` is a
/// `struct lemnos_vip_create` the caller owns.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_vip_create(l: *const LemVip, dev: *mut bindings::device, args: *mut c_void) -> c_long {
    // SAFETY: the caller's contract: a `struct lemnos_vip_create` in memory the core owns.
    let a = unsafe { &mut *(args as *mut lb::lemnos_vip_create) };
    // SAFETY: the block is alive while its probe reference is held (the caller's contract).
    let block = unsafe { &*l };
    if a.stream.job_bytes as usize != core::mem::size_of::<lb::lemnos_vip_job>() {
        return EINVAL.to_errno() as c_long;
    }
    // The stream holds its own reference to the block, so the block outlives a wedged stream.
    // SAFETY: `l` is the probe's reference, live; it is re-leaked by into_raw below.
    let probe_ref = unsafe { Arc::from_raw(l) };
    let stream_ref = probe_ref.clone();
    let _ = Arc::into_raw(probe_ref);

    let _create = block.create_lock.lock();
    let ps = match KBox::pin_init(LvipStream::new(stream_ref), GFP_KERNEL) {
        Ok(p) => p,
        Err(_) => return ENOMEM.to_errno() as c_long,
    };
    // SAFETY: the stream is never moved out of the box; it is freed only through lvip_release.
    let ps = KBox::into_raw(unsafe { core::pin::Pin::into_inner_unchecked(ps) });

    let refused = {
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
    if let Some(e) = refused {
        // SAFETY: the stream never existed, so nothing else holds the stream's private data.
        drop(unsafe { KBox::from_raw(ps) });
        return e.to_errno() as c_long;
    }

    let producer = Producer {
        stop: Some(lvip_stop),
        private: ps.cast(),
        job_bytes: core::mem::size_of::<lb::lemnos_vip_job>() as u32,
        kick: Some(lvip_kick),
        ioctl: Some(lvip_ioctl),
        mmap: None,
        release: Some(lvip_release),
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
    a.chip = block.cid;
    a.mmu = block.mmu;
    0
}

/// Frees the tables and the command buffer, whichever of them were made (probe's, and the slots').
fn free_tables(dev: *mut bindings::device, t: &Tables) {
    for s in 0..LVIP_SLOTS {
        for k in 0..LVIP_PER_SLOT {
            if !t.stlb[s][k].is_null() {
                // SAFETY: the table allocated for this slot, freed once here.
                unsafe { bindings::dma_free_attrs(dev, LVIP_STLB_BYTES, t.stlb[s][k].cast(), t.stlb_dma[s][k], 0) };
            }
        }
    }
    if !t.pta.is_null() {
        // SAFETY: the page allocated by probe, freed once here.
        unsafe { bindings::dma_free_attrs(dev, PAGE as usize, t.pta.cast(), t.pta_dma, 0) };
    }
    if !t.cmd.is_null() {
        // SAFETY: the command buffer allocated by probe, freed once here.
        unsafe { bindings::dma_free_attrs(dev, LVIP_CMD_BYTES as usize, t.cmd.cast(), t.cmd_dma, 0) };
    }
    if !t.stlb0.is_null() {
        // SAFETY: the window's table allocated by probe, freed once here.
        unsafe { bindings::dma_free_attrs(dev, LVIP_STLB_BYTES, t.stlb0.cast(), t.stlb0_dma, 0) };
    }
    if !t.mtlb.is_null() {
        // SAFETY: the root table allocated by probe, freed once here.
        unsafe { bindings::dma_free_attrs(dev, PAGE as usize, t.mtlb.cast(), t.mtlb_dma, 0) };
    }
}

/// The core's remove: the streams are gone (or wedged). Masks the interrupts, stops the watchdog and the
/// end, and frees the tables unless the block may still read them.
///
/// # Safety
/// `l` is from `lem_vip_probe`, and the device is being unbound.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_vip_remove(l: *const LemVip) {
    // SAFETY: the caller's contract: the block is alive until the device's release.
    let l = unsafe { &*l };
    let wedged = {
        let mut st = l.state.lock();
        l.wr(VR_INT_ENABLE, 0);
        st.attached = ptr::null_mut();
        st.wedged
    };
    l.watchdog.cancel_sync();
    l.end.cancel_sync();
    // A wedged block may still read its tables and commands.
    if !wedged {
        free_tables(l.dev, l.tables());
    }
}

/// devres: the PM reference the probe holds, given up at unbind.
unsafe extern "C" fn pm_put(dev: *mut c_void) {
    // SAFETY: the device whose reference the probe took.
    unsafe { bindings::__pm_runtime_idle(dev.cast(), RPM_ASYNC) };
}

/// devres release of the probe's reference to the block: after the interrupt, the end and the queue.
unsafe extern "C" fn lvip_put(data: *mut c_void) {
    // SAFETY: the reference probe made, given up once here.
    drop(unsafe { Arc::from_raw(data as *const LemVip) });
}

/// devres release of the workqueue, after the work that may run on it.
unsafe extern "C" fn lvip_destroy_wq(data: *mut c_void) {
    // SAFETY: the queue probe made, destroyed once here.
    Wq(data.cast()).destroy();
}

unsafe extern "C" {
    /// reset.h's devm_reset_control_array_get (core.c, exported); the flags are RESET_CONTROL_*.
    fn devm_reset_control_array_get(dev: *mut c_void, flags: c_uint) -> *mut c_void;
    fn reset_control_deassert(rstc: *mut c_void) -> c_int;
    /// pm.h's __pm_runtime_resume: the get-and-resume of pm_runtime_resume_and_get.
    fn __pm_runtime_resume(dev: *mut c_void, rpm_flags: c_int) -> c_int;
}

/// pm_runtime_resume_and_get: takes a runtime PM reference, resumed; a failed resume drops it again without
/// an idle request (pm_runtime_put_noidle).
fn resume_and_get(dev: *mut bindings::device) -> Result {
    // SAFETY: the device being probed.
    let r = unsafe { __pm_runtime_resume(dev.cast(), RPM_GET_PUT) };
    if r < 0 {
        // SAFETY: the device's usage count, which the failed get has just raised; dropped unless 0.
        unsafe { bindings::atomic_add_unless(&raw mut (*dev).power.usage_count, -1, 0) };
        return Err(Error::from_errno(r));
    }
    Ok(())
}

/// The chip's MMU, from its id: the table's, or unknown.
fn chip_mmu(cid: u32) -> Option<u32> {
    LVIP_CHIPS.iter().find(|(c, _)| *c == cid).map(|(_, m)| *m)
}

/// The tables and the command buffer, made once at probe and kept until unbind, into `t` (which is empty).
/// On failure what was made is in `t`, for free_tables.
fn alloc_tables(dev: *mut bindings::device, t: &mut Tables) -> Result {
    let alloc = |size: usize, dma: &mut bindings::dma_addr_t| -> *mut u32 {
        // SAFETY: the device being probed; the size and the handle are the caller's.
        unsafe { bindings::dma_alloc_attrs(dev, size, dma, bindings::GFP_KERNEL, 0) }.cast()
    };
    t.mtlb = alloc(PAGE as usize, &mut t.mtlb_dma);
    t.stlb0 = alloc(LVIP_STLB_BYTES, &mut t.stlb0_dma);
    t.cmd = alloc(LVIP_CMD_BYTES as usize, &mut t.cmd_dma);
    t.pta = alloc(PAGE as usize, &mut t.pta_dma);
    if t.mtlb.is_null() || t.stlb0.is_null() || t.cmd.is_null() || t.pta.is_null() {
        return Err(ENOMEM);
    }
    // The command buffer's window: its pages, read-only, and nothing else.
    for p in 0..LVIP_CMD_PAGES {
        let idx = (lb::LEMNOS_VIP_PROGRAM_ADDR as u64 >> 12) + p;
        // SAFETY: the window's table has LVIP_STLB_ENTRIES entries, and the command buffer's pages index
        // within them (the program address is below 16 MiB, checked by the build).
        unsafe { ptr::write(t.stlb0.add(idx as usize), pte(t.cmd_dma + p * PAGE, false).to_le()) };
    }
    Ok(())
}

/// Maps the block, takes it out of reset, reads its chip, and starts it. The interrupt and its devres
/// follow the C's order.
fn probe(pdev: *mut bindings::platform_device) -> Result<*const LemVip> {
    // SAFETY: the device being probed.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: as above.
    let res = unsafe { bindings::platform_get_resource(pdev, bindings::IORESOURCE_MEM, 0) };
    // SAFETY: a resource of the device, checked non-null first.
    if res.is_null() || unsafe { bindings::resource_size(res) } < VR_WINDOW {
        return Err(EINVAL);
    }
    // SAFETY: as above.
    let (start, size) = unsafe { ((*res).start, bindings::resource_size(res)) };
    let regs: *mut c_void = if FAKE_REGS.get() {
        // SAFETY: the device's own resource; devres unmaps it at unbind.
        let mem = from_err_ptr(unsafe { bindings::devm_memremap(dev, start, size as usize, bindings::MEMREMAP_WB as c_ulong) })?;
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "test_vip_fake_regs: registers are ordinary memory\n");
        mem
    } else {
        // SAFETY: as above.
        from_err_ptr(unsafe { bindings::devm_ioremap_resource(dev, res) })?
    };
    let mut timeout_ms = LVIP_TIMEOUT_MS;
    if TIMEOUT_MS.get() != 0 {
        timeout_ms = TIMEOUT_MS.get();
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "test_vip_timeout_ms: jobs get {} ms\n", timeout_ms);
    }

    // SAFETY: the device being probed; its interrupt number.
    let irq = unsafe { bindings::platform_get_irq(pdev, 0) };
    if irq <= 0 {
        return Err(EINVAL);
    }
    // Data at 40 bits through the tables; the tables and commands below 4 GiB.
    // SAFETY: the device being probed; the mask is 40 bits, then the coherent mask 32.
    let ret = unsafe { bindings::dma_set_mask(dev, (1 << 40) - 1) };
    if ret == 0 {
        // SAFETY: as above.
        to_result(unsafe { bindings::dma_set_coherent_mask(dev, 0xffff_ffff) })?;
    } else {
        return Err(Error::from_errno(ret));
    }
    let mut clks: *mut bindings::clk_bulk_data = ptr::null_mut();
    // SAFETY: the device being probed; the clocks are the device's, enabled and released with it.
    let n = unsafe { bindings::devm_clk_bulk_get_all_enabled(dev, &raw mut clks) };
    if n < 0 {
        return Err(Error::from_errno(n));
    }
    // SAFETY: the device being probed; its optional exclusive reset.
    let rst = from_err_ptr(unsafe { devm_reset_control_array_get(dev.cast(), RESET_CONTROL_OPTIONAL_EXCLUSIVE) })?;
    // SAFETY: the reset the device owns.
    to_result(unsafe { reset_control_deassert(rst) })?;
    // SAFETY: the device being probed; enabled as devm.
    to_result(unsafe { bindings::devm_pm_runtime_enable(dev) })?;
    resume_and_get(dev)?;
    // SAFETY: the device being probed; the put is given back at unbind.
    to_result(unsafe { bindings::devm_add_action_or_reset(dev, Some(pm_put), dev.cast()) })?;

    // The chip is read before anything else is set up: its id picks the MMU set-up.
    wr_reg(regs, VR_CLOCK, VR_CLOCK_INIT);
    let cid = rd_reg(regs, VR_CID);
    let Some(mmu) = chip_mmu(cid) else {
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_err!(d, "chip 0x{:08x}: not one whose MMU is known; refused\n", cid);
        return Err(ENODEV);
    };

    let wq = Wq::new_ordered(c"lemnos-vip")?;
    let block = match Arc::pin_init(
        pin_init!(LemVip {
            dev,
            regs,
            wq,
            irq,
            cid,
            mmu,
            timeout_ms,
            tables: TablesCell(core::cell::UnsafeCell::new(Tables::empty())),
            create_lock <- new_mutex!(()),
            state <- new_spinlock_irq!(VipState::default()),
            resets: AtomicU32::new(0),
            end <- Work::new(c"lem_vip.end", lvip_end),
            watchdog <- DelayedWork::new(c"lem_vip.watchdog", lvip_watchdog),
        }),
        GFP_KERNEL,
    ) {
        Ok(b) => b,
        Err(e) => {
            wq.destroy();
            return Err(e);
        }
    };
    if let Err(e) = alloc_tables(dev, block.tables()) {
        free_tables(dev, block.tables());
        wq.destroy();
        return Err(e);
    }
    let raw = Arc::into_raw(block);
    // The devres order of the C: the reference (released after the queue and the interrupt), then the
    // queue (destroyed after its work), then the interrupt, which is requested last. devm_add_action does
    // not run an action that fails to register, so each failure is undone here.
    // SAFETY: the device being probed; `raw` is the probe's reference, given up by lvip_put.
    if let Err(e) = to_result(unsafe { bindings::__devm_add_action(dev, Some(lvip_put), raw as *mut c_void, c"lem_vip".as_char_ptr()) }) {
        // SAFETY: the reference made above, not yet owned by devres.
        let b = unsafe { Arc::from_raw(raw) };
        free_tables(dev, b.tables());
        wq.destroy();
        return Err(e);
    }
    // SAFETY: the device being probed; the queue is destroyed by its devres, after the work.
    if let Err(e) = to_result(unsafe { bindings::__devm_add_action(dev, Some(lvip_destroy_wq), wq.0.cast(), c"lem_vip".as_char_ptr()) }) {
        // SAFETY: the reference devres holds (lvip_put, registered above) keeps the block alive.
        free_tables(dev, unsafe { &*raw }.tables());
        wq.destroy();
        return Err(e);
    }
    // SAFETY: the block is alive: the probe's reference, given up by the devres above.
    let block_ref = unsafe { &*raw };
    if let Err(e) = up(block_ref) {
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_err!(d, "block did not reset\n");
        wr_reg(regs, VR_INT_ENABLE, 0);
        free_tables(dev, block_ref.tables());
        return Err(e);
    }
    // SAFETY: the device being probed; the block is the handler's data, released after the handler.
    if let Err(e) = to_result(unsafe {
        bindings::devm_request_threaded_irq(dev, irq as c_uint, Some(lvip_isr), None, 0, bindings::dev_name(dev), raw as *mut c_void)
    }) {
        wr_reg(regs, VR_INT_ENABLE, 0);
        free_tables(dev, block_ref.tables());
        return Err(e);
    }
    // SAFETY: the device being probed.
    let d: &Device = unsafe { Device::from_raw(dev) };
    let set_up = if mmu == lb::LEMNOS_VIP_MMU_PD { "by page directory" } else { "by command" };
    dev_info!(d, "chip 0x{:08x}, MMU set up {}\n", cid, set_up);
    Ok(raw)
}

/// The core's probe: NULL is never returned; an error pointer when the block could not be made.
///
/// # Safety
/// `pdev` is the platform device being probed.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_vip_probe(pdev: *mut bindings::platform_device) -> *const LemVip {
    match probe(pdev) {
        Ok(l) => l,
        Err(e) => e.to_ptr(),
    }
}
