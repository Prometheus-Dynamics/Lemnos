use crate::LinuxPaths;
use crate::util::read_dir_sorted;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use lemnos_core::InterfaceKind;
use lemnos_discovery::{DiscoveryError, DiscoveryResult, InventoryWatchEvent, InventoryWatcher};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

const WATCHER_NAME: &str = "linux.hotplug";
pub const DEFAULT_EVENT_BUFFER_SIZE: usize = 16 * 1024;
const WATCH_MASK: WatchMask = WatchMask::CREATE
    .union(WatchMask::DELETE)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::ATTRIB)
    .union(WatchMask::DELETE_SELF)
    .union(WatchMask::MOVE_SELF);

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

#[derive(Debug)]
pub struct LinuxHotplugWatcher {
    paths: LinuxPaths,
    inotify: Inotify,
    buffer: Vec<u8>,
    watched_paths: BTreeMap<PathBuf, WatchDescriptor>,
    registrations: BTreeMap<WatchDescriptor, WatchRegistration>,
}

impl LinuxHotplugWatcher {
    pub fn new(paths: LinuxPaths) -> DiscoveryResult<Self> {
        let inotify = Inotify::init().map_err(|error| watch_error(WATCHER_NAME, error))?;
        let mut watcher = Self {
            paths,
            inotify,
            buffer: vec![0; DEFAULT_EVENT_BUFFER_SIZE],
            watched_paths: BTreeMap::new(),
            registrations: BTreeMap::new(),
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
            watched_paths = watcher.watched_paths.len(),
            "linux hotplug watcher initialized"
        );

        Ok(watcher)
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
        if self.watched_paths.contains_key(&path) || !path.exists() {
            return Ok(());
        }

        let descriptor = self
            .inotify
            .watches()
            .add(&path, WATCH_MASK)
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
        self.watched_paths.insert(path, descriptor.clone());
        self.registrations.insert(descriptor, registration);
        Ok(())
    }

    fn remove_watch(&mut self, path: &Path) -> DiscoveryResult<()> {
        let Some(descriptor) = self.watched_paths.remove(path) else {
            return Ok(());
        };
        self.registrations.remove(&descriptor);
        watch_debug!(path = %path.display(), "linux hotplug watch removed");
        match self.inotify.watches().remove(descriptor) {
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
            .registrations
            .values()
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
            watched_paths = self.watched_paths.len(),
            "linux hotplug pwm watch set synchronized"
        );

        Ok(())
    }

    fn registration_for(&self, descriptor: &WatchDescriptor) -> Option<&WatchRegistration> {
        self.registrations.get(descriptor)
    }
}

impl InventoryWatcher for LinuxHotplugWatcher {
    fn name(&self) -> &'static str {
        WATCHER_NAME
    }

    fn poll(&mut self) -> DiscoveryResult<Vec<InventoryWatchEvent>> {
        self.sync_pwm_chip_watches()?;

        let events = match self.inotify.read_events(&mut self.buffer) {
            Ok(events) => events.map(|event| event.to_owned()).collect::<Vec<_>>(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(Vec::new()),
            Err(error) => return Err(watch_error(self.name(), error)),
        };

        if events.is_empty() {
            return Ok(Vec::new());
        }

        let mut interfaces = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut needs_pwm_resync = false;

        for event in events {
            if event.mask.contains(EventMask::Q_OVERFLOW) {
                watch_warn!(
                    "linux hotplug watcher queue overflowed; scheduling full interface refresh"
                );
                interfaces.extend(LinuxHotplugWatcher::all_interfaces());
                continue;
            }

            let Some(registration) = self.registration_for(&event.wd).cloned() else {
                continue;
            };

            interfaces.extend(registration.interfaces.iter().copied());
            paths.insert(event_path(&registration.path, event.name.as_deref()));

            if registration.interfaces.contains(&InterfaceKind::Pwm) {
                needs_pwm_resync = true;
            }

            if event.mask.contains(EventMask::IGNORED) {
                self.watched_paths.remove(&registration.path);
                self.registrations.remove(&event.wd);
            }
        }

        if needs_pwm_resync {
            self.sync_pwm_chip_watches()?;
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
            self.name(),
            interfaces.into_iter().collect(),
            paths.into_iter().collect(),
        )])
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
