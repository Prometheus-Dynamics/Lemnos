/// Exclusive use of a value from a shared, `'static` place, supplied by the firmware.
///
/// On one core this is usually a critical section around a `RefCell`; on a host, a mutex. A
/// device's calls run inside its lock, and a device can call another inside its own, so locks
/// nest: a lock that is already held must answer `None` rather than wait for itself.
pub trait Lock: Sync {
    type Inner;

    /// Runs `f` on the value, or returns `None` when the value is in use (a call into the same
    /// device that hasn't returned).
    fn with<R>(&self, f: impl FnOnce(&mut Self::Inner) -> R) -> Option<R>;
}
