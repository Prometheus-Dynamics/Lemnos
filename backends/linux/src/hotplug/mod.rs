//! Watching devices appear and go away: add, remove, change, bind and unbind for any set of kernel
//! subsystems.
//!
//! Three sources, chosen by [`DeviceWatcher::new`] in this order:
//!
//! - [`DeviceWatchSource::Netlink`]: kernel uevents on the real `/sys`. Every change, with its
//!   action and the device's variables. Needs a socket the sandbox allows.
//! - [`DeviceWatchSource::Inotify`]: inotify on the sysfs class and bus directories and `/dev`.
//!   Works on any root, test trees included. It sees entries appearing and going away, not driver
//!   bind or unbind, and reports `add`, `remove` and `change` only.
//! - [`DeviceWatchSource::Rescan`]: no descriptor. Every read diffs a fresh sysfs walk against the
//!   last one. For hosts where neither of the above is allowed.
//!
//! Events have the shape of the kernel's uevents. Coldplug and rescan events are synthesized in
//! that shape, so one consumer parses all of them.

mod inotify;
mod paths;
mod sysfs;
mod uevent;

#[cfg(test)]
mod tests;

pub use paths::LinuxPaths;
pub use uevent::{Action, Uevent, UeventMsg, UeventRecv, UeventRef, UeventSocket};

use lemnos_linux_sys::poll;
use std::fs;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Which devices a watcher reports: a set of subsystems (`SUBSYSTEM`), and
/// optionally device types (`DEVTYPE`, such as `usb_device`).
///
/// Build one in a `const` (`Filter::subsystems(&["video4linux", "media"])`).
/// Inotify events carry no `DEVTYPE`, so inotify sources filter by subsystem
/// only: check `DEVTYPE` in the consumer when it matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    subsystems: &'static [&'static str],
    devtypes: &'static [&'static str],
}

impl Filter {
    /// Every device of these subsystems.
    pub const fn subsystems(subsystems: &'static [&'static str]) -> Filter {
        Filter {
            subsystems,
            devtypes: &[],
        }
    }

    /// Only devices of these `DEVTYPE`s (empty: any type).
    pub const fn with_devtypes(self, devtypes: &'static [&'static str]) -> Filter {
        Filter { devtypes, ..self }
    }

    /// Whether `subsystem` is one of the filter's.
    pub fn includes(&self, subsystem: &str) -> bool {
        self.subsystems.contains(&subsystem)
    }

    /// Whether a message is for a device the filter reports.
    pub fn matches(&self, msg: &UeventMsg<'_>) -> bool {
        msg.subsystem().is_some_and(|s| self.includes(s))
            && (self.devtypes.is_empty()
                || msg.devtype().is_some_and(|t| self.devtypes.contains(&t)))
    }
}

/// Where a [`DeviceWatcher`] learns about changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceWatchSource {
    /// Kernel uevents over netlink.
    Netlink,
    /// inotify on the sysfs and `/dev` directories of the filter.
    Inotify,
    /// Diffs of a sysfs walk on each read (no descriptor).
    Rescan,
}

/// Watches devices of a filter's subsystems. Non-blocking: [`read`] reports
/// what changed since the last call. [`fd`] exposes the descriptor to wait on
/// (`poll`, `epoll`, a reactor); rescan watchers have none, so wait with
/// [`wait`] or sleep and read.
///
/// [`read`]: DeviceWatcher::read
/// [`fd`]: DeviceWatcher::fd
/// [`wait`]: DeviceWatcher::wait
#[derive(Debug)]
pub struct DeviceWatcher {
    paths: LinuxPaths,
    filter: Filter,
    source: Source,
    /// Reused for synthesized messages (inotify events, rescan diffs).
    scratch: Vec<u8>,
}

#[derive(Debug)]
enum Source {
    Netlink(UeventSocket),
    Inotify(inotify::Watches),
    Rescan(sysfs::Snapshot),
}

impl DeviceWatcher {
    /// Netlink on the real `/sys` (when the socket opens), else inotify, else
    /// rescans. Never fails: the rescan fallback needs nothing.
    pub fn new(paths: LinuxPaths, filter: Filter) -> DeviceWatcher {
        match Self::fd_backed(paths.clone(), filter) {
            Ok(watcher) => watcher,
            Err(_error) => Self::rescan(paths, filter),
        }
    }

    /// Netlink on the real `/sys` when it opens, else inotify. Fails when
    /// neither is available: the result always has a descriptor.
    pub fn fd_backed(paths: LinuxPaths, filter: Filter) -> io::Result<DeviceWatcher> {
        if paths.sys_class_root == Path::new("/sys/class") {
            match UeventSocket::open() {
                Ok(socket) => {
                    return Ok(Self::with_source(paths, filter, Source::Netlink(socket)));
                }
                Err(_error) => {}
            }
        }
        Self::inotify(paths, filter)
    }

    /// Netlink uevents, whatever the root; fails if the socket cannot open.
    pub fn netlink(paths: LinuxPaths, filter: Filter) -> io::Result<DeviceWatcher> {
        let socket = UeventSocket::open()?;
        Ok(Self::with_source(paths, filter, Source::Netlink(socket)))
    }

    /// inotify on the filter's directories under `paths`.
    pub fn inotify(paths: LinuxPaths, filter: Filter) -> io::Result<DeviceWatcher> {
        let watches = inotify::Watches::open(&paths, &filter)?;
        Ok(Self::with_source(paths, filter, Source::Inotify(watches)))
    }

    /// Diffs of sysfs walks. The current devices become the baseline, so the
    /// first [`read`](Self::read) reports only later changes.
    pub fn rescan(paths: LinuxPaths, filter: Filter) -> DeviceWatcher {
        let baseline = sysfs::snapshot(&paths, &filter);
        Self::with_source(paths, filter, Source::Rescan(baseline))
    }

    fn with_source(paths: LinuxPaths, filter: Filter, source: Source) -> DeviceWatcher {
        DeviceWatcher {
            paths,
            filter,
            source,
            scratch: Vec::new(),
        }
    }

    /// Where this watcher learns about changes.
    pub fn source(&self) -> DeviceWatchSource {
        match self.source {
            Source::Netlink(_) => DeviceWatchSource::Netlink,
            Source::Inotify(_) => DeviceWatchSource::Inotify,
            Source::Rescan(_) => DeviceWatchSource::Rescan,
        }
    }

    /// The filter this watcher reports.
    pub fn filter(&self) -> Filter {
        self.filter
    }

    /// The paths this watcher reads.
    pub fn paths(&self) -> &LinuxPaths {
        &self.paths
    }

    /// The descriptor that becomes readable when [`read`](Self::read) has
    /// something to report. `None` for rescan watchers.
    pub fn fd(&self) -> Option<BorrowedFd<'_>> {
        match &self.source {
            Source::Netlink(socket) => Some(socket.as_fd()),
            Source::Inotify(watches) => Some(watches.as_fd()),
            Source::Rescan(_) => None,
        }
    }

    /// Waits up to `timeout` (`None`: indefinitely) until a descriptor-backed
    /// watcher has something to read; returns whether it does. A rescan
    /// watcher sleeps for `timeout` and returns `true`, since a rescan is due.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        match self.fd() {
            Some(fd) => Ok(poll::poll_one(fd, poll::POLLIN, timeout)? & poll::POLLIN != 0),
            None => {
                if let Some(timeout) = timeout {
                    std::thread::sleep(timeout);
                }
                Ok(true)
            }
        }
    }

    /// Reports every device the filter matches that is present now, as `add`
    /// events in devpath order, and returns how many there were. Watch first,
    /// then coldplug: a device that arrives in between is reported twice, so
    /// consumers should key on devpath.
    pub fn coldplug(&mut self, mut f: impl FnMut(UeventRef<'_>)) -> usize {
        let now = sysfs::snapshot(&self.paths, &self.filter);
        let mut count = 0;
        for bytes in now.values() {
            if let Some(msg) = UeventMsg::parse(bytes) {
                count += 1;
                f(UeventRef::Event(msg));
            }
        }
        if let Source::Rescan(baseline) = &mut self.source {
            *baseline = now;
        }
        count
    }

    /// Calls `f` for each change since the last call, without blocking.
    /// Returns how many records were consumed; `0` means nothing was pending
    /// (the kernel queue is drained). Records outside the filter are consumed
    /// but not reported. [`UeventRef::Overflow`] means events were lost:
    /// coldplug the filter again.
    pub fn read(&mut self, mut f: impl FnMut(UeventRef<'_>)) -> io::Result<usize> {
        let DeviceWatcher {
            paths,
            filter,
            source,
            scratch,
        } = self;
        let filter = *filter;
        match source {
            Source::Netlink(socket) => socket.drain(|item| match item {
                UeventRef::Event(msg) if !filter.matches(&msg) => {}
                item => f(item),
            }),
            Source::Inotify(watches) => watches.read(paths, &filter, scratch, &mut f),
            Source::Rescan(baseline) => {
                let now = sysfs::snapshot(paths, &filter);
                let changes = sysfs::diff(baseline, &now);
                *baseline = now;
                let count = changes.len();
                for bytes in &changes {
                    if let Some(msg) = UeventMsg::parse(bytes) {
                        f(UeventRef::Event(msg));
                    }
                }
                Ok(count)
            }
        }
    }
}

/// The entries of `root`, sorted. A missing directory is empty, not an error.
fn read_dir_sorted(root: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(root) {
        Ok(entries) => {
            let mut paths = entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?;
            paths.sort();
            Ok(paths)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}
