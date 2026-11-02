//! Listing the devices on the bus, finding one by location, and watching for
//! arrivals and removals.

use crate::error::{Error, Result};
use crate::info::UsbDeviceInfo;
use crate::location::UsbLocation;
use crate::wait::block_on_poll;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The devices on the bus now, in the platform's order.
pub fn list_devices() -> Result<Vec<UsbDeviceInfo>> {
    use nusb::MaybeFuture;
    let devices = nusb::list_devices()
        .wait()
        .map_err(|error| Error::from_nusb("listing USB devices", &error))?;
    Ok(devices
        .map(|info| UsbDeviceInfo::from_nusb(&info))
        .collect())
}

/// The device at `location` now, if one is there.
pub fn find_device(location: &UsbLocation) -> Result<Option<UsbDeviceInfo>> {
    Ok(list_devices()?
        .into_iter()
        .find(|info| info.location() == location))
}

/// A device arriving or leaving.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // as nusb does: the arrival carries the whole listing
pub enum UsbHotplugEvent {
    /// A device arrived.
    Connected(UsbDeviceInfo),
    /// The device at this location left. A device that left before its
    /// arrival was seen is not reported.
    Disconnected(UsbLocation),
}

/// A stream of [`UsbHotplugEvent`]s. Start one with [`watch`].
///
/// On Linux the events come from `lemnos-uevent`'s `DeviceWatcher` (subsystem
/// `usb`, devtype `usb_device`), the same watcher the camera stack uses. On
/// Windows and macOS they come from nusb's watch.
pub struct UsbWatch {
    inner: WatchInner,
}

/// Starts watching for devices. The watch starts before the initial listing,
/// so a device that arrives in between is not missed.
pub fn watch() -> Result<UsbWatch> {
    Ok(UsbWatch {
        inner: WatchInner::start()?,
    })
}

impl UsbWatch {
    /// Polls for the next event, from an async task or a poll loop.
    pub fn poll_next_event(&mut self, cx: &mut Context<'_>) -> Poll<Option<UsbHotplugEvent>> {
        self.inner.poll_next_event(cx)
    }

    /// Blocks for the next event. `None` when the watch has ended.
    pub fn next_event(&mut self) -> Option<UsbHotplugEvent> {
        block_on_poll(|cx| self.poll_next_event(cx))
    }
}

impl futures_core::Stream for UsbWatch {
    type Item = UsbHotplugEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().poll_next_event(cx)
    }
}

#[cfg(target_os = "linux")]
use linux_watch::WatchInner;
#[cfg(not(target_os = "linux"))]
use nusb_watch::WatchInner;

/// The Linux watch: `lemnos-uevent`'s device watcher, filtered to USB devices.
#[cfg(target_os = "linux")]
mod linux_watch {
    use super::*;
    use crate::location::UsbLocation;
    use lemnos_linux_sys::poll;
    use lemnos_uevent::uevent::{Action, UeventRef};
    use lemnos_uevent::{DeviceWatcher, Filter, LinuxPaths};
    use std::collections::VecDeque;
    use std::os::fd::{AsFd, OwnedFd};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::Waker;
    use std::time::Duration;

    /// A device record from the watcher, before it is checked against the listing.
    enum Record {
        Arrived(UsbLocation),
        Left(UsbLocation),
    }

    /// USB devices (not their interfaces, which are `usb_interface`).
    const FILTER: Filter = Filter::subsystems(&["usb"]).with_devtypes(&["usb_device"]);

    /// How often a rescan watcher, which has no descriptor, is looked at.
    const RESCAN_INTERVAL: Duration = Duration::from_millis(250);

    pub(super) struct WatchInner {
        watcher: DeviceWatcher,
        /// The devices reported present (arrivals not yet matched by a removal).
        known: Vec<UsbLocation>,
        pending: VecDeque<UsbHotplugEvent>,
        /// The waker of a pending poll, and whether a thread is waiting to wake it.
        waker: Arc<Mutex<Option<Waker>>>,
        armed: Arc<AtomicBool>,
    }

    impl WatchInner {
        pub(super) fn start() -> Result<Self> {
            // Watch first, then list: a device that arrives in between is seen
            // as an arrival, not missed.
            let watcher = DeviceWatcher::new(LinuxPaths::default(), FILTER);
            let known = list_devices()?
                .into_iter()
                .map(|info| info.location().clone())
                .collect();
            Ok(Self {
                watcher,
                known,
                pending: VecDeque::new(),
                waker: Arc::new(Mutex::new(None)),
                armed: Arc::new(AtomicBool::new(false)),
            })
        }

        pub(super) fn poll_next_event(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Option<UsbHotplugEvent>> {
            loop {
                if let Some(event) = self.pending.pop_front() {
                    return Poll::Ready(Some(event));
                }
                match self.read_pending() {
                    // Records were consumed (some may have been filtered out): look again.
                    Ok(consumed) if consumed > 0 => continue,
                    Ok(_) => {}
                    // The watch cannot continue: the stream ends, as nusb's does.
                    Err(_) => return Poll::Ready(None),
                }
                if !self.pending.is_empty() {
                    continue;
                }
                self.arm(cx.waker());
                return Poll::Pending;
            }
        }

        /// Reads the watcher's records and turns them into events; returns how
        /// many records were consumed.
        fn read_pending(&mut self) -> Result<usize> {
            // Records first, then the listing: the watcher's callback only
            // collects them, so the listing (which can fail) runs with no
            // borrow of the watcher.
            let mut records: Vec<Record> = Vec::new();
            let mut overflowed = false;
            let consumed = self
                .watcher
                .read(|item| match item {
                    UeventRef::Overflow => overflowed = true,
                    UeventRef::Event(msg) => {
                        let Some(location) = devpath_location(msg.devpath()) else {
                            return;
                        };
                        match msg.action() {
                            Action::Add => records.push(Record::Arrived(location)),
                            Action::Remove => records.push(Record::Left(location)),
                            _ => {}
                        }
                    }
                })
                .map_err(|error| Error::from_io("reading USB device events", &error))?;
            for record in records {
                match record {
                    Record::Arrived(location) => {
                        if self.known.contains(&location) {
                            continue;
                        }
                        // A device gone again by the time it is listed is not reported.
                        if let Some(info) = find_device(&location)? {
                            self.known.push(location);
                            self.pending.push_back(UsbHotplugEvent::Connected(info));
                        }
                    }
                    Record::Left(location) => {
                        if let Some(index) = self.known.iter().position(|k| *k == location) {
                            self.known.swap_remove(index);
                            self.pending
                                .push_back(UsbHotplugEvent::Disconnected(location));
                        }
                    }
                }
            }
            if overflowed {
                self.resync()?;
            }
            Ok(consumed)
        }

        /// After an overflow: diffs the listing against the devices known.
        fn resync(&mut self) -> Result<()> {
            let now = list_devices()?;
            for info in &now {
                if !self.known.contains(info.location()) {
                    self.known.push(info.location().clone());
                    self.pending
                        .push_back(UsbHotplugEvent::Connected(info.clone()));
                }
            }
            let gone: Vec<UsbLocation> = self
                .known
                .iter()
                .filter(|k| !now.iter().any(|info| info.location() == *k))
                .cloned()
                .collect();
            for location in gone {
                self.known.retain(|k| *k != location);
                self.pending
                    .push_back(UsbHotplugEvent::Disconnected(location));
            }
            Ok(())
        }

        /// Makes sure the waker is called when the watcher has something to read.
        /// A thread waits on a duplicate of the watcher's descriptor (or sleeps,
        /// for a rescan watcher) and then wakes the task.
        fn arm(&mut self, waker: &Waker) {
            *self
                .waker
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(waker.clone());
            if self.armed.swap(true, Ordering::SeqCst) {
                return;
            }
            let fd: Option<OwnedFd> = self
                .watcher
                .fd()
                .and_then(|fd| fd.as_fd().try_clone_to_owned().ok());
            let slot = Arc::clone(&self.waker);
            let armed = Arc::clone(&self.armed);
            let spawned = std::thread::Builder::new()
                .name("lemnos-usb-watch".into())
                .spawn(move || {
                    match fd {
                        Some(fd) => {
                            let _ = poll::poll_one(fd.as_fd(), poll::POLLIN, None);
                        }
                        None => std::thread::sleep(RESCAN_INTERVAL),
                    }
                    armed.store(false, Ordering::SeqCst);
                    let waker = slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take();
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                });
            if spawned.is_err() {
                // No thread: the caller's waker is never called. Clear the flag so
                // a later poll can try again.
                self.armed.store(false, Ordering::SeqCst);
            }
        }
    }

    /// The location a devpath names: its last component (`/devices/.../3-1.2`).
    /// Interfaces (`3-1:1.0`) and root hubs (`usb3`) are not device locations.
    pub(super) fn devpath_location(devpath: &str) -> Option<UsbLocation> {
        let name = devpath.rsplit('/').next()?;
        if name.contains(':') {
            return None;
        }
        name.parse().ok()
    }
}

#[cfg(not(target_os = "linux"))]
mod nusb_watch {
    use super::*;

    pub(super) struct WatchInner {
        watch: nusb::hotplug::HotplugWatch,
        known: Vec<(nusb::DeviceId, UsbLocation)>,
    }

    impl WatchInner {
        pub(super) fn start() -> Result<Self> {
            let watch = nusb::watch_devices()
                .map_err(|error| Error::from_nusb("watching USB devices", &error))?;
            let known = list_devices_with_ids()?;
            Ok(Self { watch, known })
        }

        pub(super) fn poll_next_event(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Option<UsbHotplugEvent>> {
            use futures_core::Stream;
            loop {
                match Pin::new(&mut self.watch).poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Ready(Some(nusb::hotplug::HotplugEvent::Connected(info))) => {
                        let device = UsbDeviceInfo::from_nusb(&info);
                        self.known.push((info.id(), device.location().clone()));
                        return Poll::Ready(Some(UsbHotplugEvent::Connected(device)));
                    }
                    Poll::Ready(Some(nusb::hotplug::HotplugEvent::Disconnected(id))) => {
                        let Some(index) = self.known.iter().position(|(known, _)| *known == id)
                        else {
                            continue;
                        };
                        let (_, location) = self.known.swap_remove(index);
                        return Poll::Ready(Some(UsbHotplugEvent::Disconnected(location)));
                    }
                }
            }
        }
    }

    fn list_devices_with_ids() -> Result<Vec<(nusb::DeviceId, UsbLocation)>> {
        use nusb::MaybeFuture;
        let devices = nusb::list_devices()
            .wait()
            .map_err(|error| Error::from_nusb("listing USB devices", &error))?;
        Ok(devices
            .map(|info| {
                (
                    info.id(),
                    UsbDeviceInfo::from_nusb(&info).location().clone(),
                )
            })
            .collect())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux_watch::devpath_location;

    #[test]
    fn a_devpath_names_a_device_location_not_an_interface_or_a_root_hub() {
        let device = "/devices/platform/scb/fd500000.pcie/pci0000:00/0000:00:00.0/0000:01:00.0/usb3/3-1/3-1.2";
        assert_eq!(devpath_location(device).unwrap().to_string(), "3-1.2");
        assert!(
            devpath_location("/devices/.../usb3/3-1/3-1:1.0").is_none(),
            "an interface"
        );
        assert!(
            devpath_location("/devices/.../usb3").is_none(),
            "a root hub"
        );
        assert!(devpath_location("/devices/.../3-1.2/unrelated").is_none());
    }
}
