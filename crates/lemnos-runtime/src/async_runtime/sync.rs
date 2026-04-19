use super::*;
use std::sync::{MutexGuard, RwLockReadGuard, RwLockWriteGuard};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn subscription_has_pending(
    runtime: &Arc<RwLock<Runtime>>,
    subscription: &Arc<Mutex<RuntimeEventSubscription>>,
) -> bool {
    with_subscription_runtime_read(runtime, subscription, |subscription, runtime| {
        runtime.pending_event_count(&subscription.cursor()) > 0
    })
}

pub(crate) fn with_subscription_runtime_read<T>(
    runtime: &Arc<RwLock<Runtime>>,
    subscription: &Arc<Mutex<RuntimeEventSubscription>>,
    operation: impl FnOnce(&mut RuntimeEventSubscription, &Runtime) -> T,
) -> T {
    let mut subscription = lock(subscription);
    let runtime = read_lock(runtime);
    operation(&mut subscription, &runtime)
}
