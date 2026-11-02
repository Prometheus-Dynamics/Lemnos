//! Lemnos's hotplug watcher: the generic [`DeviceWatcher`] over Lemnos's
//! subsystems, reporting which interfaces to refresh.

use crate::LinuxPaths;
use crate::device_watch::{DeviceWatchSource, DeviceWatcher, Filter};
use crate::uevent::{UeventMsg, UeventRef};
use lemnos_core::InterfaceKind;
use lemnos_discovery::{DiscoveryError, DiscoveryResult, InventoryWatchEvent, InventoryWatcher};
use std::collections::BTreeSet;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::PathBuf;

const WATCHER_NAME: &str = "linux.hotplug";

/// The subsystems Lemnos's interfaces live under.
pub(crate) const LEMNOS_FILTER: Filter = Filter::subsystems(&[
    "gpio",
    "leds",
    "pwm",
    "hwmon",
    "i2c",
    "i2c-dev",
    "spi",
    "spidev",
    "spi_master",
    "tty",
    "serial",
    "usb",
    "usbmisc",
]);

/// Where a [`LinuxHotplugWatcher`] learns about changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotplugSource {
    /// Kernel uevents over netlink (`NETLINK_KOBJECT_UEVENT`): every device
    /// add, remove, bind and unbind, without polling sysfs. Needs the real
    /// `/sys`.
    Uevent,
    /// inotify watches on the sysfs and devfs directories Lemnos probes: works
    /// on any root (test trees included) and where netlink is refused.
    Inotify,
}

/// Watches for hardware changes and reports which interfaces to refresh.
///
/// [`LinuxHotplugWatcher::new`] picks netlink uevents when `paths` is the real
/// system root and the socket can be opened, and inotify otherwise; the
/// choice is visible through [`source`](Self::source). Both are non-blocking
/// and expose their descriptor through [`AsFd`] for `poll(2)`/`epoll` or an
/// async reactor.
#[derive(Debug)]
pub struct LinuxHotplugWatcher {
    paths: LinuxPaths,
    source: HotplugSource,
    inner: DeviceWatcher,
}

impl LinuxHotplugWatcher {
    /// Uevents on the real system root (falling back to inotify if the
    /// socket is refused), inotify on any other root.
    pub fn new(paths: LinuxPaths) -> DiscoveryResult<Self> {
        let inner = DeviceWatcher::fd_backed(paths.clone(), LEMNOS_FILTER)
            .map_err(|error| watch_error(WATCHER_NAME, error))?;
        Self::from_inner(paths, inner)
    }

    /// Uevents only; fails if the socket cannot be opened.
    pub fn with_uevents(paths: LinuxPaths) -> DiscoveryResult<Self> {
        let inner = DeviceWatcher::netlink(paths.clone(), LEMNOS_FILTER)
            .map_err(|error| watch_error(WATCHER_NAME, error))?;
        Self::from_inner(paths, inner)
    }

    /// inotify watches on the probed directories under `paths`.
    pub fn with_inotify(paths: LinuxPaths) -> DiscoveryResult<Self> {
        let inner = DeviceWatcher::inotify(paths.clone(), LEMNOS_FILTER)
            .map_err(|error| watch_error(WATCHER_NAME, error))?;
        Self::from_inner(paths, inner)
    }

    fn from_inner(paths: LinuxPaths, inner: DeviceWatcher) -> DiscoveryResult<Self> {
        let source = match inner.source() {
            DeviceWatchSource::Netlink => HotplugSource::Uevent,
            DeviceWatchSource::Inotify => HotplugSource::Inotify,
            DeviceWatchSource::Rescan => {
                return Err(watch_error(
                    WATCHER_NAME,
                    io::Error::other("no watch descriptor"),
                ));
            }
        };
        Ok(Self {
            paths,
            source,
            inner,
        })
    }

    /// Where this watcher learns about changes.
    pub fn source(&self) -> HotplugSource {
        self.source
    }

    /// Reads pending changes and maps them to interfaces. `WouldBlock` when
    /// the kernel queue was empty (async callers clear readiness then).
    pub(crate) fn read_batch(&mut self) -> Result<Vec<InventoryWatchEvent>, ReadBatchError> {
        let mut interfaces = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let roots = &self.paths;
        let consumed = self
            .inner
            .read(|item| match item {
                UeventRef::Overflow => {
                    watch_warn!("uevent queue overflowed; scheduling full interface refresh");
                    interfaces.extend(all_interfaces());
                }
                UeventRef::Event(msg) => {
                    let Some(interface) = msg.subsystem().and_then(uevent_interface) else {
                        return;
                    };
                    watch_debug!(
                        action = msg.action_str(),
                        devpath = msg.devpath(),
                        "linux hotplug uevent"
                    );
                    interfaces.insert(interface);
                    paths.insert(event_path(roots, &msg));
                }
            })
            .map_err(|error| ReadBatchError::Failed(watch_error(WATCHER_NAME, error)))?;

        if consumed == 0 {
            return Err(ReadBatchError::WouldBlock);
        }
        if interfaces.is_empty() && paths.is_empty() {
            return Ok(Vec::new());
        }
        watch_info!(
            touched_interfaces = interfaces.len(),
            touched_paths = paths.len(),
            "linux hotplug watcher observed inventory changes"
        );
        Ok(vec![InventoryWatchEvent::new(
            WATCHER_NAME,
            interfaces.into_iter().collect(),
            paths.into_iter().collect(),
        )])
    }
}

/// Outcome of a single non-blocking read that produced no batch.
/// `WouldBlock` means the kernel queue is drained, which async callers need
/// to distinguish from "read events, none relevant" before clearing readiness.
pub(crate) enum ReadBatchError {
    WouldBlock,
    Failed(DiscoveryError),
}

impl InventoryWatcher for LinuxHotplugWatcher {
    fn name(&self) -> &'static str {
        WATCHER_NAME
    }

    fn poll(&mut self) -> DiscoveryResult<Vec<InventoryWatchEvent>> {
        match self.read_batch() {
            Ok(events) => Ok(events),
            Err(ReadBatchError::WouldBlock) => Ok(Vec::new()),
            Err(ReadBatchError::Failed(error)) => Err(error),
        }
    }
}

/// The Lemnos interface a subsystem belongs to.
pub(crate) fn uevent_interface(subsystem: &str) -> Option<InterfaceKind> {
    match subsystem {
        "gpio" | "leds" => Some(InterfaceKind::Gpio),
        "pwm" | "hwmon" => Some(InterfaceKind::Pwm),
        "i2c" | "i2c-dev" => Some(InterfaceKind::I2c),
        "spi" | "spidev" | "spi_master" => Some(InterfaceKind::Spi),
        "tty" | "serial" => Some(InterfaceKind::Uart),
        "usb" | "usbmisc" => Some(InterfaceKind::Usb),
        _ => None,
    }
}

impl AsFd for LinuxHotplugWatcher {
    /// The underlying non-blocking uevent socket or inotify descriptor. It
    /// becomes readable when [`InventoryWatcher::poll`] has events to report,
    /// so callers can wait on it with `poll(2)`, `epoll`, or an async reactor
    /// instead of sleeping.
    fn as_fd(&self) -> BorrowedFd<'_> {
        // Constructed only from descriptor-backed watchers (see `from_inner`).
        self.inner
            .fd()
            .expect("LinuxHotplugWatcher is always descriptor-backed")
    }
}

impl AsRawFd for LinuxHotplugWatcher {
    fn as_raw_fd(&self) -> RawFd {
        self.as_fd().as_raw_fd()
    }
}

fn all_interfaces() -> impl Iterator<Item = InterfaceKind> {
    [
        InterfaceKind::Gpio,
        InterfaceKind::Pwm,
        InterfaceKind::I2c,
        InterfaceKind::Spi,
        InterfaceKind::Uart,
        InterfaceKind::Usb,
    ]
    .into_iter()
}

/// The host path a device event names: its sysfs devpath below the sysfs
/// mount, or the `/dev` node for events that carry only a `DEVNAME`.
fn event_path(paths: &LinuxPaths, msg: &UeventMsg<'_>) -> PathBuf {
    if !msg.devpath().is_empty() {
        return paths.sys_root().join(msg.devpath().trim_start_matches('/'));
    }
    match msg.devname() {
        Some(name) => paths.dev_root.join(name),
        None => paths.sys_root(),
    }
}

fn watch_error(watcher: &str, error: io::Error) -> DiscoveryError {
    DiscoveryError::WatchFailed {
        watcher: watcher.to_string(),
        message: error.to_string(),
    }
}
