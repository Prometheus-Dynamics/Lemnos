//! Counts allocations in tests. This crate's test binary runs on [`Counting`]; with the
//! `alloc-count` feature another crate's test binary can install it too (the `unsafe` stays
//! here):
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: lemnos_linux_sys::test_alloc::Counting = lemnos_linux_sys::test_alloc::Counting;
//! ```
//!
//! Test support only: never install it in a shipped binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Counts this thread's allocations, so parallel tests do not disturb a count. Forwards every
/// call to [`System`].
pub struct Counting;

thread_local! {
    /// This thread's allocations so far.
    pub static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    /// Set while this thread records a backtrace, so the recording's own allocations are not
    /// recorded.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Every thread's allocations so far, for code that allocates on threads a test does not own
/// (a service's loop and workers): the difference over a stretch, less the test thread's own.
pub static TOTAL: AtomicUsize = AtomicUsize::new(0);

static RECORD: AtomicBool = AtomicBool::new(false);
static RECORDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn count() {
    let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
    TOTAL.fetch_add(1, Ordering::Relaxed);
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
        // SAFETY: the caller's layout, passed on.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's pointer and layout, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: the caller's arguments, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The value of `f`, and how many allocations this thread made while it ran (zero counts
/// unless [`Counting`] is the global allocator).
pub fn allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATIONS.with(Cell::get);
    let value = f();
    (value, ALLOCATIONS.with(Cell::get) - before)
}
