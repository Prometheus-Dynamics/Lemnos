//! The media controller: device info, topology (entities, pads, links, interfaces), link setup
//! and requests.
//!
//! ```no_run
//! use lemnos_v4l2::media::MediaDevice;
//!
//! let media = MediaDevice::open("/dev/media0")?;
//! let topo = media.topology()?;
//! for link in topo.data_links() {
//!     let src = topo.entity(link.source.entity).unwrap();
//!     let sink = topo.entity(link.sink.entity).unwrap();
//!     println!("{}:{} -> {}:{}", src.name, link.source.index, sink.name, link.sink.index);
//! }
//! # Ok::<(), lemnos_v4l2::Error>(())
//! ```

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::media::{
    MEDIA_IOC_DEVICE_INFO, MEDIA_IOC_SETUP_LINK, lock_holder, media_device_info, media_link_desc,
    media_pad_desc, media_v2_entity, media_v2_interface, media_v2_link, media_v2_pad,
    media_v2_topology, request_alloc, request_queue, request_reinit, topology as read_topology,
    topology_counts,
};
use lemnos_linux_sys::poll::POLLPRI;
use lemnos_linux_sys::v4l2::device::zeroed;

use crate::node::{self, cstr_field};
use crate::{Error, Ready, Result};

mod topology;

pub use topology::{
    DataLink, DevNode, Entity, EntityFlags, EntityFunction, Interface, InterfaceType, Link,
    LinkFlags, LinkType, Pad, PadFlags, PadRef, Topology,
};

/// The most objects of one kind read from a media graph. These counts come from the kernel and
/// size allocations, so one past this is refused.
const MAX_TOPOLOGY_OBJECTS: u32 = 1 << 16;

/// The four array lengths for a topology read, each under [`MAX_TOPOLOGY_OBJECTS`]: entities,
/// interfaces, pads, links.
fn checked_counts(counts: &media_v2_topology) -> Result<[usize; 4]> {
    let lengths = [
        counts.num_entities,
        counts.num_interfaces,
        counts.num_pads,
        counts.num_links,
    ];
    if lengths.iter().any(|&n| n > MAX_TOPOLOGY_OBJECTS) {
        return Err(Error::Invalid(format!(
            "the kernel reports an implausible media graph: {lengths:?}"
        )));
    }
    Ok(lengths.map(|n| n as usize))
}

/// A pad as the kernel's link descriptor holds it.
fn pad_desc(pad: PadRef) -> Result<media_pad_desc> {
    let index = u16::try_from(pad.index)
        .map_err(|_| Error::Invalid(format!("pad index {} does not fit", pad.index)))?;
    Ok(media_pad_desc {
        entity: pad.entity,
        index,
        ..Default::default()
    })
}

/// Information about a media device (`MEDIA_IOC_DEVICE_INFO`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Driver name, e.g. `uvcvideo`.
    pub driver: String,
    /// Model, e.g. `UVC Camera (046d:0825)`.
    pub model: String,
    /// Serial number (may be empty).
    pub serial: String,
    /// Bus location.
    pub bus_info: String,
    /// Media API version (`KERNEL_VERSION`-encoded).
    pub media_version: u32,
    /// Hardware revision.
    pub hw_revision: u32,
    /// Driver version (`KERNEL_VERSION`-encoded).
    pub driver_version: u32,
}

/// An open media controller device (`/dev/mediaN`).
#[derive(Debug)]
pub struct MediaDevice {
    fd: OwnedFd,
    path: PathBuf,
}

impl MediaDevice {
    /// Opens a media device read-write, close-on-exec.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Ok(Self {
            fd: node::open(path, true, false)?,
            path: path.to_owned(),
        })
    }

    /// Opens a media device read-only: device info and topology work, link setup and request
    /// allocation do not.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Ok(Self {
            fd: node::open(path, false, false)?,
            path: path.to_owned(),
        })
    }

    /// The path it was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Queries the device information (`MEDIA_IOC_DEVICE_INFO`).
    pub fn device_info(&self) -> Result<DeviceInfo> {
        let mut raw: media_device_info = zeroed();
        node::ioctl(self.as_fd(), MEDIA_IOC_DEVICE_INFO, &mut raw)?;
        Ok(DeviceInfo {
            driver: cstr_field(&raw.driver),
            model: cstr_field(&raw.model),
            serial: cstr_field(&raw.serial),
            bus_info: cstr_field(&raw.bus_info),
            media_version: raw.media_version,
            hw_revision: raw.hw_revision,
            driver_version: raw.driver_version,
        })
    }

    /// Reads the whole graph (`MEDIA_IOC_G_TOPOLOGY`), retrying if it changes while being read.
    pub fn topology(&self) -> Result<Topology> {
        for _ in 0..8 {
            let counts = topology_counts(self.as_fd())
                .map_err(|source| Error::ioctl_io("MEDIA_IOC_G_TOPOLOGY", source))?;
            let [num_entities, num_interfaces, num_pads, num_links] = checked_counts(&counts)?;
            let mut entities = vec![zeroed::<media_v2_entity>(); num_entities];
            let mut interfaces = vec![zeroed::<media_v2_interface>(); num_interfaces];
            let mut pads = vec![zeroed::<media_v2_pad>(); num_pads];
            let mut links = vec![zeroed::<media_v2_link>(); num_links];
            let read = read_topology(
                self.as_fd(),
                &mut entities,
                &mut interfaces,
                &mut pads,
                &mut links,
            );
            let topo = match read {
                Ok(topo) => topo,
                // The graph grew between the two calls.
                Err(e) if e.raw_os_error() == Some(libc::ENOSPC) => continue,
                Err(e) => return Err(Error::ioctl_io("MEDIA_IOC_G_TOPOLOGY", e)),
            };
            if topo.topology_version != counts.topology_version {
                continue;
            }
            entities.truncate(topo.num_entities as usize);
            interfaces.truncate(topo.num_interfaces as usize);
            pads.truncate(topo.num_pads as usize);
            links.truncate(topo.num_links as usize);
            return Ok(Topology::from_raw(
                topo.topology_version,
                &entities,
                &interfaces,
                &pads,
                &links,
            ));
        }
        Err(Error::Invalid(
            "media graph kept changing while being read".into(),
        ))
    }

    /// Enables or disables a data link between two pads (`MEDIA_IOC_SETUP_LINK`). `flags` is
    /// usually [`LinkFlags::ENABLED`] or empty; immutable links cannot be changed. An error
    /// when a pad index does not fit the kernel's 16-bit field.
    pub fn setup_link(&self, source: PadRef, sink: PadRef, flags: LinkFlags) -> Result<()> {
        let mut desc = media_link_desc {
            source: pad_desc(source)?,
            sink: pad_desc(sink)?,
            flags: flags.0,
            ..Default::default()
        };
        node::ioctl(self.as_fd(), MEDIA_IOC_SETUP_LINK, &mut desc)
    }

    /// The process holding a record lock on the device node (`fcntl(F_GETLK)` for a write
    /// lock over the whole file), as a camera stack may take one while using the device.
    /// Takes no lock itself; this process's own locks are not reported.
    pub fn lock_holder(&self) -> Result<Option<i32>> {
        lock_holder(self.as_fd()).map_err(|source| Error::sys_io("fcntl(F_GETLK)", source))
    }

    /// Allocates a request (`MEDIA_IOC_REQUEST_ALLOC`) for the request API.
    pub fn alloc_request(&self) -> Result<Request> {
        let fd = request_alloc(self.as_fd())
            .map_err(|source| Error::ioctl_io("MEDIA_IOC_REQUEST_ALLOC", source))?;
        Ok(Request { fd })
    }
}

impl AsFd for MediaDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl AsRawFd for MediaDevice {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// A media request: a set of buffers and control values applied together.
///
/// Fill it by queueing buffers with [`crate::v4l2::QueueBuffer::request`] and setting controls
/// with [`crate::v4l2::ControlWhich::Request`], then [`Request::queue`] it. Completion makes the
/// descriptor ready for priority (`POLLPRI`).
#[derive(Debug)]
pub struct Request {
    fd: OwnedFd,
}

impl Request {
    /// Queues the request (`MEDIA_REQUEST_IOC_QUEUE`).
    pub fn queue(&self) -> Result<()> {
        request_queue(self.as_fd())
            .map_err(|source| Error::ioctl_io("MEDIA_REQUEST_IOC_QUEUE", source))
    }

    /// Empties a completed (or never queued) request for reuse (`MEDIA_REQUEST_IOC_REINIT`).
    pub fn reinit(&self) -> Result<()> {
        request_reinit(self.as_fd())
            .map_err(|source| Error::ioctl_io("MEDIA_REQUEST_IOC_REINIT", source))
    }

    /// Waits until the request completes, or the timeout passes (`false`).
    pub fn wait(&self, timeout: Option<Duration>) -> Result<bool> {
        let ready: Ready = node::wait_events(self.as_fd(), POLLPRI, timeout)?;
        Ok(ready.priority)
    }
}

impl AsFd for Request {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl AsRawFd for Request {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// Lists `/dev/media*` nodes, sorted by number.
pub fn list_media_devices() -> Vec<PathBuf> {
    node::list_dev_nodes("media")
}

/// The `/dev/media*` nodes, sorted by number, that open (read-only) and that `select` takes.
pub fn find_media_devices(select: impl Fn(&MediaDevice) -> bool) -> Vec<PathBuf> {
    list_media_devices()
        .into_iter()
        .filter(|p| MediaDevice::open_read_only(p).is_ok_and(|m| select(&m)))
        .collect()
}

/// The media devices whose driver is `driver`, sorted by number.
pub fn find_media_by_driver(driver: &str) -> Vec<PathBuf> {
    find_media_devices(|m| m.device_info().is_ok_and(|i| i.driver == driver))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(
        num_entities: u32,
        num_interfaces: u32,
        num_pads: u32,
        num_links: u32,
    ) -> media_v2_topology {
        let mut raw = media_v2_topology::default();
        raw.num_entities = num_entities;
        raw.num_interfaces = num_interfaces;
        raw.num_pads = num_pads;
        raw.num_links = num_links;
        raw
    }

    #[test]
    fn crafted_counts_never_size_an_allocation() {
        assert_eq!(checked_counts(&counts(3, 2, 6, 4)).unwrap(), [3, 2, 6, 4]);
        let at_bound = MAX_TOPOLOGY_OBJECTS;
        assert_eq!(
            checked_counts(&counts(at_bound, 0, 0, 0)).unwrap()[0],
            at_bound as usize
        );
        for crafted in [
            counts(u32::MAX, 0, 0, 0),
            counts(0, at_bound + 1, 0, 0),
            counts(0, 0, u32::MAX, 0),
            counts(0, 0, 0, u32::MAX),
        ] {
            assert!(matches!(
                checked_counts(&crafted).unwrap_err(),
                Error::Invalid(_)
            ));
        }
    }

    #[test]
    fn pad_indices_must_fit_the_kernel_field() {
        let pad = |index| PadRef { entity: 4, index };
        let desc = pad_desc(pad(65_535)).unwrap();
        assert_eq!({ desc.index }, 65_535);
        assert_eq!({ desc.entity }, 4);
        assert!(matches!(
            pad_desc(pad(65_536)).err().unwrap(),
            Error::Invalid(_)
        ));
    }
}
