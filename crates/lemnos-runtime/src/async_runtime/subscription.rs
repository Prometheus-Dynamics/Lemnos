use super::*;
use crate::async_runtime::sync::{lock, subscription_has_pending, with_subscription_runtime_read};

impl AsyncRuntimeEventSubscription {
    pub fn cursor(&self) -> RuntimeEventCursor {
        lock(&self.subscription).cursor()
    }

    pub async fn cursor_async(&self) -> AsyncRuntimeResult<RuntimeEventCursor> {
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || lock(&subscription).cursor()).await?)
    }

    pub fn next_index(&self) -> usize {
        lock(&self.subscription).next_index()
    }

    pub async fn next_index_async(&self) -> AsyncRuntimeResult<usize> {
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || lock(&subscription).next_index()).await?)
    }

    pub fn is_stale(&self) -> bool {
        with_subscription_runtime_read(
            &self.runtime,
            &self.subscription,
            |subscription, runtime| subscription.is_stale(runtime),
        )
    }

    pub async fn is_stale_async(&self) -> AsyncRuntimeResult<bool> {
        let runtime = Arc::clone(&self.runtime);
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || {
            with_subscription_runtime_read(&runtime, &subscription, |subscription, runtime| {
                subscription.is_stale(runtime)
            })
        })
        .await?)
    }

    pub fn has_pending(&self) -> bool {
        with_subscription_runtime_read(
            &self.runtime,
            &self.subscription,
            |subscription, runtime| subscription.has_pending(runtime),
        )
    }

    pub async fn has_pending_async(&self) -> AsyncRuntimeResult<bool> {
        let runtime = Arc::clone(&self.runtime);
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || {
            with_subscription_runtime_read(&runtime, &subscription, |subscription, runtime| {
                subscription.has_pending(runtime)
            })
        })
        .await?)
    }

    pub fn pending_count(&self) -> usize {
        with_subscription_runtime_read(
            &self.runtime,
            &self.subscription,
            |subscription, runtime| subscription.pending_count(runtime),
        )
    }

    pub async fn pending_count_async(&self) -> AsyncRuntimeResult<usize> {
        let runtime = Arc::clone(&self.runtime);
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || {
            with_subscription_runtime_read(&runtime, &subscription, |subscription, runtime| {
                subscription.pending_count(runtime)
            })
        })
        .await?)
    }

    pub async fn poll(&self) -> AsyncRuntimeResult<Vec<LemnosEvent>> {
        let runtime = Arc::clone(&self.runtime);
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || {
            with_subscription_runtime_read(&runtime, &subscription, |subscription, runtime| {
                subscription.poll(runtime).to_vec()
            })
        })
        .await?)
    }

    pub async fn wait_for_update(&self, timeout: Option<Duration>) -> AsyncRuntimeResult<bool> {
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || {
            let subscription = lock(&subscription);
            subscription.wait_for_update(timeout)
        })
        .await?)
    }

    pub async fn wait_and_poll_next(
        &self,
        timeout: Option<Duration>,
    ) -> AsyncRuntimeResult<Option<Vec<LemnosEvent>>> {
        let runtime = Arc::clone(&self.runtime);
        let subscription = Arc::clone(&self.subscription);
        Ok(tokio::task::spawn_blocking(move || {
            if subscription_has_pending(&runtime, &subscription) {
                return Some(with_subscription_runtime_read(
                    &runtime,
                    &subscription,
                    |subscription, runtime| subscription.poll(runtime).to_vec(),
                ));
            }

            let updated = {
                let subscription = lock(&subscription);
                subscription.wait_for_update(timeout)
            };
            if !updated {
                return None;
            }

            Some(with_subscription_runtime_read(
                &runtime,
                &subscription,
                |subscription, runtime| subscription.poll(runtime).to_vec(),
            ))
        })
        .await?)
    }
}

impl Clone for AsyncRuntimeEventSubscription {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
            subscription: Arc::clone(&self.subscription),
        }
    }
}
