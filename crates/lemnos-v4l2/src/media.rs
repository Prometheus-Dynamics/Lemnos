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
    request_alloc, request_queue, request_reinit, topology as read_topology, topology_counts,
};
use lemnos_linux_sys::poll::POLLPRI;
use lemnos_linux_sys::v4l2::device::{Pod, from_bytes, zeroed};

use crate::node::{self, cstr_field};
use crate::{Error, Ready, Result};

mod topology;

pub use topology::{
    DataLink, DevNode, Entity, EntityFlags, EntityFunction, Interface, InterfaceType, Link,
    LinkFlags, LinkType, Pad, PadFlags, PadRef, Topology,
};

/// Build a topology from what `MEDIA_IOC_G_TOPOLOGY` would fill in, from `bytes` (four
/// counts, then the arrays), and resolve every link. For fuzzing.
#[doc(hidden)]
pub fn fuzz_topology(bytes: &[u8]) {
    fn take<T: Pod>(bytes: &mut &[u8], count: u8) -> Vec<T> {
        (0..count)
            .map(|_| {
                let n = bytes.len().min(size_of::<T>());
                let value = from_bytes::<T>(&bytes[..n]);
                *bytes = &bytes[n..];
                value
            })
            .collect()
    }
    let Some((counts, mut rest)) = bytes.split_first_chunk::<4>() else {
        return;
    };
    let entities = take::<media_v2_entity>(&mut rest, counts[0] % 32);
    let interfaces = take::<media_v2_interface>(&mut rest, counts[1] % 32);
    let pads = take::<media_v2_pad>(&mut rest, counts[2] % 64);
    let links = take::<media_v2_link>(&mut rest, counts[3] % 64);
    let topology = Topology::from_raw(7, &entities, &interfaces, &pads, &links);
    let _ = topology.data_links();
    for e in &topology.entities {
        let _ = topology.entity_by_name(&e.name);
        let _ = (topology.pads_of(e.id), topology.links_from(e.id));
        let _ = (topology.links_to(e.id), topology.interface_of(e.id));
        let _ = (e.function.name(), e.flags);
    }
    for i in &topology.interfaces {
        let _ = (topology.entity_of_interface(i.id), i.intf_type.name());
    }
    for l in &topology.links {
        let _ = l.flags.link_type();
    }
}

/// Information about a media device (`MEDIA_IOC_DEVICE_INFO`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Driver name, e.g. `rp1-cfe`, `uvcvideo`.
    pub driver: String,
    /// Model, e.g. `rp1-cfe`, `UVC Camera (046d:0825)`.
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
            let mut entities = vec![zeroed::<media_v2_entity>(); counts.num_entities as usize];
            let mut interfaces =
                vec![zeroed::<media_v2_interface>(); counts.num_interfaces as usize];
            let mut pads = vec![zeroed::<media_v2_pad>(); counts.num_pads as usize];
            let mut links = vec![zeroed::<media_v2_link>(); counts.num_links as usize];
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
    /// usually [`LinkFlags::ENABLED`] or empty; immutable links cannot be changed.
    pub fn setup_link(&self, source: PadRef, sink: PadRef, flags: LinkFlags) -> Result<()> {
        let pad = |p: PadRef| media_pad_desc {
            entity: p.entity,
            index: p.index as u16,
            ..Default::default()
        };
        let mut desc = media_link_desc {
            source: pad(source),
            sink: pad(sink),
            flags: flags.0,
            ..Default::default()
        };
        node::ioctl(self.as_fd(), MEDIA_IOC_SETUP_LINK, &mut desc)
    }

    /// The process holding a record lock on the device node (`fcntl(F_GETLK)` for a write
    /// lock over the whole file), as libcamera takes one (`lockf`) while one of the device's
    /// cameras is acquired. Takes no lock itself; this process's own locks are not reported.
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

/// The media devices whose driver is `driver` (e.g. `rp1-cfe`, `pispbe`), sorted by number.
pub fn find_media_by_driver(driver: &str) -> Vec<PathBuf> {
    find_media_devices(|m| m.device_info().is_ok_and(|i| i.driver == driver))
}
