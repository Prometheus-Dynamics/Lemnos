//! Counts allocations in tests. This crate's test binary runs on [`Counting`]; with the
//! `alloc-count` feature another crate's test binary can install it too (the `unsafe` stays
//! here):
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
//! ```
//!
//! Test support only: never install it in a shipped binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

thread_local! {
    /// This thread's live bytes, as its allocations and frees have moved them, and the most they
    /// have been since [`peak_bytes`] last began.
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

/// Counts this thread's allocations, so parallel tests do not disturb a count. Forwards every
/// call to [`System`].
pub struct Counting;

thread_local! {
    /// This thread's allocations so far.
    pub static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    /// Set while this thread records a backtrace, so the recording's own allocations are not
    /// recorded.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    /// Cleared by [`leave_out_of_total`]: this thread's allocations are not in [`TOTAL`].
    static IN_TOTAL: Cell<bool> = const { Cell::new(true) };
}

/// Every thread's allocations so far, for code that allocates on threads a test does not own
/// (a service's loop and workers): the difference over a stretch, less the test thread's own.
pub static TOTAL: AtomicUsize = AtomicUsize::new(0);

static RECORD: AtomicBool = AtomicBool::new(false);
static RECORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Moves this thread's live bytes by `delta` and keeps the peak.
fn live(delta: isize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

fn count() {
    let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    if IN_TOTAL.try_with(Cell::get).unwrap_or(true) {
        TOTAL.fetch_add(1, Ordering::Relaxed);
    }
    if RECORD.load(Ordering::Relaxed) {
        record();
    }
}

/// Keeps the backtrace of an allocation while recording is on (see [`record`]).
fn record() {
    if RECORDING.try_with(|r| r.replace(true)).unwrap_or(true) {
        return;
    }
    let thread = std::thread::current();
    let trace = format!(
        "thread {}\n{}",
        thread.name().unwrap_or("?"),
        std::backtrace::Backtrace::force_capture()
    );
    if let Ok(mut records) = RECORDS.lock() {
        records.push(trace);
    }
    let _ = RECORDING.try_with(|r| r.set(false));
}

/// Leaves this thread's allocations out of [`TOTAL`] from now on: for a test's own client
/// threads, so a count over [`TOTAL`] is the code under test's alone. Exact, unlike sampling the
/// thread's [`ALLOCATIONS`] from inside its loop, which a starved thread publishes late.
pub fn leave_out_of_total() {
    let _ = IN_TOTAL.try_with(|t| t.set(false));
}

/// Turns backtrace recording of every allocation on or off (all threads), for finding where a
/// counted allocation comes from. Slow: for diagnosis only.
pub fn set_recording(on: bool) {
    RECORD.store(on, Ordering::Relaxed);
}

/// The backtraces recorded so far, each led by its thread's name, and clears them.
pub fn take_records() -> Vec<String> {
    RECORDS
        .lock()
        .map(|mut r| std::mem::take(&mut *r))
        .unwrap_or_default()
}

// SAFETY: every call forwards to `System` unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        live(layout.size() as isize);
        // SAFETY: the caller's layout, passed on.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        live(-(layout.size() as isize));
        // SAFETY: the caller's pointer and layout, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        live(new_size as isize - layout.size() as isize);
        // SAFETY: the caller's arguments, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// The value of `f`, and how many allocations this thread made while it ran (zero counts
/// unless [`Counting`] is the global allocator).
pub fn allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATIONS.with(Cell::get);
    let value = f();
    (value, ALLOCATIONS.with(Cell::get) - before)
}

/// The value of `f`, and the most bytes this thread held beyond what it held when `f` began, at
/// any point while `f` ran (zero unless [`Counting`] is the global allocator). A buffer `f`
/// allocates and frees is part of the peak only while it is alive.
pub fn peak_bytes<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(base));
    let value = f();
    let peak = PEAK.with(Cell::get);
    (value, (peak - base).max(0) as usize)
}
