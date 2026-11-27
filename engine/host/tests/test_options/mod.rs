// The host options for a test here whose subject is not a hang or a deadline: a copy of
// `lemnos-test-options` (testing/crates/test-options), which lemnos-host's own tests cannot depend
// on without a circle. That crate's tests check the two deadlines are equal.

use std::time::Duration;

use lemnos_host::Options;

/// 30 s, far above what a loaded gate stretches a healthy call to. A test that is not about a hang
/// and hangs still fails, at this deadline.
/// Deadline sized for a loaded gate, pending the user's call on CALL-DEADLINE-RUNNABLE (whether the
/// host counts a call's wait for a CPU toward its deadline); if the host stops counting it, tests go
/// back to the default.
#[allow(dead_code)]
pub const CALL_DEADLINE: Duration = Duration::from_secs(30);

/// The host's default options with the call deadline [`CALL_DEADLINE`].
#[allow(dead_code)]
#[must_use]
pub fn options() -> Options {
    Options {
        call_deadline: CALL_DEADLINE,
        ..Options::default()
    }
}
