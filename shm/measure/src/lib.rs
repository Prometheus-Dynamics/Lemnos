//! Test support for the shm crates. Not published.
//!
//! - A counting allocator and [`harness`], the gate for "this path allocates nothing".
//! - [`sched`]: a proof that a path makes no system call, and thread pinning and CPU time for the
//!   bench.

#[allow(unsafe_code)]
mod alloc_counter;
pub mod harness;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub mod sched;

pub use alloc_counter::{AllocCount, CountingAlloc, thread_counts};
