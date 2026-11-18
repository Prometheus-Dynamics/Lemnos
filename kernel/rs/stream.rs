// SPDX-License-Identifier: GPL-2.0
//! The frame stream's side of the block ports (stage 1k of the port): what the graphics DMA engine
//! (rs/gsdma.rs) and the VIP9000 (rs/vip.rs) share with it. The stream itself is lemnos_stream.c and
//! stays C; this file holds the types its interface (lemnos_internal.h) names, the calls the ports make
//! on it, and the kernel's waitqueue, delayed work and workqueue the two blocks run their jobs on.
//!
//! The layouts of `struct lem_job_range`, `struct lem_job`, `struct lem_stream_slot` and
//! `struct lem_stream_producer` are the C ones, checked below. Everything else is the kernel's.

use crate::bindings as lb;
use core::{
    ffi::{c_int, c_long, c_uint, c_void},
    mem::MaybeUninit,
    ptr,
    sync::atomic::{AtomicI64, AtomicU32, Ordering},
};
use kernel::{
    bindings,
    error::{Result, to_result},
    ffi::{c_char, c_ulong},
    prelude::*,
    types::Opaque,
};

/// LEM_JOB_HOLDS of lemnos_internal.h: the ranges one job may hold at once.
pub(crate) const JOB_HOLDS: usize = 13;

/// `struct lem_job_range`: a range a job names, resolved by the stream.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct JobRange {
    pub(crate) region: u32,
    pub(crate) offset: u64,
    pub(crate) len: u64,
}

/// `struct lem_job`: what a held job names.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct Job {
    pub(crate) identity: u64,
    pub(crate) buffer: u32,
    pub(crate) count: u32,
    pub(crate) range: [JobRange; JOB_HOLDS],
}

/// `struct lem_stream_slot`: one frame a block holds, from the stream until it is completed or cancelled.
#[repr(C)]
#[derive(Default)]
pub(crate) struct Slot {
    pub(crate) job: Job,
    pub(crate) addr: u64,
    pub(crate) bytes: u32,
}

/// `struct lem_stream`, opaque: only the stream's code reads it.
pub(crate) enum Stream {}

/// `struct lem_stream_producer`: what a block that takes its jobs from the ring provides. The stream
/// calls `stop` once, `kick`, `ioctl`, `mmap` and `release` as the C interface says.
#[repr(C)]
pub(crate) struct Producer {
    pub(crate) stop: Option<unsafe extern "C" fn(*mut c_void, *mut Stream)>,
    pub(crate) private: *mut c_void,
    pub(crate) job_bytes: u32,
    pub(crate) kick: Option<unsafe extern "C" fn(*mut c_void) -> c_long>,
    pub(crate) ioctl: Option<unsafe extern "C" fn(*mut c_void, c_uint, *mut c_void) -> c_long>,
    pub(crate) mmap:
        Option<unsafe extern "C" fn(*mut c_void, *mut bindings::vm_area_struct) -> c_int>,
    pub(crate) release: Option<unsafe extern "C" fn(*mut c_void)>,
}

const _: () = assert!(core::mem::size_of::<JobRange>() == 24);
const _: () = assert!(core::mem::size_of::<Job>() == 328);
const _: () = assert!(core::mem::size_of::<Slot>() == 344);
const _: () = assert!(core::mem::size_of::<Producer>() == 56);

unsafe extern "C" {
    /// lemnos_stream.c: makes the stream the producer drives, and the file for it (`out`).
    pub(crate) fn lem_stream_create_producer(
        dev: *mut c_void,
        c: *mut lb::lemnos_stream_create,
        producer: *const Producer,
        out: *mut *mut Stream,
    ) -> c_long;
    fn lem_stream_fault(s: *mut Stream, err: c_int);
    fn lem_stream_complete(
        s: *mut Stream,
        slot: *const Slot,
        bytes: u32,
        sequence: u64,
        timestamp_ns: u64,
    );
    fn lem_stream_cancel(s: *mut Stream, slot: *const Slot);
    fn lem_stream_wedge(s: *mut Stream);
    fn lem_stream_range_addr(s: *mut Stream, r: *const JobRange, addr: *mut u64) -> c_int;
    fn lem_stream_job_peek(s: *mut Stream, job: *mut c_void, bytes: usize) -> c_int;
    fn lem_stream_job_take(
        s: *mut Stream,
        buffer: u32,
        identity: u64,
        range: *const lb::lemnos_stream_range,
        count: u32,
        slot: *mut Slot,
    ) -> c_int;
}

/// A stream the block drives: the pointer the stream's calls take. Valid while the stream exists, and
/// the block calls it only inside the producer's callbacks, which the stream makes while it is alive.
#[derive(Clone, Copy)]
pub(crate) struct Handle(pub(crate) *mut Stream);

impl Handle {
    /// The job's end: the frame is published with `bytes` (0 for a frame that went wrong).
    pub(crate) fn complete(self, slot: &Slot, bytes: u32, sequence: u64, timestamp_ns: u64) {
        // SAFETY: the stream is alive (see Handle), and the slot is the one the stream holds.
        unsafe { lem_stream_complete(self.0, slot, bytes, sequence, timestamp_ns) };
    }

    /// A held slot that was never written: given back unpublished.
    pub(crate) fn cancel(self, slot: &Slot) {
        // SAFETY: as complete.
        unsafe { lem_stream_cancel(self.0, slot) };
    }

    /// The block can't go on: the consumer learns with `err` and the stream ends.
    pub(crate) fn fault(self, err: c_int) {
        // SAFETY: as complete.
        unsafe { lem_stream_fault(self.0, err) };
    }

    /// The block's DMA can't be shown to have stopped: the stream keeps its pins for good.
    pub(crate) fn wedge(self) {
        // SAFETY: as complete; called once, when the stream is being stopped.
        unsafe { lem_stream_wedge(self.0) };
    }

    /// The device address of a held job's range, computed by the stream.
    pub(crate) fn range_addr(self, r: &JobRange) -> Result<u64> {
        let mut addr = 0u64;
        // SAFETY: the stream is alive; the range and the address are valid for the call.
        to_result(unsafe { lem_stream_range_addr(self.0, r, &raw mut addr) })?;
        Ok(addr)
    }

    /// Reads the job at the front of the ring into `job`: 1 with a job, 0 with none, -errno.
    pub(crate) fn job_peek<T>(self, job: &mut T) -> c_long {
        // SAFETY: the stream is alive; `job` is `size_of::<T>()` bytes, which the stream writes whole.
        unsafe {
            lem_stream_job_peek(self.0, (job as *mut T).cast(), core::mem::size_of::<T>()) as c_long
        }
    }

    /// Resolves the job's ranges and, if they all resolve, holds the job's frame in `slot`.
    pub(crate) fn job_take(
        self,
        buffer: u32,
        identity: u64,
        ranges: &[lb::lemnos_stream_range],
        slot: &mut Slot,
    ) -> Result {
        // SAFETY: the stream is alive; `ranges` holds `count` entries and `slot` is written whole.
        to_result(unsafe {
            lem_stream_job_take(
                self.0,
                buffer,
                identity,
                ranges.as_ptr(),
                ranges.len() as u32,
                slot,
            )
        })
    }
}

/// A struct wait_queue_head, woken from any context and waited on with a timeout.
#[pin_data]
pub(crate) struct WaitQueue {
    #[pin]
    wq: Opaque<bindings::wait_queue_head>,
}

impl WaitQueue {
    /// init_waitqueue_head, under the given class.
    pub(crate) fn new(name: &'static core::ffi::CStr) -> impl PinInit<Self> {
        pin_init!(Self {
            wq <- Opaque::ffi_init(move |p| {
                // SAFETY: initialises the queue in place, before any use; the name and class are static.
                unsafe {
                    bindings::__init_waitqueue_head(p, name.as_char_ptr(), kernel::static_lock_class!().as_ptr())
                }
            }),
        })
    }

    /// wake_up_all: every waiter, from any context.
    pub(crate) fn wake_all(&self) {
        // SAFETY: the queue is initialised (new); __wake_up is the exported wake.
        unsafe { bindings::__wake_up(self.wq.get(), bindings::TASK_NORMAL, 0, ptr::null_mut()) };
    }

    /// wait_event_timeout(q, done(), timeout): sleeps until `done` holds or `timeout` jiffies pass.
    /// Process context only. The result is `done` as it was last seen; the caller checks it again.
    pub(crate) fn wait_until(&self, timeout: c_ulong, mut done: impl FnMut() -> bool) {
        let wq = self.wq.get();
        let mut entry = MaybeUninit::<bindings::wait_queue_entry>::uninit();
        let entry = entry.as_mut_ptr();
        // SAFETY: initialises the entry, on this stack frame, before any use.
        unsafe { bindings::init_wait_entry(entry, 0) };
        let mut left: isize = timeout as isize;
        loop {
            // SAFETY: the queue and the entry above.
            unsafe {
                bindings::prepare_to_wait_event(wq, entry, bindings::TASK_UNINTERRUPTIBLE as c_int)
            };
            if done() || left <= 0 {
                break;
            }
            // SAFETY: sleeps in the state set above.
            left = unsafe { bindings::schedule_timeout(left) } as isize;
        }
        // SAFETY: the entry prepared above, on this queue.
        unsafe { bindings::finish_wait(wq, entry) };
    }
}

/// `struct delayed_work`, initialised as INIT_DELAYED_WORK does: the work's function and its timer.
#[pin_data]
pub(crate) struct DelayedWork {
    #[pin]
    dw: Opaque<bindings::delayed_work>,
}

impl DelayedWork {
    /// INIT_DELAYED_WORK(work, func), with the name and class of the kernel crate's DelayedWork.
    pub(crate) fn new(
        name: &'static core::ffi::CStr,
        func: unsafe extern "C" fn(*mut bindings::work_struct),
    ) -> impl PinInit<Self> {
        pin_init!(Self {
            dw <- Opaque::ffi_init(move |p: *mut bindings::delayed_work| {
                // SAFETY: the work and its timer are initialised in place, before any queueing; the
                // timer is the work's own, so delayed_work_timer_fn is its function.
                unsafe {
                    bindings::init_work_with_key(
                        &raw mut (*p).work,
                        Some(func),
                        false,
                        name.as_char_ptr(),
                        kernel::static_lock_class!().as_ptr(),
                    );
                    bindings::timer_init_key(
                        &raw mut (*p).timer,
                        Some(bindings::delayed_work_timer_fn),
                        bindings::TIMER_IRQSAFE,
                        name.as_char_ptr(),
                        kernel::static_lock_class!().as_ptr(),
                    );
                }
            }),
        })
    }

    /// queue_delayed_work(wq, this, delay): unless already queued.
    pub(crate) fn queue(&self, wq: Wq, delay: c_ulong) {
        // SAFETY: initialised (new), and lives as long as the owner; WORK_CPU_UNBOUND is the kernel's.
        unsafe {
            bindings::queue_delayed_work_on(
                bindings::wq_misc_consts_WORK_CPU_UNBOUND as c_int,
                wq.0,
                self.dw.get(),
                delay,
            )
        };
    }

    /// mod_delayed_work(wq, this, delay): queued (or moved) to run after `delay`.
    pub(crate) fn modify(&self, wq: Wq, delay: c_ulong) {
        // SAFETY: as queue.
        unsafe {
            bindings::mod_delayed_work_on(
                bindings::wq_misc_consts_WORK_CPU_UNBOUND as c_int,
                wq.0,
                self.dw.get(),
                delay,
            )
        };
    }

    /// cancel_delayed_work(this): true when it was pending and is now off the queue.
    pub(crate) fn cancel(&self) -> bool {
        // SAFETY: as queue.
        unsafe { bindings::cancel_delayed_work(self.dw.get()) }
    }

    /// cancel_delayed_work_sync(this): off the queue, and not running, when it returns.
    pub(crate) fn cancel_sync(&self) {
        // SAFETY: as queue.
        unsafe { bindings::cancel_delayed_work_sync(self.dw.get()) };
    }
}

/// A workqueue the block runs its jobs on. Ordered: one item at a time.
#[derive(Clone, Copy)]
pub(crate) struct Wq(pub(crate) *mut bindings::workqueue_struct);

// SAFETY: a workqueue is usable from any context; the owner destroys it once, after its work is done.
unsafe impl Send for Wq {}
unsafe impl Sync for Wq {}

unsafe extern "C" {
    /// alloc_workqueue, the variadic function behind alloc_ordered_workqueue (linux/workqueue.h).
    fn alloc_workqueue_noprof(
        fmt: *const c_char,
        flags: c_uint,
        max_active: c_int,
        ...
    ) -> *mut bindings::workqueue_struct;
    fn destroy_workqueue(wq: *mut bindings::workqueue_struct);
}

/// WQ_UNBOUND | __WQ_ORDERED, as alloc_ordered_workqueue passes them, with one item at a time.
const WQ_ORDERED: c_uint = (1 << 1) | (1 << 17);

impl Wq {
    /// alloc_ordered_workqueue(name, 0): the queue, or ENOMEM.
    pub(crate) fn new_ordered(name: &'static core::ffi::CStr) -> Result<Self> {
        // SAFETY: the format is a static C string with no conversions; the flags and count are the kernel's.
        let wq = unsafe { alloc_workqueue_noprof(name.as_char_ptr(), WQ_ORDERED, 1) };
        if wq.is_null() {
            return Err(kernel::error::code::ENOMEM);
        }
        Ok(Wq(wq))
    }

    /// destroy_workqueue. Once only, after every item is done, by the owner.
    pub(crate) fn destroy(self) {
        // SAFETY: the queue from new_ordered, destroyed once by its owner.
        unsafe { destroy_workqueue(self.0) };
    }
}

/// ktime_get(): the monotonic clock, in ns.
pub(crate) fn ktime_now() -> bindings::ktime_t {
    // SAFETY: the monotonic clock may be read from any process or interrupt context.
    unsafe { bindings::ktime_get() }
}

/// The delay of `ms` ms, in ktime units; the sum is the kernel's ktime_add_ms.
pub(crate) fn ktime_add_ms(t: bindings::ktime_t, ms: u64) -> bindings::ktime_t {
    t + ms as bindings::ktime_t * 1_000_000
}

/// msecs_to_jiffies of a constant or a value, as the exported function.
pub(crate) fn msecs_to_jiffies(ms: u32) -> c_ulong {
    // SAFETY: the exported conversion, valid for any value.
    unsafe { bindings::__msecs_to_jiffies(ms) }
}

/// A sleep of between `min` and `max` microseconds, in process context.
pub(crate) fn usleep_range(min: u64, max: u64) {
    // SAFETY: sleeps in TASK_UNINTERRUPTIBLE, which the kernel's usleep_range does; process context only.
    unsafe {
        bindings::usleep_range_state(
            min as c_ulong,
            max as c_ulong,
            bindings::TASK_UNINTERRUPTIBLE,
        )
    };
}

/// Holds a reference to the module while a block may still touch its memory (the wedge). Without
/// CONFIG_MODULES the kernel's __module_get is an empty inline, so there is nothing to hold.
pub(crate) fn module_hold() {
    #[cfg(lemnos_modules)]
    // SAFETY: the module is live (this is its code); the reference is never put.
    unsafe {
        bindings::__module_get(this_module())
    };
}

/// THIS_MODULE, as rs/regjob.rs takes it: the kernel's own `__this_module` for the module.
#[cfg(lemnos_modules)]
fn this_module() -> *mut bindings::module {
    #[cfg(MODULE)]
    {
        (&raw const bindings::__this_module).cast_mut()
    }
    #[cfg(not(MODULE))]
    {
        ptr::null_mut()
    }
}

/// `struct work_struct`, initialised as INIT_WORK does: its function, on the kernel's name and class.
#[pin_data]
pub(crate) struct Work {
    #[pin]
    w: Opaque<bindings::work_struct>,
}

impl Work {
    /// INIT_WORK(work, func), with a static name and class.
    pub(crate) fn new(
        name: &'static core::ffi::CStr,
        func: unsafe extern "C" fn(*mut bindings::work_struct),
    ) -> impl PinInit<Self> {
        pin_init!(Self {
            w <- Opaque::ffi_init(move |p: *mut bindings::work_struct| {
                // SAFETY: the work is initialised in place, before any queueing; name and class are static.
                unsafe {
                    bindings::init_work_with_key(
                        p,
                        Some(func),
                        false,
                        name.as_char_ptr(),
                        kernel::static_lock_class!().as_ptr(),
                    )
                }
            }),
        })
    }

    /// queue_work(wq, this): on the ordered queue, unless already queued.
    pub(crate) fn queue(&self, wq: Wq) {
        // SAFETY: initialised (new), and lives as long as its owner; WORK_CPU_UNBOUND is the kernel's.
        unsafe {
            bindings::queue_work_on(
                bindings::wq_misc_consts_WORK_CPU_UNBOUND as c_int,
                wq.0,
                self.w.get(),
            )
        };
    }

    /// cancel_work_sync(this): off the queue, and not running, when it returns.
    pub(crate) fn cancel_sync(&self) {
        // SAFETY: as queue.
        unsafe { bindings::cancel_work_sync(self.w.get()) };
    }
}

/// A busy wait of `us` microseconds, for the reset's steps.
pub(crate) fn udelay(us: u64) {
    // SAFETY: the kernel's delay: a busy wait, valid in process and interrupt context.
    unsafe { bindings::udelay(us as c_ulong) };
}

/// The kernel's default rate limit (DEFAULT_RATELIMIT_INTERVAL and _BURST): ten lines in five seconds.
const RATE_INTERVAL_NS: i64 = 5_000_000_000;
const RATE_BURST: u32 = 10;

/// What `dev_warn_ratelimited` keeps for one call site: the window's start, the lines it has let
/// through and the lines it has dropped. The kernel's own (`struct ratelimit_state` and
/// `___ratelimit`) is a static the C macro makes per call site, with a lock in it that only a C
/// initialiser can make, so this is kept here: three atomics, no lock and no allocation. Two callers
/// racing at a window's edge may let one line more or fewer through, which a rate limit can take.
pub(crate) struct RateLimit {
    start: AtomicI64,
    passed: AtomicU32,
    missed: AtomicU32,
}

impl RateLimit {
    pub(crate) const fn new() -> Self {
        Self {
            start: AtomicI64::new(0),
            passed: AtomicU32::new(0),
            missed: AtomicU32::new(0),
        }
    }

    /// Whether this line may be printed. `Some(n)`: yes, and `n` lines were dropped in the window
    /// before this one (reported once, as the kernel does). `None`: the window is full.
    pub(crate) fn pass(&self) -> Option<u32> {
        let now = ktime_now().max(1);
        let start = self.start.load(Ordering::Relaxed);
        let mut missed = 0;
        if (start == 0 || now - start >= RATE_INTERVAL_NS)
            && self
                .start
                .compare_exchange(start, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.passed.store(0, Ordering::Relaxed);
            missed = self.missed.swap(0, Ordering::Relaxed);
        }
        if self.passed.load(Ordering::Relaxed) < RATE_BURST {
            self.passed.fetch_add(1, Ordering::Relaxed);
            Some(missed)
        } else {
            self.missed.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// `dev_warn_ratelimited`: `dev_warn!`, at most ten lines in five seconds for each call site.
macro_rules! dev_warn_ratelimited {
    ($dev:expr, $($arg:tt)*) => {{
        static LIMIT: $crate::stream::RateLimit = $crate::stream::RateLimit::new();
        if let Some(missed) = LIMIT.pass() {
            if missed != 0 {
                dev_warn!($dev, "{} warnings suppressed\n", missed);
            }
            dev_warn!($dev, $($arg)*);
        }
    }};
}
pub(crate) use dev_warn_ratelimited;
