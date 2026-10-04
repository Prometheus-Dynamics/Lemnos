use crate::LinuxPaths;
use crate::uevent::{Uevent, UeventRecv, UeventSocket};
use crate::util::read_dir_sorted;
use lemnos_core::InterfaceKind;
use lemnos_discovery::{DiscoveryError, DiscoveryResult, InventoryWatchEvent, InventoryWatcher};
use lemnos_linux_sys::inotify::{self as sys_inotify, mask};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::{Path, PathBuf};

const WATCHER_NAME: &str = "linux.hotplug";
pub const DEFAULT_EVENT_BUFFER_SIZE: usize = 16 * 1024;
const WATCH_MASK: u32 = mask::CREATE
    | mask::DELETE
    | mask::MOVED_FROM
    | mask::MOVED_TO
    | mask::ATTRIB
    | mask::DELETE_SELF
    | mask::MOVE_SELF;

#[cfg(feature = "tracing")]
macro_rules! watch_debug {
    ($($arg:tt)*) => {
        { tracing::debug!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! watch_debug {
    ($($arg:tt)*) => {};
}

#[cfg(feature = "tracing")]
macro_rules! watch_info {
    ($($arg:tt)*) => {
        { tracing::info!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! watch_info {
    ($($arg:tt)*) => {};
}

#[cfg(feature = "tracing")]
macro_rules! watch_warn {
    ($($arg:tt)*) => {
        { tracing::warn!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! watch_warn {
    ($($arg:tt)*) => {};
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchRegistration {
    path: PathBuf,
    interfaces: Vec<InterfaceKind>,
    dynamic: bool,
}

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
    source: Source,
}

#[derive(Debug)]
enum Source {
    Uevent(UeventSocket),
    Inotify(InotifyWatches),
}

#[derive(Debug)]
struct InotifyWatches {
    file: File,
    buffer: Vec<u8>,
    watched_paths: BTreeMap<PathBuf, i32>,
    registrations: BTreeMap<i32, WatchRegistration>,
}

impl LinuxHotplugWatcher {
    /// Uevents on the real system root (falling back to inotify if the
    /// socket is refused), inotify on any other root.
    pub fn new(paths: LinuxPaths) -> DiscoveryResult<Self> {
        if paths.sys_class_root == Path::new("/sys/class") {
            match UeventSocket::open() {
                Ok(socket) => {
                    watch_info!("linux hotplug watcher using netlink uevents");
                    return Ok(Self {
                        paths,
                        source: Source::Uevent(socket),
                    });
                }
                Err(_error) => {
                    watch_warn!(error = %_error, "uevent socket unavailable; falling back to inotify");
                }
            }
        }
        Self::with_inotify(paths)
    }

    /// Uevents only; fails if the socket cannot be opened.
    pub fn with_uevents(paths: LinuxPaths) -> DiscoveryResult<Self> {
        let socket = UeventSocket::open().map_err(|error| watch_error(WATCHER_NAME, error))?;
        Ok(Self {
            paths,
            source: Source::Uevent(socket),
        })
    }

    /// inotify watches on the probed directories under `paths`.
    pub fn with_inotify(paths: LinuxPaths) -> DiscoveryResult<Self> {
        let fd = sys_inotify::init().map_err(|error| watch_error(WATCHER_NAME, error))?;
        let mut watcher = Self {
            paths,
            source: Source::Inotify(InotifyWatches {
                file: File::from(fd),
                buffer: vec![0; DEFAULT_EVENT_BUFFER_SIZE],
                watched_paths: BTreeMap::new(),
                registrations: BTreeMap::new(),
            }),
        };

        watcher.add_static_watch(watcher.paths.gpio_class_root(), InterfaceKind::Gpio)?;
        watcher.add_static_watch(watcher.paths.led_class_root(), InterfaceKind::Gpio)?;
        watcher.add_static_watch(watcher.paths.pwm_class_root(), InterfaceKind::Pwm)?;
        watcher.add_static_watch(watcher.paths.hwmon_class_root(), InterfaceKind::Pwm)?;
        watcher.add_static_watch(watcher.paths.i2c_class_root(), InterfaceKind::I2c)?;
        watcher.add_static_watch(watcher.paths.i2c_devices_root(), InterfaceKind::I2c)?;
        watcher.add_static_watch(watcher.paths.spi_devices_root(), InterfaceKind::Spi)?;
        watcher.add_static_watch(watcher.paths.tty_class_root(), InterfaceKind::Uart)?;
        watcher.add_static_watch(watcher.paths.usb_devices_root(), InterfaceKind::Usb)?;
        watcher.sync_pwm_chip_watches()?;

        watch_info!(
            watched_paths = watcher.inotify().map_or(0, |w| w.watched_paths.len()),
            "linux hotplug watcher initialized (inotify)"
        );

        Ok(watcher)
    }

    /// Where this watcher learns about changes.
    pub fn source(&self) -> HotplugSource {
        match self.source {
            Source::Uevent(_) => HotplugSource::Uevent,
            Source::Inotify(_) => HotplugSource::Inotify,
        }
    }

    fn inotify(&self) -> Option<&InotifyWatches> {
        match &self.source {
            Source::Inotify(w) => Some(w),
            Source::Uevent(_) => None,
        }
    }

    fn inotify_mut(&mut self) -> Option<&mut InotifyWatches> {
        match &mut self.source {
            Source::Inotify(w) => Some(w),
            Source::Uevent(_) => None,
        }
    }

    fn add_static_watch(&mut self, path: PathBuf, interface: InterfaceKind) -> DiscoveryResult<()> {
        self.add_watch(path, vec![interface], false)
    }

    fn add_watch(
        &mut self,
        path: PathBuf,
        interfaces: Vec<InterfaceKind>,
        dynamic: bool,
    ) -> DiscoveryResult<()> {
        let Some(watches) = self.inotify_mut() else {
            return Ok(());
        };
        if watches.watched_paths.contains_key(&path) || !path.exists() {
            return Ok(());
        }

        let descriptor = sys_inotify::add_watch(watches.file.as_fd(), &path, WATCH_MASK)
            .map_err(|error| watch_error(WATCHER_NAME, error))?;

        let registration = WatchRegistration {
            path: path.clone(),
            interfaces,
            dynamic,
        };
        watch_debug!(
            path = %path.display(),
            interfaces = ?registration.interfaces,
            dynamic = dynamic,
            "linux hotplug watch registered"
        );
        watches.watched_paths.insert(path, descriptor);
        watches.registrations.insert(descriptor, registration);
        Ok(())
    }

    fn remove_watch(&mut self, path: &Path) -> DiscoveryResult<()> {
        let Some(watches) = self.inotify_mut() else {
            return Ok(());
        };
        let Some(descriptor) = watches.watched_paths.remove(path) else {
            return Ok(());
        };
        watches.registrations.remove(&descriptor);
        watch_debug!(path = %path.display(), "linux hotplug watch removed");
        match sys_inotify::rm_watch(watches.file.as_fd(), descriptor) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(watch_error(WATCHER_NAME, error)),
        }
    }

    fn sync_pwm_chip_watches(&mut self) -> DiscoveryResult<()> {
        if self.inotify().is_none() {
            return Ok(());
        }
        let pwm_root = self.paths.pwm_class_root();
        let current_chip_paths = read_dir_sorted(&pwm_root)
            .map_err(|error| watch_error(WATCHER_NAME, error))?
            .into_iter()
            .filter(|path| path.is_dir())
            .filter(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with("pwmchip"))
            })
            .collect::<BTreeSet<_>>();

        let stale_chip_paths = self
            .inotify()
            .map(|w| &w.registrations)
            .into_iter()
            .flatten()
            .map(|(_, registration)| registration)
            .filter(|registration| {
                registration.dynamic
                    && registration.interfaces.as_slice() == [InterfaceKind::Pwm]
                    && !current_chip_paths.contains(&registration.path)
            })
            .map(|registration| registration.path.clone())
            .collect::<Vec<_>>();

        for stale in stale_chip_paths {
            self.remove_watch(&stale)?;
        }

        for chip_path in current_chip_paths {
            self.add_watch(chip_path, vec![InterfaceKind::Pwm], true)?;
        }

        watch_debug!(
            watched_paths = self.inotify().map_or(0, |w| w.watched_paths.len()),
            "linux hotplug pwm watch set synchronized"
        );

        Ok(())
    }
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

/// Outcome of a single non-blocking inotify read that failed to produce a
/// batch. `WouldBlock` means the kernel queue is drained, which async callers
/// need to distinguish from "read events, none relevant" before clearing
/// readiness.
pub(crate) enum ReadBatchError {
    WouldBlock,
    Failed(DiscoveryError),
}

impl From<DiscoveryError> for ReadBatchError {
    fn from(error: DiscoveryError) -> Self {
        Self::Failed(error)
    }
}

impl LinuxHotplugWatcher {
    pub(crate) fn read_batch(&mut self) -> Result<Vec<InventoryWatchEvent>, ReadBatchError> {
        let (interfaces, paths) = match &mut self.source {
            Source::Uevent(_) => self.read_uevents()?,
            Source::Inotify(_) => self.read_inotify()?,
        };

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

    /// Drains pending uevents. `WouldBlock` when there were none at all.
    fn read_uevents(&mut self) -> Result<Touched, ReadBatchError> {
        let sys_root = self
            .paths
            .sys_class_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/sys"));
        let Source::Uevent(socket) = &mut self.source else {
            return Ok(Touched::default());
        };
        let mut interfaces = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut received = 0usize;
        loop {
            match socket.recv() {
                Ok(None) => break,
                Ok(Some(UeventRecv::Overflow)) => {
                    received += 1;
                    watch_warn!("uevent queue overflowed; scheduling full interface refresh");
                    interfaces.extend(LinuxHotplugWatcher::all_interfaces());
                }
                Ok(Some(UeventRecv::Event(event))) => {
                    received += 1;
                    if let Some(interface) = uevent_interface(&event) {
                        watch_debug!(
                            action = %event.action,
                            devpath = %event.devpath,
                            "linux hotplug uevent"
                        );
                        interfaces.insert(interface);
                        paths.insert(sys_root.join(event.devpath.trim_start_matches('/')));
                    }
                }
                Err(error) => return Err(watch_error(WATCHER_NAME, error).into()),
            }
        }
        if received == 0 {
            return Err(ReadBatchError::WouldBlock);
        }
        Ok((interfaces, paths))
    }

    fn read_inotify(&mut self) -> Result<Touched, ReadBatchError> {
        self.sync_pwm_chip_watches()?;

        let Some(watches) = self.inotify_mut() else {
            return Ok(Touched::default());
        };
        let events = match watches.file.read(&mut watches.buffer) {
            Ok(n) => sys_inotify::parse_events(&watches.buffer[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(ReadBatchError::WouldBlock);
            }
            Err(error) => return Err(watch_error(WATCHER_NAME, error).into()),
        };

        let mut interfaces = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut needs_pwm_resync = false;

        for event in events {
            if event.mask & mask::Q_OVERFLOW != 0 {
                watch_warn!(
                    "linux hotplug watcher queue overflowed; scheduling full interface refresh"
                );
                interfaces.extend(LinuxHotplugWatcher::all_interfaces());
                continue;
            }

            let Some(registration) = watches.registrations.get(&event.wd).cloned() else {
                continue;
            };

            interfaces.extend(registration.interfaces.iter().copied());
            paths.insert(event_path(&registration.path, event.name.as_deref()));

            if registration.interfaces.contains(&InterfaceKind::Pwm) {
                needs_pwm_resync = true;
            }

            if event.mask & mask::IGNORED != 0 {
                watches.watched_paths.remove(&registration.path);
                watches.registrations.remove(&event.wd);
            }
        }

        if needs_pwm_resync {
            self.sync_pwm_chip_watches()?;
        }

        Ok((interfaces, paths))
    }
}

type Touched = (BTreeSet<InterfaceKind>, BTreeSet<PathBuf>);

/// The Lemnos interface a uevent's subsystem belongs to.
pub(crate) fn uevent_interface(event: &Uevent) -> Option<InterfaceKind> {
    match event.subsystem()? {
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
        match &self.source {
            Source::Uevent(socket) => socket.as_fd(),
            Source::Inotify(watches) => watches.file.as_fd(),
        }
    }
}

impl AsRawFd for LinuxHotplugWatcher {
    fn as_raw_fd(&self) -> RawFd {
        self.as_fd().as_raw_fd()
    }
}

impl LinuxHotplugWatcher {
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
}

fn event_path(root: &Path, name: Option<&OsStr>) -> PathBuf {
    match name {
        Some(name) => root.join(name),
        None => root.to_path_buf(),
    }
}

fn watch_error(watcher: &str, error: io::Error) -> DiscoveryError {
    DiscoveryError::WatchFailed {
        watcher: watcher.to_string(),
        message: error.to_string(),
    }
}
