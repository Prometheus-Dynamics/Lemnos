//! A counting global allocator for a shipped service: `lemnosd` built with its `alloc-stats`
//! feature installs [`Counter`] and answers `lemnos-ctl stats` with [`snapshot`], so "no
//! allocations in steady state" can be checked on a running board.
//!
//! Every call forwards to [`System`] and adds two relaxed atomic increments: no lock, no
//! thread-local, no allocation of its own.
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: lemnos_measure::alloc_stats::Counter = lemnos_measure::alloc_stats::Counter;
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// Counts every allocation and free of the process, forwarding to [`System`].
pub struct Counter;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static FREED_BYTES: AtomicU64 = AtomicU64::new(0);

/// The counts since the process started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// Allocations (a reallocation counts as one, and as one free).
    pub allocs: u64,
    /// Frees.
    pub frees: u64,
    /// Bytes allocated in all.
    pub bytes: u64,
    /// Bytes allocated and not yet freed.
    pub live_bytes: u64,
}

/// The counts so far, or `None` when [`Counter`] is not the global allocator (nothing was
/// counted: a Rust process allocates before `main`).
pub fn snapshot() -> Option<Snapshot> {
    let allocs = ALLOCS.load(Ordering::Relaxed);
    if allocs == 0 {
        return None;
    }
    let bytes = BYTES.load(Ordering::Relaxed);
    Some(Snapshot {
        allocs,
        frees: FREES.load(Ordering::Relaxed),
        bytes,
        live_bytes: bytes.saturating_sub(FREED_BYTES.load(Ordering::Relaxed)),
    })
}

fn allocated(size: usize) {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(size as u64, Ordering::Relaxed);
}

fn freed(size: usize) {
    FREES.fetch_add(1, Ordering::Relaxed);
    FREED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
}

// SAFETY: every call forwards to `System` unchanged; the counts only read the sizes.
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, passed on.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, passed on.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        freed(layout.size());
        // SAFETY: the caller's pointer and layout, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's arguments, passed on.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            freed(layout.size());
            allocated(new_size);
        }
        p
    }
}
