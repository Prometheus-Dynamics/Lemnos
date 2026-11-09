//! What a piece of code costs: hardware counters per thread, and allocations.
//! Both are compiled out unless their feature is on.

#[cfg(feature = "alloc-count")]
pub mod alloc_stats;
mod perf;
#[cfg(feature = "alloc-count")]
pub mod test_alloc;

pub use perf::{Event, PerfGroup};
