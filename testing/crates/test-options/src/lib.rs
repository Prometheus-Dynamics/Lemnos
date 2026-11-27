//! The host options for a test whose subject is not a hang or a deadline: the defaults, with a call
//! deadline of [`CALL_DEADLINE`].
//!
//! The host's call deadline is wall time. On a loaded machine a call that waits its turn on a CPU
//! for longer than the default 2 s is judged hung though its driver never stopped (a gate at load
//! 86 to 100 failed a 10 ms call that way). Whether the host should count that wait at all is a
//! product question held for the user (CALL-DEADLINE-RUNNABLE). Until it is decided, the tests
//! that are not about a hang take this deadline, and the tests that are keep their own.
//!
//! engine/host's own tests keep a copy of the same value in `engine/host/tests/test_options`
//! (a dependency of lemnos-host's tests on this crate would go round in a circle); this crate's
//! tests check the two are equal.

use std::time::Duration;

use lemnos_host::Options;

/// 30 s, far above what a loaded gate stretches a healthy call to. A test that is not about a hang
/// and hangs still fails, at this deadline.
/// Deadline sized for a loaded gate, pending the user's call on CALL-DEADLINE-RUNNABLE (whether the
/// host counts a call's wait for a CPU toward its deadline); if the host stops counting it, tests go
/// back to the default.
pub const CALL_DEADLINE: Duration = Duration::from_secs(30);

/// The host's default options with the call deadline [`CALL_DEADLINE`].
#[must_use]
pub fn options() -> Options {
    Options {
        call_deadline: CALL_DEADLINE,
        ..Options::default()
    }
}
