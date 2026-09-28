use crate::watch::{LinuxHotplugWatcher, ReadBatchError};
use lemnos_discovery::{DiscoveryError, DiscoveryResult, InventoryWatchEvent, InventoryWatcher};
use tokio::io::unix::AsyncFd;

/// Reactor-driven wrapper around [`LinuxHotplugWatcher`].
///
/// Unlike polling [`InventoryWatcher::poll`] on a timer, [`next_events`]
/// parks on the inotify descriptor through Tokio's reactor and wakes only when
/// the kernel reports sysfs/devfs changes. It never blocks an executor thread.
///
/// The wrapper only produces watch events. Running the resulting refresh is
/// still blocking discovery work, so callers should hand it to
/// `tokio::task::spawn_blocking` or an `AsyncRuntime` refresh.
///
/// [`next_events`]: AsyncLinuxHotplugWatcher::next_events
#[derive(Debug)]
pub struct AsyncLinuxHotplugWatcher {
    inner: AsyncFd<LinuxHotplugWatcher>,
}

impl AsyncLinuxHotplugWatcher {
    /// Registers the watcher with the current Tokio reactor.
    ///
    /// Must be called from within a Tokio runtime with IO enabled.
    pub fn new(watcher: LinuxHotplugWatcher) -> DiscoveryResult<Self> {
        let inner = AsyncFd::new(watcher).map_err(|error| DiscoveryError::WatchFailed {
            watcher: "linux.hotplug".to_string(),
            message: error.to_string(),
        })?;
        Ok(Self { inner })
    }

    /// Waits until the watcher observes at least one relevant change and
    /// returns the coalesced watch events.
    ///
    /// Cancel-safe: dropping the future before it resolves loses no events;
    /// they are reported by the next call.
    pub async fn next_events(&mut self) -> DiscoveryResult<Vec<InventoryWatchEvent>> {
        loop {
            let mut guard =
                self.inner
                    .readable_mut()
                    .await
                    .map_err(|error| DiscoveryError::WatchFailed {
                        watcher: "linux.hotplug".to_string(),
                        message: error.to_string(),
                    })?;

            match guard.get_inner_mut().read_batch() {
                Ok(events) if !events.is_empty() => return Ok(events),
                // Events were read but none mapped to a registration; the
                // queue may still hold more, so read again before waiting.
                Ok(_) => continue,
                Err(ReadBatchError::WouldBlock) => guard.clear_ready(),
                Err(ReadBatchError::Failed(error)) => return Err(error),
            }
        }
    }

    pub fn get_ref(&self) -> &LinuxHotplugWatcher {
        self.inner.get_ref()
    }

    pub fn get_mut(&mut self) -> &mut LinuxHotplugWatcher {
        self.inner.get_mut()
    }

    /// Deregisters from the reactor and returns the synchronous watcher.
    pub fn into_inner(self) -> LinuxHotplugWatcher {
        self.inner.into_inner()
    }

    pub fn name(&self) -> &'static str {
        self.inner.get_ref().name()
    }
}
