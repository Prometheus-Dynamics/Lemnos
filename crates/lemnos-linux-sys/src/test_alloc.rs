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

/// Counts this thread's allocations, so parallel tests do not disturb a count. Forwards every
/// call to [`System`].
pub struct Counting;

thread_local! {
    /// This thread's allocations so far.
    pub static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
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
