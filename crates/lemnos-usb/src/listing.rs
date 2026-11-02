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
pub struct UsbWatch {
    watch: nusb::hotplug::HotplugWatch,
    known: Vec<(nusb::DeviceId, UsbLocation)>,
}

/// Starts watching for devices. The watch starts before the initial listing,
/// so a device that arrives in between is not missed.
pub fn watch() -> Result<UsbWatch> {
    let watch =
        nusb::watch_devices().map_err(|error| Error::from_nusb("watching USB devices", &error))?;
    let known = list_devices_with_ids()?;
    Ok(UsbWatch { watch, known })
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

impl UsbWatch {
    /// Polls for the next event, from an async task or a poll loop.
    pub fn poll_next_event(&mut self, cx: &mut Context<'_>) -> Poll<Option<UsbHotplugEvent>> {
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
                    let Some(index) = self.known.iter().position(|(known, _)| *known == id) else {
                        continue;
                    };
                    let (_, location) = self.known.swap_remove(index);
                    return Poll::Ready(Some(UsbHotplugEvent::Disconnected(location)));
                }
            }
        }
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
