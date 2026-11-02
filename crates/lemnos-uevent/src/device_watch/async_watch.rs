//! Reactor-driven [`DeviceWatcher`]: parks on the watcher's descriptor
//! through Tokio's reactor instead of polling.

use super::{DeviceWatchSource, DeviceWatcher};
use crate::uevent::UeventRef;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use tokio::io::unix::AsyncFd;

/// A [`DeviceWatcher`] registered with the current Tokio reactor.
///
/// [`next_batch`](AsyncDeviceWatcher::next_batch) waits until something is
/// readable and hands the changes to a callback. Cancel-safe: a dropped
/// future loses nothing, since the kernel queue keeps the events.
#[derive(Debug)]
pub struct AsyncDeviceWatcher {
    inner: AsyncFd<FdHolder>,
}

/// Gives the watcher a raw descriptor for [`AsyncFd`]. Only constructed with
/// a descriptor-backed watcher (checked in [`AsyncDeviceWatcher::new`]).
#[derive(Debug)]
struct FdHolder(DeviceWatcher);

impl AsRawFd for FdHolder {
    fn as_raw_fd(&self) -> RawFd {
        self.0.fd().map_or(-1, |fd| fd.as_raw_fd())
    }
}

impl AsyncDeviceWatcher {
    /// Registers `watcher` with the current reactor. Fails for a rescan
    /// watcher (no descriptor to wait on) and outside a Tokio runtime with IO
    /// enabled.
    pub fn new(watcher: DeviceWatcher) -> io::Result<Self> {
        if watcher.source() == DeviceWatchSource::Rescan {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "rescan watchers have no descriptor to wait on",
            ));
        }
        Ok(Self {
            inner: AsyncFd::new(FdHolder(watcher))?,
        })
    }

    /// Waits until the watcher has changes, then calls `f` for each (the same
    /// contract as [`DeviceWatcher::read`]) and returns how many records were
    /// consumed. Filtered-out records count too, so the call only returns
    /// after the kernel queue has records (it never returns 0 on its own).
    pub async fn next_batch(&mut self, mut f: impl FnMut(UeventRef<'_>)) -> io::Result<usize> {
        loop {
            let mut guard = self.inner.readable_mut().await?;
            match guard.get_inner_mut().0.read(&mut f) {
                Ok(0) => guard.clear_ready(),
                Ok(consumed) => return Ok(consumed),
                Err(error) => return Err(error),
            }
        }
    }

    /// The watcher, for coldplug or inspection.
    pub fn get_mut(&mut self) -> &mut DeviceWatcher {
        &mut self.inner.get_mut().0
    }
}
