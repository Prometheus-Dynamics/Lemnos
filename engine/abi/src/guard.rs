use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::Status;

/// Runs an `extern "C"` entry point's body: a panic comes back as
/// [`Status::PANIC`] instead of unwinding into the caller, which is undefined
/// behaviour across a C frame.
///
/// ```
/// use lemnos_abi::{Status, guard};
///
/// extern "C" fn reset(count: u32) -> Status {
///     guard(|| if count > 4 { Status::INVALID_ARGUMENT } else { Status::OK })
/// }
/// assert_eq!(reset(9), Status::INVALID_ARGUMENT);
/// ```
///
/// With `panic = "abort"` there is nothing to catch: the process aborts, as
/// it would without the guard.
pub fn guard(body: impl FnOnce() -> Status) -> Status {
    guard_or(Status::PANIC, body)
}

/// [`guard`] for an entry point that returns something other than a
/// [`Status`] (a handle, a count): `on_panic` is returned on a panic.
pub fn guard_or<R>(on_panic: R, body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(on_panic)
}
