// SPDX-License-Identifier: GPL-2.0
//! Register jobs (lemnos_regjob.h), Rust (stage 1i of the port): a block whose register writes can make
//! it reach memory is run by the module from a list of register values and the caller's dma-bufs. What
//! a caller may write, which registers take addresses and how far the block reaches from each are the
//! block's profile (rs/regjob_ve.rs, rs/regjob_isp.rs); this file is the part every profile shares.
//!
//! A job is copied into kernel memory once and only the copy is checked and written. The module writes
//! every address register itself, from a buffer the session holds, after proving the job's offset and
//! length lie inside it and cover what the profile says the block will reach. Nothing a caller writes
//! reaches an address register, and nothing is clamped: a job that fails a check is refused before any
//! register is written.
//!
//! Buffer lifetime. A buffer is pinned (attached and mapped for the block) from import to release.
//! While the block may be using it, release is refused: during a job, and, for a block that keeps using
//! its addresses after a job completes (a capture path), until a later job has completed with other
//! addresses or the block has been reset. Closing the session or unbinding the block resets a block that
//! may still be using a buffer before anything is released. A block that can't be reset can't be shown
//! to have stopped: it is wedged, and what it may still use stays pinned for good.
//!
//! A job started without waiting (LEMNOS_REGJOB_RUN_NOWAIT) is collected by LEMNOS_REGJOB_IOC_WAIT,
//! which never sleeps: the deadline is checked there, and a job past it is handled as a timeout. Until
//! it is collected no other job starts. The registers a job reads back are read in the handler, at the
//! job's end.
//!
//! Two roles. The frames session runs jobs; the configuration session (on a profile that has one)
//! writes and reads registers that take no address, at once, and takes the block's other interrupt as
//! events. Every register write of either role is made under the state lock, and on a profile that
//! keeps bits a register is read, merged with the writer's bits and written back there, so a register
//! both roles write (the K230 ISP's isp_ctrl) keeps each role's bits. The handlers write only registers
//! no table lists (interrupt status and clear, the module's switch of a capture path), so they never
//! race a merge.
//!
//! Locking: `state` (a mutex) covers the sessions, the buffers, the live sets, a job's start and
//! collection and every register write; `irqs` (a spinlock, interrupts off) the completion, the job's
//! reads and slots the handler uses, and the event queue, and is the only lock the handlers take.
//!
//! The core (lemnos_core.c) holds a block as `struct lem_rj *`, which is the probe's reference to the
//! `Arc<Rj>`; each session file holds another, so a block unbound while a session is open lives until
//! it closes. Buffers belong to the frames role, one session at a time, and sit in the block; the copy
//! of a job (its writes, slots and reads) is made once, at probe, so a job allocates nothing.

use crate::bindings as lb;
use crate::regjob_profile::{Addrs, Ctx, Plain, Profile, Regs, MAX_ADDRS};
use core::{ffi::c_void, mem::MaybeUninit, ptr};
use kernel::{
    alloc::{KBox, KVBox},
    bindings,
    device::Device,
    error::{code::*, from_err_ptr, Error},
    ffi::{c_char, c_int, c_long, c_uint, c_ulong},
    ioctl::{_IOR, _IOW, _IOWR},
    new_mutex, new_spinlock_irq,
    prelude::*,
    static_lock_class,
    sync::{Arc, ArcBorrow, Mutex, SpinLockIrq},
    transmute::{AsBytes, FromBytes},
    types::Opaque,
    uaccess::{UserPtr, UserSlice},
};

const MAX_WRITES: usize = lb::LEMNOS_REGJOB_MAX_WRITES as usize;
const MAX_READS: usize = lb::LEMNOS_REGJOB_MAX_READS as usize;
const MAX_BUFFERS: usize = lb::LEMNOS_REGJOB_MAX_BUFFERS as usize;
const EVENTS: u32 = lb::LEMNOS_REGJOB_EVENTS;

// The live sets are u64 bit sets by buffer, and a job's slots a u32 by slot.
const _: () = assert!(MAX_BUFFERS <= 64);
const _: () = assert!(MAX_ADDRS <= 32);

const IOC_IMPORT: u32 = _IOWR::<lb::lemnos_regjob_import>(b'L' as u32, 0xf1);
const IOC_RELEASE: u32 = _IOW::<u32>(b'L' as u32, 0xf2);
const IOC_RUN: u32 = _IOWR::<lb::lemnos_regjob_run>(b'L' as u32, 0xf3);
const IOC_INFO: u32 = _IOR::<lb::lemnos_regjob_info>(b'L' as u32, 0xf4);
const IOC_WAIT: u32 = _IOWR::<lb::lemnos_regjob_wait>(b'L' as u32, 0xf5);
const IOC_WRITE: u32 = _IOW::<lb::lemnos_regjob_regs>(b'L' as u32, 0xf6);
const IOC_EVENT: u32 = _IOR::<lb::lemnos_regjob_event>(b'L' as u32, 0xf7);

// The uAPI structs are plain integers with no padding (the sizes are lemnos_regjob.h's).
const _: () = assert!(size_of::<lb::lemnos_regjob_write>() == 8);
const _: () = assert!(size_of::<lb::lemnos_regjob_addr>() == 24);
const _: () = assert!(size_of::<lb::lemnos_regjob_import>() == 24);
const _: () = assert!(size_of::<lb::lemnos_regjob_run>() == 64);
const _: () = assert!(size_of::<lb::lemnos_regjob_wait>() == 24);
const _: () = assert!(size_of::<lb::lemnos_regjob_regs>() == 32);
const _: () = assert!(size_of::<lb::lemnos_regjob_event>() == 16);
const _: () = assert!(size_of::<lb::lemnos_regjob_info>() == 48);

// SAFETY: integers only, no padding (asserted above): any bytes are a value, and every byte is one.
unsafe impl FromBytes for lb::lemnos_regjob_write {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_regjob_write {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_regjob_addr {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_regjob_addr {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_regjob_import {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_regjob_import {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_regjob_run {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_regjob_run {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_regjob_wait {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_regjob_wait {}
// SAFETY: as above.
unsafe impl FromBytes for lb::lemnos_regjob_regs {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_regjob_event {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_regjob_info {}

crate::param::param!(
    A733_VE,
    a733_ve: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks run as the A733 video engine's encoder through register jobs (vendor,name pairs); they must also be in owned="
);
crate::param::param!(
    K230_ISP,
    k230_isp: *mut c_char = c"".as_ptr().cast_mut().cast(),
    param_ops_charp,
    "charp",
    "Compatibles of the blocks run as the K230's ISP main path through register jobs (vendor,name pairs); they must also be in owned="
);
crate::param::param!(
    FAKE_REGS,
    test_rj_fake_regs: bool = false,
    param_ops_bool,
    "bool",
    "Test only: a register-job block's register window is ordinary memory (the test harness)"
);

unsafe extern "C" {
    /// lib/anon_inodes.c, exported. Not in the kernel crate's bindings.
    fn anon_inode_getfd(name: *const c_char, fops: *const c_void, priv_: *mut c_void, flags: c_int) -> c_int;
    /// drivers/reset/core.c, exported (reset.h is not in the kernel crate's bindings).
    fn __devm_reset_control_get(dev: *mut c_void, id: *const c_char, index: c_int, flags: c_uint) -> *mut c_void;
    fn reset_control_reset(rstc: *mut c_void) -> c_int;
    fn reset_control_assert(rstc: *mut c_void) -> c_int;
    fn reset_control_deassert(rstc: *mut c_void) -> c_int;
}

/// reset.h's RESET_CONTROL_OPTIONAL_EXCLUSIVE: FLAGS_BIT_OPTIONAL | FLAGS_BIT_ACQUIRED.
const RESET_CONTROL_OPTIONAL_EXCLUSIVE: c_uint = (1 << 1) | (1 << 2);

/// THIS_MODULE (only the module's own reference uses it: see `wedge`).
#[cfg(lemnos_modules)]
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

fn jiffies() -> c_ulong {
    // SAFETY: the kernel's tick count, a word always valid to read; read once, as READ_ONCE does.
    unsafe { ptr::read_volatile(&raw const bindings::jiffies) }
}

fn msecs_to_jiffies(ms: u32) -> c_ulong {
    // SAFETY: a pure conversion.
    unsafe { bindings::__msecs_to_jiffies(ms) }
}

fn now_ns() -> u64 {
    // SAFETY: reads the monotonic clock; callable in any context.
    unsafe { bindings::ktime_get() as u64 }
}

/// A held buffer: pinned (attached and mapped for the block) from import to release.
#[derive(Clone, Copy)]
struct Buf {
    /// Null: the index is free.
    buf: *mut bindings::dma_buf,
    att: *mut bindings::dma_buf_attachment,
    sgt: *mut bindings::sg_table,
    addr: u64,
    bytes: u64,
}

impl Buf {
    const FREE: Self = Self {
        buf: ptr::null_mut(),
        att: ptr::null_mut(),
        sgt: ptr::null_mut(),
        addr: 0,
        bytes: 0,
    };

    /// Unpins it, and frees its index.
    fn free(&mut self) {
        // SAFETY: a held buffer's mapping, attachment and reference, made by import and undone only here.
        unsafe { bindings::dma_buf_unmap_attachment_unlocked(self.att, self.sgt, bindings::dma_data_direction::DMA_BIDIRECTIONAL) };
        // SAFETY: as above.
        unsafe { bindings::dma_buf_detach(self.buf, self.att) };
        // SAFETY: as above.
        unsafe { bindings::dma_buf_put(self.buf) };
        *self = Self::FREE;
    }
}

/// The copy of a job or of a configuration write, and the frames role's buffers. Made once, at probe.
struct Scratch {
    w: [lb::lemnos_regjob_write; MAX_WRITES],
    a: [lb::lemnos_regjob_addr; MAX_ADDRS],
    reads: [u32; MAX_READS],
    values: [u32; MAX_READS],
    buf: [Buf; MAX_BUFFERS],
}

// SAFETY: integers and raw pointers only: all zeroes is a value (null pointers are a free buffer).
unsafe impl Zeroable for Scratch {}

/// What the state lock covers.
struct State {
    /// The sessions in each role, by id; 0: none.
    session: u64,
    config: u64,
    next_id: u64,
    /// Unbound: nothing touches the block again.
    gone: bool,
    wedged: bool,
    module_held: bool,
    /// Buffer bits the block may be using.
    cur: u64,
    prev: u64,
    jobs: u64,
    refused: u64,
    timeouts: u64,
    resets: u64,
    /// A job started and not yet collected.
    pending: bool,
    pending_reads: u32,
    /// In jiffies.
    deadline: c_ulong,
    timeout_ms: u32,
    s: KVBox<Scratch>,
}

/// The job the handler completes: the registers it reads back, where their values go, and the slots
/// a continuous profile switches to; and the configuration role's events.
struct IrqBufs {
    reads: [u32; MAX_READS],
    values: [u32; MAX_READS],
    ev: [lb::lemnos_regjob_event; EVENTS as usize],
}

// SAFETY: integers only.
unsafe impl Zeroable for IrqBufs {}

/// What the interrupt lock covers.
struct IrqState {
    armed: bool,
    done: bool,
    /// Continuous: switch to the job's addresses at its end.
    latch_pending: bool,
    status: u32,
    /// When the handler completed it.
    done_ns: u64,
    nreads: u32,
    slots: Addrs,
    /// The configuration session's id (as the state's), 0: none. Its events queue only while it is open.
    config: u64,
    ev_head: u32,
    ev_count: u32,
    ev_lost: u32,
    b: KBox<IrqBufs>,
}

impl IrqState {
    /// Queues an event for the configuration session, if one is open.
    fn push(&mut self, status: u32, now: u64) {
        if self.config == 0 {
            return;
        }
        if self.ev_count == EVENTS {
            self.ev_lost += 1;
            return;
        }
        let at = ((self.ev_head + self.ev_count) % EVENTS) as usize;
        self.b.ev[at] = lb::lemnos_regjob_event { status, lost: self.ev_lost, timestamp_ns: now };
        self.ev_lost = 0;
        self.ev_count += 1;
    }
}

/// A block run through register jobs.
#[pin_data]
pub(crate) struct Rj {
    dev: *mut bindings::device,
    p: &'static Profile,
    regs: Regs,
    /// Null: the block has none, and a timeout wedges it.
    rst: *mut c_void,
    irq: c_int,
    /// The configuration role's interrupt; 0: none of its own (the jobs' handler takes its events).
    event_irq: c_int,
    #[pin]
    state: Mutex<State>,
    #[pin]
    irqs: SpinLockIrq<IrqState>,
    #[pin]
    wq: Opaque<bindings::wait_queue_head>,
}

// SAFETY: the raw pointers are the device, its reset line and its mapped registers, which the kernel
// lets any thread use; everything that changes is under `state` or `irqs`.
unsafe impl Send for Rj {}
// SAFETY: as above.
unsafe impl Sync for Rj {}

/// Whether `offset` is a register of a window of `window` bytes.
fn in_window(window: u32, offset: u32) -> bool {
    offset % 4 == 0 && window >= 4 && offset <= window - 4
}

/// The mask a role's table gives `offset`; None: the table doesn't list it.
fn mask(table: &[Plain], offset: u32) -> Option<u32> {
    table
        .iter()
        .find(|r| offset >= r.offset && (offset - r.offset) / 4 < r.count)
        .map(|r| r.mask)
}

/// Every write in a role's table, with only its bits, on a register boundary in the window.
fn check_writes(p: &Profile, table: &[Plain], w: &[lb::lemnos_regjob_write]) -> Result {
    for w in w {
        if !in_window(p.window, w.offset) {
            return Err(EINVAL);
        }
        match mask(table, w.offset) {
            Some(m) if w.value & !m == 0 => {}
            _ => return Err(EPERM),
        }
    }
    Ok(())
}

fn copy_in<T: AsBytes + FromBytes>(addr: u64, out: &mut [T]) -> Result {
    let out = out.as_bytes_mut();
    UserSlice::new(UserPtr::from_addr(addr as usize), out.len()).reader().read_slice(out)
}

fn copy_out<T: AsBytes>(addr: u64, src: &[T]) -> Result {
    let src = src.as_bytes();
    UserSlice::new(UserPtr::from_addr(addr as usize), src.len()).writer().write_slice(src)
}

fn read_user<T: FromBytes>(arg: c_ulong) -> Result<T> {
    UserSlice::new(UserPtr::from_addr(arg as usize), size_of::<T>()).reader().read::<T>()
}

fn write_user<T: AsBytes>(arg: c_ulong, v: &T) -> Result {
    UserSlice::new(UserPtr::from_addr(arg as usize), size_of::<T>()).writer().write(v)
}

impl Rj {
    fn dev(&self) -> &Device {
        // SAFETY: the bound device, which outlives every use: after unbind (`gone`) nothing logs.
        unsafe { Device::from_raw(self.dev) }
    }

    /// One checked write. On a profile that keeps bits, the bits outside the writer's mask keep their
    /// value. State lock held: every merge is under it.
    fn write_checked(&self, table: &[Plain], w: &lb::lemnos_regjob_write) {
        let mask = mask(table, w.offset).unwrap_or(0);
        let mut value = w.value;
        if self.p.keep_bits && mask != u32::MAX {
            value |= self.regs.rd(w.offset) & !mask;
        }
        self.regs.wr(w.offset, value);
    }

    /// The block can't be shown to have stopped: what it may use stays pinned for good.
    fn wedge(&self, st: &mut State) {
        if !st.wedged {
            dev_crit!(self.dev(), "block did not finish and has no reset: its buffers stay held until a reboot\n");
        }
        st.wedged = true;
        if !st.module_held {
            st.module_held = true;
            // Without CONFIG_MODULES the kernel's __module_get is an empty inline (no reference to hold).
            #[cfg(lemnos_modules)]
            // SAFETY: the module is live (this is its code); the reference is never put.
            unsafe { bindings::__module_get(this_module()) };
        }
    }

    /// Pulses the reset line: 0, or the kernel's error.
    fn pulse_reset(&self) -> c_int {
        // SAFETY: the block's reset line, held (devm) while it is bound; `stop` runs only while bound.
        let mut ret = unsafe { reset_control_reset(self.rst) };
        if ret == -(bindings::ENOTSUPP as c_int) || ret == -(bindings::EOPNOTSUPP as c_int) {
            // SAFETY: as above.
            ret = unsafe { reset_control_assert(self.rst) };
            if ret == 0 {
                // SAFETY: a busy wait, callable in any context.
                unsafe { bindings::udelay(10) };
                // SAFETY: as above.
                ret = unsafe { reset_control_deassert(self.rst) };
            }
        }
        ret
    }

    /// Stops whatever the block is doing: interrupts off, the reset line pulsed, the block's own reset
    /// sequence. After it nothing is live.
    fn stop(&self, st: &mut State) {
        {
            let mut i = self.irqs.lock();
            i.armed = false;
            i.latch_pending = false;
            (self.p.quiet)(&self.regs);
        }
        st.pending = false;
        let mut reset = false;
        if self.rst.is_null() {
            self.wedge(st);
        } else {
            let ret = self.pulse_reset();
            if ret != 0 {
                dev_err!(self.dev(), "reset failed ({})\n", ret);
                self.wedge(st);
            } else {
                st.resets += 1;
                (self.p.reset_regs)(&self.regs);
                (self.p.quiet)(&self.regs);
                st.cur = 0;
                st.prev = 0;
                reset = true;
            }
        }
        // The configuration role keeps its interrupt, and learns that its registers are gone.
        let mut i = self.irqs.lock();
        if i.config != 0 {
            if !st.gone {
                if let Some(arm) = self.p.event_arm {
                    arm(&self.regs, true);
                }
            }
            if reset {
                i.push(lb::LEMNOS_REGJOB_EVENT_RESET, now_ns());
            }
        }
    }

    /// The frames session ends: the block is stopped if it may be using a buffer, and every buffer is
    /// released except, on a wedged block, the ones it may use, which keep their references until a
    /// reboot.
    fn end_session(&self, st: &mut State) {
        if !st.wedged && (st.cur | st.prev) != 0 {
            self.stop(st);
        }
        let live = st.cur | st.prev;
        for n in 0..MAX_BUFFERS {
            let b = &mut st.s.buf[n];
            if b.buf.is_null() {
                continue;
            }
            if st.wedged && live & (1 << n) != 0 {
                *b = Buf::FREE; // forgotten, its references held for good
                continue;
            }
            b.free();
        }
    }

    fn import(&self, st: &mut State, imp: &mut lb::lemnos_regjob_import) -> Result {
        let n = st.s.buf.iter().position(|b| b.buf.is_null()).ok_or(ENOSPC)?;
        // SAFETY: looks up the caller's fd; an error pointer when it is not a dma-buf.
        let buf = from_err_ptr(unsafe { bindings::dma_buf_get(imp.fd) })?;
        // SAFETY: the buffer just taken, and the bound device.
        let att = match from_err_ptr(unsafe { bindings::dma_buf_attach(buf, self.dev) }) {
            Ok(att) => att,
            Err(e) => {
                // SAFETY: the reference dma_buf_get took.
                unsafe { bindings::dma_buf_put(buf) };
                return Err(e);
            }
        };
        // SAFETY: the attachment just made.
        let sgt = match from_err_ptr(unsafe {
            bindings::dma_buf_map_attachment_unlocked(att, bindings::dma_data_direction::DMA_BIDIRECTIONAL)
        }) {
            Ok(sgt) => sgt,
            Err(e) => {
                // SAFETY: the attachment just made.
                unsafe { bindings::dma_buf_detach(buf, att) };
                // SAFETY: the reference dma_buf_get took.
                unsafe { bindings::dma_buf_put(buf) };
                return Err(e);
            }
        };
        let mut b = Buf { buf, att, sgt, addr: 0, bytes: 0 };
        // One contiguous range in the block's address space, or refused.
        // SAFETY: the mapping just made, live until freed.
        let (mut sg, nents) = unsafe { ((*sgt).sgl, (*sgt).nents) };
        // SAFETY: a mapped table has at least one entry.
        let start = unsafe { bindings::sg_dma_address(sg) };
        let mut next = start;
        let mut bytes = 0u64;
        for _ in 0..nents {
            // SAFETY: an entry of the mapped table, within its nents.
            let (addr, len) = unsafe { (bindings::sg_dma_address(sg), bindings::sg_dma_len(sg) as u64) };
            if addr != next {
                b.free();
                return Err(EINVAL);
            }
            next = next.wrapping_add(len);
            bytes += len;
            // SAFETY: as above; the last entry's next is never used.
            sg = unsafe { bindings::sg_next(sg) };
        }
        if bytes == 0 || start.checked_add(bytes).is_none() {
            b.free();
            return Err(EINVAL);
        }
        b.addr = start;
        b.bytes = bytes;
        st.s.buf[n] = b;
        imp.index = n as u32;
        imp.bytes = bytes;
        imp.addr = start;
        Ok(())
    }

    fn release_buf(&self, st: &mut State, index: u32) -> Result {
        let n = index as usize;
        if n >= MAX_BUFFERS || st.s.buf[n].buf.is_null() {
            return Err(ENOENT);
        }
        if (st.cur | st.prev) & (1 << n) != 0 {
            return Err(EBUSY);
        }
        st.s.buf[n].free();
        Ok(())
    }

    /// Every check, on the kernel's copy, before any register is written: the buffers the job uses and
    /// its checked slots.
    fn check(&self, st: &State, r: &lb::lemnos_regjob_run) -> Result<(u64, Addrs)> {
        let p = self.p;
        let s = &*st.s;
        let w = &s.w[..r.nwrites as usize];
        check_writes(p, p.plain, w)?;

        if r.naddrs as usize != p.slots.len() {
            return Err(EINVAL);
        }
        let mut slots = Addrs::ZERO;
        let mut seen = 0u32;
        let mut used = 0u64;
        for a in &s.a[..r.naddrs as usize] {
            let k = p.slots.iter().position(|sl| sl.reg == a.reg).ok_or(EINVAL)?;
            if seen & (1 << k) != 0 {
                return Err(EINVAL);
            }
            seen |= 1 << k;
            let n = a.buffer as usize;
            if n >= MAX_BUFFERS || s.buf[n].buf.is_null() {
                return Err(ENOENT);
            }
            let b = &s.buf[n];
            if a.bytes == 0 {
                return Err(EINVAL);
            }
            let end = a.offset.checked_add(a.bytes).ok_or(EFAULT)?;
            if end > b.bytes {
                return Err(EFAULT);
            }
            let addr = b.addr.checked_add(a.offset).ok_or(EFAULT)?;
            let top = addr.checked_add(a.bytes).ok_or(EFAULT)?;
            if addr & (p.slots[k].align as u64 - 1) != 0 {
                return Err(EINVAL);
            }
            if top > p.addr_limit {
                return Err(EINVAL); // the register can't hold it
            }
            slots.addr[k] = addr;
            slots.bytes[k] = a.bytes;
            used |= 1 << n;
        }

        if s.reads[..r.nreads as usize].iter().any(|&o| !in_window(p.window, o)) {
            return Err(EINVAL);
        }
        if !(p.start_ok)(r.start) {
            return Err(EPERM);
        }
        let mut need = [0u64; MAX_ADDRS];
        (p.check)(&Ctx { w, a: &slots }, &mut need)?;
        if (0..p.slots.len()).any(|k| need[k] > slots.bytes[k]) {
            return Err(EFAULT);
        }
        Ok((used, slots))
    }

    /// Writes the checked job and starts it.
    fn start(&self, st: &mut State, r: &lb::lemnos_regjob_run, used: u64, slots: &Addrs) {
        let p = self.p;
        let idle = st.cur | st.prev == 0;
        let w = &st.s.w[..r.nwrites as usize];

        // The addresses first, from the copy: nothing the caller wrote reaches them.
        for (k, sl) in p.slots.iter().enumerate() {
            self.regs.wr(sl.reg, slots.addr[k] as u32);
        }
        (p.derive)(&self.regs, &Ctx { w, a: slots });
        for w in w {
            self.write_checked(p.plain, w);
        }

        // What the jobs before left on a block that keeps going stays live until this one completes;
        // on any other block nothing is left by now.
        st.prev |= st.cur;
        st.cur = used;
        st.pending = true;
        st.pending_reads = r.nreads;
        st.timeout_ms = r.timeout_ms;
        st.deadline = jiffies().wrapping_add(msecs_to_jiffies(r.timeout_ms));

        let n = r.nreads as usize;
        let mut i = self.irqs.lock();
        i.done = false;
        i.status = 0;
        i.latch_pending = false;
        i.nreads = r.nreads;
        i.b.reads[..n].copy_from_slice(&st.s.reads[..n]);
        i.slots = *slots;
        if p.continuous {
            // An idle block takes the addresses now; a running one at its frame's end.
            if idle {
                if let Some(latch) = p.latch {
                    i.status = latch(&self.regs, slots);
                }
            } else {
                i.latch_pending = true;
            }
        }
        i.armed = true;
        (p.arm)(&self.regs);
        drop(i);
        self.regs.wr(p.start_reg, r.start);
    }

    fn done(&self) -> bool {
        self.irqs.lock().done
    }

    /// Sleeps, not interruptibly, until the handler completes the job or `timeout` jiffies pass:
    /// until it ends or is reset, the block is using the buffers.
    fn wait_done(&self, timeout: c_ulong) {
        let wq = self.wq.get();
        let mut entry = MaybeUninit::<bindings::wait_queue_entry>::uninit();
        let entry = entry.as_mut_ptr();
        // SAFETY: initialises the entry, on this stack frame, before any use.
        unsafe { bindings::init_wait_entry(entry, 0) };
        let mut left = timeout as c_long;
        loop {
            // SAFETY: the block's wait queue, initialised at probe, and the entry above.
            unsafe { bindings::prepare_to_wait_event(wq, entry, bindings::TASK_UNINTERRUPTIBLE as c_int) };
            if self.done() || left <= 0 {
                break;
            }
            // SAFETY: sleeps in the state set above.
            left = unsafe { bindings::schedule_timeout(left) };
        }
        // SAFETY: the entry prepared above, on this queue.
        unsafe { bindings::finish_wait(wq, entry) };
    }

    /// The started job's end, if it has come: its status and when, None while it runs, or ETIMEDOUT
    /// once it is past its deadline (the block is reset). Its values are in the scratch's.
    fn collect(&self, st: &mut State) -> Result<Option<(u32, u64)>> {
        let end = {
            let mut i = self.irqs.lock();
            if i.done {
                i.armed = false;
                let n = st.pending_reads as usize;
                st.s.values[..n].copy_from_slice(&i.b.values[..n]);
                Some((i.status, i.done_ns))
            } else {
                None
            }
        };
        let Some((status, at)) = end else {
            if (jiffies().wrapping_sub(st.deadline) as isize) < 0 {
                return Ok(None);
            }
            st.timeouts += 1;
            dev_warn!(self.dev(), "job did not finish in {} ms: resetting the block\n", st.timeout_ms);
            self.stop(st);
            return Err(ETIMEDOUT);
        };
        st.pending = false;
        st.jobs += 1;
        if !self.p.continuous {
            st.cur = 0; // the block is done with every buffer
        } else if status & lb::LEMNOS_REGJOB_STATUS_UNLATCHED == 0 {
            st.prev = 0; // this job's addresses are the ones in use now
        }
        // Not taken: the block may use either set, so both stay live.
        Ok(Some((status, at)))
    }

    /// The configuration role's interrupt, if pending: read, cleared and queued.
    fn event_irq_now(&self, i: &mut IrqState) -> bool {
        let status = match self.p.event {
            Some(event) => event(&self.regs),
            None => 0,
        };
        if status == 0 {
            return false;
        }
        i.push(status, now_ns());
        true
    }

    fn isr(&self) -> bool {
        let p = self.p;
        let mut i = self.irqs.lock();
        let mut handled = false;
        // One line for both roles: the events are this handler's too.
        if !p.config.is_empty() && self.event_irq == 0 && self.event_irq_now(&mut i) {
            handled = true;
        }
        if let Some(mut status) = (p.irq)(&self.regs) {
            handled = true;
            // A completion with no job armed is stale, and only cleared.
            if i.armed && !i.done {
                let n = i.nreads as usize;
                let b = &mut *i.b;
                for k in 0..n {
                    b.values[k] = self.regs.rd(b.reads[k]);
                }
                if i.latch_pending {
                    if let Some(latch) = p.latch {
                        status |= latch(&self.regs, &i.slots);
                    }
                    i.latch_pending = false;
                }
                i.done = true;
                i.done_ns = now_ns();
                i.status |= status;
                // SAFETY: the block's wait queue; wake_up(), from any context.
                unsafe { bindings::__wake_up(self.wq.get(), bindings::TASK_NORMAL, 1, ptr::null_mut()) };
            }
        }
        handled
    }
}

/// One opener's session: the file's private data.
struct Session {
    rj: Arc<Rj>,
    id: u64,
    /// The configuration role.
    config: bool,
}

impl Session {
    /// Whether this is still its block's session in its role. State lock held.
    fn current(&self, st: &State) -> bool {
        !st.gone && (if self.config { st.config } else { st.session }) == self.id
    }

    fn run(&self, r: &mut lb::lemnos_regjob_run) -> Result {
        let rj = &*self.rj;
        let mut st = rj.state.lock();
        if !self.current(&st) {
            return Err(ENODEV);
        }
        if st.wedged {
            return Err(EIO);
        }
        if st.pending {
            return Err(EBUSY);
        }
        let checked = (|| {
            if r.nwrites as usize > MAX_WRITES
                || r.naddrs as usize > MAX_ADDRS
                || r.nreads as usize > MAX_READS
                || r.timeout_ms == 0
                || r.timeout_ms > lb::LEMNOS_REGJOB_MAX_TIMEOUT_MS
                || r.flags & !lb::LEMNOS_REGJOB_RUN_NOWAIT != 0
                || r.reserved != 0
            {
                return Err(EINVAL);
            }
            // The copy: what is checked and what runs.
            let s = &mut *st.s;
            copy_in(r.writes, &mut s.w[..r.nwrites as usize])?;
            copy_in(r.addrs, &mut s.a[..r.naddrs as usize])?;
            copy_in(r.reads, &mut s.reads[..r.nreads as usize])?;
            rj.check(&st, r)
        })();
        let (used, slots) = match checked {
            Ok(c) => c,
            Err(e) => {
                st.refused += 1;
                return Err(e);
            }
        };
        rj.start(&mut st, r, used, &slots);
        if r.flags & lb::LEMNOS_REGJOB_RUN_NOWAIT != 0 {
            return Ok(());
        }
        rj.wait_done(msecs_to_jiffies(r.timeout_ms));
        if !rj.done() {
            st.deadline = jiffies(); // its time is up
        }
        // A deadline in the past times out, so a job not done is never still running here.
        let (status, _) = rj.collect(&mut st)?.ok_or(ETIMEDOUT)?;
        r.status = status;
        copy_out(r.values, &st.s.values[..r.nreads as usize])
    }

    fn wait(&self, w: &mut lb::lemnos_regjob_wait) -> Result {
        let rj = &*self.rj;
        let mut st = rj.state.lock();
        if !self.current(&st) {
            return Err(ENODEV);
        }
        if !st.pending || w.nreads != st.pending_reads {
            return Err(EINVAL);
        }
        let (status, at) = rj.collect(&mut st)?.ok_or(EAGAIN)?;
        w.status = status;
        w.timestamp_ns = at;
        copy_out(w.values, &st.s.values[..w.nreads as usize])
    }

    /// LEMNOS_REGJOB_IOC_WRITE: checked writes, then reads, at once.
    fn regs(&self, r: &lb::lemnos_regjob_regs) -> Result {
        let rj = &*self.rj;
        let p = rj.p;
        let table = if self.config { p.config } else { p.plain };
        if p.config.is_empty() {
            return Err(EINVAL); // no second role: the block's registers move with its jobs
        }
        if r.nwrites as usize > MAX_WRITES || r.nreads as usize > MAX_READS {
            return Err(EINVAL);
        }
        let (nw, nr) = (r.nwrites as usize, r.nreads as usize);
        let mut st = rj.state.lock();
        if !self.current(&st) {
            return Err(ENODEV);
        }
        if st.wedged {
            return Err(EIO);
        }
        // The copy: what is checked and what is written.
        copy_in(r.writes, &mut st.s.w[..nw])?;
        copy_in(r.reads, &mut st.s.reads[..nr])?;
        let checked = check_writes(p, table, &st.s.w[..nw]).and_then(|()| {
            if st.s.reads[..nr].iter().any(|&o| !in_window(p.window, o)) {
                Err(EINVAL)
            } else {
                Ok(())
            }
        });
        if let Err(e) = checked {
            st.refused += 1;
            return Err(e);
        }
        let s = &mut *st.s;
        for w in &s.w[..nw] {
            rj.write_checked(table, w);
        }
        for k in 0..nr {
            s.values[k] = rj.regs.rd(s.reads[k]);
        }
        copy_out(r.values, &s.values[..nr])
    }

    /// The next queued event; EAGAIN when none is.
    fn event(&self) -> Result<lb::lemnos_regjob_event> {
        let mut i = self.rj.irqs.lock();
        if i.config != self.id {
            return Err(ENODEV);
        }
        if i.ev_count == 0 {
            return Err(EAGAIN);
        }
        let e = i.b.ev[i.ev_head as usize];
        i.ev_head = (i.ev_head + 1) % EVENTS;
        i.ev_count -= 1;
        Ok(e)
    }

    fn info(&self) -> lb::lemnos_regjob_info {
        let rj = &*self.rj;
        let st = rj.state.lock();
        lb::lemnos_regjob_info {
            profile: rj.p.id,
            window_bytes: rj.p.window,
            wedged: st.wedged as u32,
            live: (st.cur | st.prev).count_ones(),
            jobs: st.jobs,
            refused: st.refused,
            timeouts: st.timeouts,
            resets: st.resets,
        }
    }

    fn ioctl(&self, cmd: u32, arg: c_ulong) -> Result {
        // A configuration session names no buffer; a frames session has no events.
        match cmd {
            IOC_RUN | IOC_WAIT | IOC_IMPORT | IOC_RELEASE if self.config => return Err(EINVAL),
            IOC_EVENT if !self.config => return Err(EINVAL),
            _ => {}
        }
        match cmd {
            IOC_WRITE => self.regs(&read_user(arg)?),
            IOC_EVENT => write_user(arg, &self.event()?),
            IOC_RUN => {
                let mut r = read_user(arg)?;
                self.run(&mut r)?;
                write_user(arg, &r)
            }
            IOC_WAIT => {
                let mut w = read_user(arg)?;
                self.wait(&mut w)?;
                write_user(arg, &w)
            }
            IOC_IMPORT => {
                let mut imp = read_user(arg)?;
                {
                    let mut st = self.rj.state.lock();
                    if !self.current(&st) {
                        return Err(ENODEV);
                    }
                    self.rj.import(&mut st, &mut imp)?;
                }
                // A failed copy leaves the buffer to the close: the caller has lost its index.
                write_user(arg, &imp)
            }
            IOC_RELEASE => {
                let index = read_user::<u32>(arg)?;
                let mut st = self.rj.state.lock();
                if !self.current(&st) {
                    return Err(ENODEV);
                }
                self.rj.release_buf(&mut st, index)
            }
            IOC_INFO => write_user(arg, &self.info()),
            _ => Err(ENOTTY),
        }
    }
}

/// The session's file operations.
#[repr(transparent)]
struct Fops(bindings::file_operations);

// SAFETY: never written; the kernel only reads it.
unsafe impl Sync for Fops {}

static LRJ_FOPS: Fops = Fops(bindings::file_operations {
    #[cfg(MODULE)]
    // `__this_module` is made by the kernel at load and lives until the module is unloaded.
    owner: (&raw const bindings::__this_module).cast_mut(),
    #[cfg(not(MODULE))]
    owner: ptr::null_mut(),
    release: Some(lrj_release),
    unlocked_ioctl: Some(lrj_ioctl),
    compat_ioctl: bindings::compat_ptr_ioctl,
    llseek: Some(bindings::noop_llseek),
    // SAFETY: every other operation absent (null), as C's designated initialiser leaves them.
    ..unsafe { core::mem::zeroed() }
});

unsafe extern "C" fn lrj_ioctl(file: *mut bindings::file, cmd: c_uint, arg: c_ulong) -> c_long {
    // SAFETY: the VFS calls this with a session file, whose private data is its Session until release.
    let s = unsafe { &*((*file).private_data as *const Session) };
    match s.ioctl(cmd, arg) {
        Ok(()) => 0,
        Err(e) => e.to_errno() as c_long,
    }
}

unsafe extern "C" fn lrj_release(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
    // SAFETY: the session file's private data, from KBox::into_raw in `lem_rj_open`; released once.
    let s = unsafe { KBox::from_raw((*file).private_data as *mut Session) };
    let rj = &*s.rj;
    let mut st = rj.state.lock();
    if st.session == s.id {
        rj.end_session(&mut st);
        st.session = 0;
    }
    if st.config == s.id {
        // Its interrupt off; what it wrote stays, as the frames role's writes do.
        let mut i = rj.irqs.lock();
        if !st.gone && !st.wedged {
            if let Some(arm) = rj.p.event_arm {
                arm(&rj.regs, false);
            }
        }
        i.config = 0;
        st.config = 0;
    }
    drop(st);
    drop(s); // may be the block's last reference
    0
}

/// Opens a session on the block in the role `o` asks for: the core's LEMNOS_IOC_REGJOB_OPEN.
///
/// # Safety
/// `rj` is a block from `lem_rj_probe`, not yet removed; `o` is valid.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_rj_open(rj: *const Rj, o: *mut lb::lemnos_regjob_open) -> c_long {
    // SAFETY: the caller's contract: the probe's reference is held, so the block is live.
    let rj: Arc<Rj> = unsafe { ArcBorrow::from_raw(rj) }.into();
    // SAFETY: the caller's contract.
    let o = unsafe { &mut *o };
    match open(rj, o) {
        Ok(()) => 0,
        Err(e) => e.to_errno() as c_long,
    }
}

fn open(rj: Arc<Rj>, o: &mut lb::lemnos_regjob_open) -> Result {
    let config = o.flags & lb::LEMNOS_REGJOB_OPEN_CONFIG != 0;
    if o.flags & !lb::LEMNOS_REGJOB_OPEN_CONFIG != 0 || (config && rj.p.config.is_empty()) {
        return Err(EINVAL);
    }
    let r = rj.clone();
    let mut st = r.state.lock();
    if st.gone {
        return Err(ENODEV);
    }
    if (if config { st.config } else { st.session }) != 0 {
        return Err(EBUSY);
    }
    st.next_id += 1;
    let id = st.next_id;
    let s = KBox::into_raw(KBox::new(Session { rj, id, config }, GFP_KERNEL)?);
    // SAFETY: the file takes the session; its operations are a static.
    let fd = unsafe {
        anon_inode_getfd(
            c"lemnos-regjob".as_ptr().cast(),
            (&raw const LRJ_FOPS.0).cast(),
            s.cast(),
            (bindings::O_RDWR | bindings::O_CLOEXEC) as c_int,
        )
    };
    if fd < 0 {
        // SAFETY: from KBox::into_raw above; no file holds it.
        drop(unsafe { KBox::from_raw(s) });
        return Err(Error::from_errno(fd));
    }
    // Set before the lock is let go: a close of the new fd from another thread waits for the lock,
    // then finds the session in its role.
    if config {
        st.config = id;
        let mut i = r.irqs.lock();
        i.config = id;
        i.ev_head = 0;
        i.ev_count = 0;
        i.ev_lost = 0;
        if !st.wedged {
            if let Some(arm) = r.p.event_arm {
                arm(&r.regs, true);
            }
        }
    } else {
        st.session = id;
    }
    drop(st);
    o.fd = fd;
    o.profile = r.p.id;
    o.window_bytes = r.p.window;
    o.abi = lb::LEMNOS_REGJOB_ABI;
    Ok(())
}

unsafe extern "C" fn lrj_isr(_irq: c_int, data: *mut c_void) -> bindings::irqreturn_t {
    // SAFETY: the block, registered with its interrupt; freed only after the interrupt is.
    let rj = unsafe { &*(data as *const Rj) };
    if rj.isr() {
        bindings::irqreturn::IRQ_HANDLED
    } else {
        bindings::irqreturn::IRQ_NONE
    }
}

unsafe extern "C" fn lrj_event_isr(_irq: c_int, data: *mut c_void) -> bindings::irqreturn_t {
    // SAFETY: as lrj_isr.
    let rj = unsafe { &*(data as *const Rj) };
    let mut i = rj.irqs.lock();
    if rj.event_irq_now(&mut i) {
        bindings::irqreturn::IRQ_HANDLED
    } else {
        bindings::irqreturn::IRQ_NONE
    }
}

fn profile_for(dev: *mut bindings::device) -> Option<&'static Profile> {
    // SAFETY: the bound device and a parameter's string, set before init.
    if unsafe { crate::lists::lem_device_listed(dev, A733_VE.get()) } {
        return Some(&crate::regjob_ve::PROFILE);
    }
    // SAFETY: as above.
    if unsafe { crate::lists::lem_device_listed(dev, K230_ISP.get()) } {
        return Some(&crate::regjob_isp::PROFILE);
    }
    None
}

/// DMA_BIT_MASK(fls64(limit - 1)): the device addresses below `limit`.
fn dma_mask(limit: u64) -> u64 {
    let bits = 64 - (limit - 1).leading_zeros();
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// An interrupt of the device, or its error (0 is EINVAL).
fn get_irq(pdev: *mut bindings::platform_device, index: u32) -> Result<c_int> {
    // SAFETY: the device being probed.
    let irq = unsafe { bindings::platform_get_irq(pdev, index) };
    match irq {
        i if i > 0 => Ok(i),
        0 => Err(EINVAL),
        e => Err(Error::from_errno(e)),
    }
}

fn request_irq(dev: *mut bindings::device, irq: c_int, handler: bindings::irq_handler_t, data: *const Rj) -> Result {
    // SAFETY: the device being probed; the block outlives the interrupt (lem_rj_remove frees it first).
    kernel::error::to_result(unsafe {
        bindings::devm_request_threaded_irq(dev, irq as c_uint, handler, None, 0, bindings::dev_name(dev), data as *mut c_void)
    })
}

/// Maps the block, turns its clocks on and takes it out of reset; at the end, interrupts on.
fn probe(pdev: *mut bindings::platform_device, p: &'static Profile) -> Result<*const Rj> {
    // SAFETY: the device being probed.
    let dev = unsafe { &raw mut (*pdev).dev };
    // SAFETY: as above.
    let res = unsafe { bindings::platform_get_resource(pdev, bindings::IORESOURCE_MEM, 0) };
    // SAFETY: a resource of the device, checked non-null first.
    if res.is_null() || unsafe { bindings::resource_size(res) } < p.window as u64 {
        return Err(EINVAL);
    }
    // SAFETY: as above.
    let (start, size) = unsafe { ((*res).start, bindings::resource_size(res)) };
    let base: *mut u8 = if FAKE_REGS.get() {
        // SAFETY: the device's own resource; devres unmaps it at unbind.
        let mem = from_err_ptr(unsafe { bindings::devm_memremap(dev, start, size as usize, bindings::MEMREMAP_WB as c_ulong) })?;
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "test_rj_fake_regs: registers are ordinary memory\n");
        mem.cast()
    } else {
        // SAFETY: as above.
        from_err_ptr(unsafe { bindings::devm_ioremap_resource(dev, res) })?.cast()
    };
    // SAFETY: the device being probed.
    kernel::error::to_result(unsafe { bindings::dma_set_mask_and_coherent(dev, dma_mask(p.addr_limit)) })?;
    let mut clks = ptr::null_mut();
    // SAFETY: as above; devres turns them off at unbind.
    let n = unsafe { bindings::devm_clk_bulk_get_all_enabled(dev, &mut clks) };
    if n < 0 {
        return Err(Error::from_errno(n));
    }
    // SAFETY: as above; devres puts it at unbind.
    let rst = from_err_ptr(unsafe { __devm_reset_control_get(dev.cast(), ptr::null(), 0, RESET_CONTROL_OPTIONAL_EXCLUSIVE) })?;
    if !rst.is_null() {
        // SAFETY: the line just taken.
        kernel::error::to_result(unsafe { reset_control_deassert(rst) })?;
    } else {
        // SAFETY: the device being probed.
        let d: &Device = unsafe { Device::from_raw(dev) };
        dev_warn!(d, "no reset line: a job that doesn't finish wedges the block\n");
    }
    // SAFETY: mapped above, at least the window (checked), until unbind; the Regs lives in the block,
    // which touches no register after lem_rj_remove.
    let regs = unsafe { Regs::new(base, p.window) };
    (p.reset_regs)(&regs);
    (p.quiet)(&regs);
    let irq = get_irq(pdev, p.irq_index)?;
    let event_irq = if p.config.is_empty() {
        0
    } else {
        // The same line as the jobs': lrj_isr takes both.
        match get_irq(pdev, p.event_irq_index)? {
            e if e == irq => 0,
            e => e,
        }
    };

    let s = KVBox::init(Scratch::init_zeroed(), GFP_KERNEL)?;
    let b = KBox::init(IrqBufs::init_zeroed(), GFP_KERNEL)?;
    let rj = Arc::pin_init(
        try_pin_init!(Rj {
            dev,
            p,
            regs,
            rst,
            irq,
            event_irq,
            state <- new_mutex!(State {
                session: 0,
                config: 0,
                next_id: 0,
                gone: false,
                wedged: false,
                module_held: false,
                cur: 0,
                prev: 0,
                jobs: 0,
                refused: 0,
                timeouts: 0,
                resets: 0,
                pending: false,
                pending_reads: 0,
                deadline: 0,
                timeout_ms: 0,
                s,
            }),
            irqs <- new_spinlock_irq!(IrqState {
                armed: false,
                done: false,
                latch_pending: false,
                status: 0,
                done_ns: 0,
                nreads: 0,
                slots: Addrs::ZERO,
                config: 0,
                ev_head: 0,
                ev_count: 0,
                ev_lost: 0,
                b,
            }),
            wq <- Opaque::ffi_init(|wq| {
                // SAFETY: initialises the queue in place, before the block is used.
                unsafe { bindings::__init_waitqueue_head(wq, c"lem_rj->wq".as_ptr().cast(), static_lock_class!().as_ptr()) }
            }),
        }? Error),
        GFP_KERNEL,
    )?;
    let raw = Arc::into_raw(rj);
    if let Err(e) = request_irq(dev, irq, Some(lrj_isr), raw) {
        // SAFETY: the reference into_raw made; no interrupt holds it.
        drop(unsafe { Arc::from_raw(raw) });
        return Err(e);
    }
    if event_irq != 0 {
        if let Err(e) = request_irq(dev, event_irq, Some(lrj_event_isr), raw) {
            // SAFETY: the interrupt just requested.
            unsafe { bindings::devm_free_irq(dev, irq as c_uint, raw as *mut c_void) };
            // SAFETY: the reference into_raw made; no interrupt holds it now.
            drop(unsafe { Arc::from_raw(raw) });
            return Err(e);
        }
    }
    let both = if !p.config.is_empty() && event_irq == 0 { ", both roles on one interrupt" } else { "" };
    // SAFETY: the device being probed.
    let d: &Device = unsafe { Device::from_raw(dev) };
    dev_info!(d, "register jobs as {}{}\n", p.name.to_str().unwrap_or("?"), both);
    Ok(raw)
}

/// The core's probe: NULL when the device is not listed for register jobs, else the block or an error
/// pointer.
///
/// # Safety
/// `pdev` is the platform device being probed.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_rj_probe(pdev: *mut bindings::platform_device) -> *const Rj {
    // SAFETY: the caller's contract.
    let Some(p) = profile_for(unsafe { &raw mut (*pdev).dev }) else {
        return ptr::null();
    };
    match probe(pdev, p) {
        Ok(rj) => rj,
        Err(e) => e.to_ptr(),
    }
}

/// The block is being unbound: a running job ends first (its timeout bounds that, as RUN holds the
/// state lock while it waits), then the block is stopped if it may be using a buffer, every buffer is
/// released, the configuration session gets nothing more (its calls answer ENODEV), and nothing
/// touches the registers again. Drops the probe's reference.
///
/// # Safety
/// `raw` is from `lem_rj_probe`, and is not used after.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_rj_remove(raw: *const Rj) {
    // SAFETY: the caller's contract: the probe's reference, given back once.
    let rj = unsafe { Arc::from_raw(raw) };
    let wedged = {
        let mut st = rj.state.lock();
        // The configuration session goes first: a reset below is no news to it.
        {
            let mut i = rj.irqs.lock();
            if i.config != 0 && !st.wedged {
                if let Some(arm) = rj.p.event_arm {
                    arm(&rj.regs, false);
                }
            }
            i.config = 0;
        }
        st.config = 0;
        if st.session != 0 {
            rj.end_session(&mut st);
            st.session = 0;
        }
        st.gone = true;
        let mut i = rj.irqs.lock();
        i.armed = false;
        if !st.wedged {
            (rj.p.quiet)(&rj.regs);
        }
        st.wedged
    };
    // SAFETY: the interrupts the probe requested with this block; freeing waits for their handlers.
    unsafe { bindings::devm_free_irq(rj.dev, rj.irq as c_uint, raw as *mut c_void) };
    if rj.event_irq != 0 {
        // SAFETY: as above.
        unsafe { bindings::devm_free_irq(rj.dev, rj.event_irq as c_uint, raw as *mut c_void) };
    }
    if !rj.rst.is_null() && !wedged {
        // SAFETY: the block's reset line, held until the unbind's devres runs, after this.
        unsafe { reset_control_assert(rj.rst) };
    }
}
