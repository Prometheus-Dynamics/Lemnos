// Every allocation of this test binary is counted per thread, so a test can
// check that a hot path allocates nothing (`lemnos_linux_sys::test_alloc`).
#[global_allocator]
static ALLOCATOR: lemnos_linux_sys::test_alloc::Counting = lemnos_linux_sys::test_alloc::Counting;

mod backend;
mod device_watch;
mod discovery;
#[cfg(feature = "hotplug")]
mod hotplug;
mod support;

/// Notes that a hardware-dependent test was skipped (on stderr, for the test log).
#[cfg(any(feature = "dma", feature = "v4l2-m2m"))]
#[allow(clippy::print_stderr)]
pub(crate) fn skip(reason: &str) {
    eprintln!("skipped: {reason}");
}

/// Whether the tests may scan this machine's device nodes, which opens and
/// queries whatever is there: `LEMNOS_TEST_DEVICE_NODES=1`. Off by default, so
/// a test run never touches a device it was not pointed at.
#[cfg(feature = "v4l2-m2m")]
pub(crate) fn device_nodes_allowed() -> bool {
    std::env::var_os("LEMNOS_TEST_DEVICE_NODES").is_some_and(|value| value == "1")
}
