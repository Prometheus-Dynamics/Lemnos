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
