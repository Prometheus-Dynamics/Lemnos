// SPDX-License-Identifier: GPL-2.0
//! The event stream's queue, and temperature watches, Rust (stage 1h of the port; replaces
//! lemnos_events.c). The interface is lemnos.h's. Interrupts bypass the queue, on the core's ready list
//! (rs/irq.rs), so they can't be lost to it.
//!
//! The queue is a fixed ring of LEM_EVENTS under one interrupt-safe spinlock: posting allocates nothing
//! and may come from any context. When it is full the newest is lost, and the sequence gap says so. The
//! wait queue readers sleep on is the core's (rs/dev.rs, `WAIT`); a post wakes it.
//!
//! A watch is one of LEMNOS_MAX_THERMAL_WATCHES static slots, each with its own delayed work, which
//! reads the zone every period and posts LEMNOS_EVENT_TEMPERATURE when it crosses. The kernel's thermal
//! functions come from the module's generated bindings (rs/bindings.rs). INIT_DELAYED_WORK and
//! schedule_delayed_work are composed from their exported parts, as the kernel crate's DelayedWork does.

use crate::bindings as lb;
use core::{cell::UnsafeCell, ptr};
use kernel::{
    bindings,
    error::{code::*, from_err_ptr, Error},
    ffi::{c_int, c_long, c_uint, c_ulong},
    prelude::*,
    static_lock_class,
};

/// The queue's length.
const LEM_EVENTS: usize = 64;
/// The shortest period a watch may have.
const LEM_WATCH_MIN_MS: u32 = 10;
/// LEMNOS_MAX_THERMAL_WATCHES of lemnos.h.
const WATCHES: usize = lb::LEMNOS_MAX_THERMAL_WATCHES as usize;

const _: () = assert!(core::mem::size_of::<lb::lemnos_event>() == 32);

/// The queue: a ring of `count` events from `head`, and the sequence of the last one posted.
struct Queue {
    events: [lb::lemnos_event; LEM_EVENTS],
    head: usize,
    count: usize,
    seq: u32,
}

const EMPTY: lb::lemnos_event = lb::lemnos_event { block: 0, index: 0, count: 0, kind: 0, ns: 0, value: 0 };

kernel::sync::global_lock! {
    // SAFETY: initialised by lem_events_init, at module load, before anything can post.
    unsafe(uninit) static QUEUE: SpinLockIrq<Queue> = Queue { events: [EMPTY; LEM_EVENTS], head: 0, count: 0, seq: 0 };
}

/// The event stream's post: queues one event and wakes the readers. Full: the newest is lost, and the
/// sequence gap says so. Any context; allocates nothing.
pub(crate) fn post(kind: u32, block: u32, index: u32, value: i64) {
    // SAFETY: reads the monotonic clock (ktime_get_ns is ktime_get, in ns).
    let ns = unsafe { bindings::ktime_get() } as u64;
    {
        let mut q = QUEUE.lock();
        q.seq = q.seq.wrapping_add(1);
        if q.count < LEM_EVENTS {
            let at = (q.head + q.count) % LEM_EVENTS;
            let count = q.seq;
            q.events[at] = lb::lemnos_event { block, index, count, kind, ns, value };
            q.count += 1;
        }
    }
    // SAFETY: wake_up_interruptible(&lem_wait): the core's wait queue (rs/dev.rs), a static made at load.
    unsafe { bindings::__wake_up(crate::dev::WAIT.get(), bindings::TASK_INTERRUPTIBLE, 1, ptr::null_mut()) };
}

/// `lem_event_post` of lemnos_internal.h, for the C still calling it.
#[no_mangle]
pub(crate) extern "C" fn lem_event_post(kind: u32, block: u32, index: u32, value: i64) {
    post(kind, block, index, value);
}

/// Whether an event is queued.
#[no_mangle]
pub(crate) extern "C" fn lem_event_pending() -> bool {
    QUEUE.lock().count != 0
}

/// Takes the oldest event into `e`, if there is one.
///
/// # Safety
/// `e` points at a writable `struct lemnos_event`.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_event_take(e: *mut lb::lemnos_event) -> bool {
    let mut q = QUEUE.lock();
    if q.count == 0 {
        return false;
    }
    let head = q.head;
    // SAFETY: the caller's contract.
    unsafe { e.write(q.events[head]) };
    q.head = (head + 1) % LEM_EVENTS;
    q.count -= 1;
    true
}

// --- temperature ---

/// One watch slot. `work` is the slot's delayed work; the rest is written only while the work is
/// neither queued nor running (under WATCHES_USED's lock, before it is scheduled, and after
/// cancel_delayed_work_sync), and read by the work. `above` is the work's alone.
#[repr(C)]
struct Watch {
    work: bindings::delayed_work,
    zone: *mut lb::thermal_zone_device,
    index: u32,
    low: c_int,
    high: c_int,
    period: c_ulong,
    above: bool,
}

#[repr(transparent)]
struct WatchCell(UnsafeCell<Watch>);

// SAFETY: see Watch: each field has one writer at a time, ordered by the workqueue's own queueing and
// cancel_delayed_work_sync.
unsafe impl Sync for WatchCell {}

const fn empty_watch() -> WatchCell {
    WatchCell(UnsafeCell::new(Watch {
        // SAFETY: a struct delayed_work of zeroes, initialised by INIT_DELAYED_WORK before each use.
        work: unsafe { core::mem::zeroed() },
        zone: ptr::null_mut(),
        index: 0,
        low: 0,
        high: 0,
        period: 0,
        above: false,
    }))
}

static WATCH_SLOTS: [WatchCell; WATCHES] = [const { empty_watch() }; WATCHES];

kernel::sync::global_lock! {
    // SAFETY: initialised by lem_events_init, at module load, before any watch can be made.
    unsafe(uninit) static WATCHES_USED: Mutex<[bool; WATCHES]> = [false; WATCHES];
}

/// A zone by its type: the name must be NUL-terminated within its field and not empty.
fn lem_zone(name: &[u8]) -> Result<*mut lb::thermal_zone_device> {
    match name.iter().position(|&b| b == 0) {
        None | Some(0) => Err(EINVAL),
        // SAFETY: `name` is NUL-terminated (found above); the call only reads it.
        Some(_) => from_err_ptr(unsafe { lb::thermal_zone_get_zone_by_name(name.as_ptr().cast()) }),
    }
}

fn thermal_read(t: &mut lb::lemnos_thermal) -> Result {
    // SAFETY: the zone field is `c_char`s; read as bytes.
    let zone = lem_zone(unsafe { core::slice::from_raw_parts(t.zone.as_ptr().cast(), t.zone.len()) })?;
    let mut temp: c_int = 0;
    // SAFETY: `zone` is a registered zone (zones are not freed while the module can name them by type).
    let r = unsafe { lb::thermal_zone_get_temp(zone, &raw mut temp) };
    if r != 0 {
        return Err(Error::from_errno(r));
    }
    t.temperature = i64::from(temp);
    Ok(())
}

/// `LEMNOS_IOC_THERMAL_READ`.
///
/// # Safety
/// `t` is the ioctl's argument, from the copy.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_thermal_read(t: *mut lb::lemnos_thermal) -> c_long {
    // SAFETY: the caller's argument.
    match thermal_read(unsafe { &mut *t }) {
        Ok(()) => 0,
        Err(e) => e.to_errno() as c_long,
    }
}

/// A watch's period: reads the zone and posts when it crosses; a zone that fails a read is tried again
/// next period.
///
/// # Safety
/// Called by the workqueue with the work of a watch slot.
unsafe extern "C" fn lem_watch_tick(work: *mut bindings::work_struct) {
    // The work is the first field of the delayed work, itself the first field of the slot (repr(C)).
    let w = work.cast::<Watch>();
    let mut temp: c_int = 0;
    // SAFETY: the slot's fields other than `above` are not written while its work runs (see Watch).
    let zone = unsafe { (*w).zone };
    // SAFETY: the watch's zone, found when the watch was made.
    if unsafe { lb::thermal_zone_get_temp(zone, &raw mut temp) } == 0 {
        // SAFETY: as above; `above` is written by this work alone.
        let (above, low, high, index) = unsafe { ((*w).above, (*w).low, (*w).high, (*w).index) };
        let cross = if above { temp <= low } else { temp >= high };
        if cross {
            // SAFETY: as above.
            unsafe { (*w).above = !above };
            post(lb::LEMNOS_EVENT_TEMPERATURE, 0, index, i64::from(temp));
        }
    }
    // SAFETY: as above.
    let period = unsafe { (*w).period };
    // SAFETY: re-queues this slot's own delayed work.
    unsafe { schedule(&raw mut (*w).work, period) };
}

/// schedule_delayed_work: queue_delayed_work_on(WORK_CPU_UNBOUND, system_percpu_wq, ...).
///
/// # Safety
/// `work` is an initialised delayed work that lives for ever.
unsafe fn schedule(work: *mut bindings::delayed_work, delay: c_ulong) {
    // SAFETY: the system queue is a C global that is always there.
    let wq = unsafe { bindings::system_percpu_wq };
    // SAFETY: the caller's contract.
    unsafe {
        bindings::queue_delayed_work_on(bindings::wq_misc_consts_WORK_CPU_UNBOUND as c_int, wq, work, delay)
    };
}

fn thermal_watch(r: &mut lb::lemnos_thermal_watch) -> Result {
    // SAFETY: the zone field is `c_char`s; read as bytes.
    let zone = lem_zone(unsafe { core::slice::from_raw_parts(r.zone.as_ptr().cast(), r.zone.len()) })?;
    if r.low_mc >= r.high_mc || r.period_ms < LEM_WATCH_MIN_MS {
        return Err(EINVAL);
    }
    let mut used = WATCHES_USED.lock();
    let Some(i) = used.iter().position(|&u| !u) else {
        return Err(ENOSPC);
    };
    let w = WATCH_SLOTS[i].0.get();
    // SAFETY: the slot is unused, so its work is neither queued nor running (lem_events_reset
    // cancelled it); the lock is held, so nothing else writes it.
    unsafe {
        (*w).zone = zone;
    }
    // SAFETY: as above.
    unsafe { (*w).index = i as u32 };
    // SAFETY: as above.
    unsafe { (*w).low = r.low_mc };
    // SAFETY: as above.
    unsafe { (*w).high = r.high_mc };
    // SAFETY: msecs_to_jiffies for a value that is not a constant is this exported function.
    let period = unsafe { bindings::__msecs_to_jiffies(r.period_ms as c_uint) };
    // SAFETY: as above.
    unsafe { (*w).period = period };
    // SAFETY: as above.
    unsafe { (*w).above = false };
    // INIT_DELAYED_WORK: the work, then its timer, as the kernel crate's DelayedWork::new.
    // SAFETY: the slot's work is idle (as above) and lives for ever; the name and key are static.
    unsafe {
        bindings::init_work_with_key(
            &raw mut (*w).work.work,
            Some(lem_watch_tick),
            false,
            c"lem_watch".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: as above; the timer is the delayed work's, so delayed_work_timer_fn is its function.
    unsafe {
        bindings::timer_init_key(
            &raw mut (*w).work.timer,
            Some(bindings::delayed_work_timer_fn),
            bindings::TIMER_IRQSAFE,
            c"lem_watch.timer".as_ptr().cast(),
            static_lock_class!().as_ptr(),
        )
    };
    // SAFETY: the slot's work, initialised above.
    unsafe { schedule(&raw mut (*w).work, 0) };
    used[i] = true;
    r.watch = i as u32;
    Ok(())
}

/// `LEMNOS_IOC_THERMAL_WATCH`.
///
/// # Safety
/// `r` is the ioctl's argument, from the copy.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_thermal_watch(r: *mut lb::lemnos_thermal_watch) -> c_long {
    // SAFETY: the caller's argument.
    match thermal_watch(unsafe { &mut *r }) {
        Ok(()) => 0,
        Err(e) => e.to_errno() as c_long,
    }
}

/// Ends the watches and empties the queue: the file closed.
#[no_mangle]
pub(crate) extern "C" fn lem_events_reset() {
    {
        let mut used = WATCHES_USED.lock();
        for (i, u) in used.iter_mut().enumerate() {
            if !*u {
                continue;
            }
            // SAFETY: the slot's work was initialised when it was made; this waits for it to stop.
            unsafe { bindings::cancel_delayed_work_sync(&raw mut (*WATCH_SLOTS[i].0.get()).work) };
            *u = false;
        }
    }
    let mut q = QUEUE.lock();
    q.head = 0;
    q.count = 0;
    q.seq = 0;
}

/// The two locks, once, at module load.
pub(crate) fn lem_events_init() {
    // SAFETY: called once, at module load, before anything can post or make a watch.
    unsafe { QUEUE.init() };
    // SAFETY: as above.
    unsafe { WATCHES_USED.init() };
}
