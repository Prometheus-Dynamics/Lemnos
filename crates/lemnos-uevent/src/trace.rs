//! Watcher diagnostics: `tracing` events with the `tracing` feature, nothing
//! otherwise (the arguments are then not evaluated).

#[cfg(feature = "tracing")]
macro_rules! watch_debug {
    ($($arg:tt)*) => {
        { tracing::debug!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! watch_debug {
    ($($arg:tt)*) => {};
}

#[cfg(feature = "tracing")]
macro_rules! watch_info {
    ($($arg:tt)*) => {
        { tracing::info!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! watch_info {
    ($($arg:tt)*) => {};
}

#[cfg(feature = "tracing")]
macro_rules! watch_warn {
    ($($arg:tt)*) => {
        { tracing::warn!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! watch_warn {
    ($($arg:tt)*) => {};
}
