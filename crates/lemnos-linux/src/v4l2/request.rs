//! Requests for stateless codecs (the media controller's request API).
//!
//! A stateless device takes each frame's parameters with its buffers as one
//! request: allocate a request on the device's media node, set the controls
//! and queue the output buffer into it, queue the request, and wait for it to
//! complete. A completed request is reinitialised to take the next frame. The
//! session that owns the video node is [`super::m2m::M2mSession`].

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::media;
use lemnos_linux_sys::poll;

/// A request on a media device. Dropping it closes the request.
#[derive(Debug)]
pub struct Request {
    fd: OwnedFd,
}

impl Request {
    /// Allocates a request on the media device behind `media`
    /// (`MEDIA_IOC_REQUEST_ALLOC`; see [`media_device_for`]).
    pub fn allocate(media: impl AsFd) -> io::Result<Self> {
        Ok(Self {
            fd: media::request_alloc(media.as_fd())?,
        })
    }

    /// The request's descriptor, for the session's ext-controls and buffers.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Queues the request to run (`MEDIA_REQUEST_IOC_QUEUE`). The driver refuses
    /// it if a buffer or control in it is not valid for the device.
    pub fn queue(&self) -> io::Result<()> {
        media::request_queue(self.fd.as_fd())
    }

    /// Returns a completed (or never queued) request to its initial state, so it
    /// takes new buffers and controls (`MEDIA_REQUEST_IOC_REINIT`).
    pub fn reinit(&self) -> io::Result<()> {
        media::request_reinit(self.fd.as_fd())
    }

    /// Waits until the request completes or `timeout` passes (`None`: forever).
    /// Returns whether it completed. A completed request reports `POLLPRI`.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        let revents = poll::poll_one(self.fd.as_fd(), poll::POLLPRI, timeout)?;
        Ok(revents & poll::POLLPRI != 0)
    }
}

/// The media device that owns the video node at `video` (its entities include a
/// device node with the same major and minor), or `None` when no `/dev/mediaN`
/// lists it. Media devices are read in numeric order.
pub fn media_device_for(video: impl AsRef<Path>) -> io::Result<Option<PathBuf>> {
    let (major, minor) = split_dev(std::fs::metadata(video.as_ref())?.rdev());
    let mut nodes: Vec<(u32, PathBuf)> = std::fs::read_dir("/dev")?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let index = name.to_str()?.strip_prefix("media")?.parse::<u32>().ok()?;
            Some((index, e.path()))
        })
        .collect();
    nodes.sort_by_key(|(index, _)| *index);
    for (_, path) in nodes {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let Ok(list) = media::entities(file.as_fd()) else {
            continue;
        };
        if list.iter().any(|e| e.dev == Some((major, minor))) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// A device number's major and minor, as glibc splits them.
fn split_dev(dev: u64) -> (u32, u32) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major as u32, minor as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_numbers_split_into_major_and_minor() {
        // makedev(81, 16) as the kernel encodes it.
        assert_eq!(split_dev((81 << 8) | 16), (81, 16));
        // A major of 4096 or more uses the high bits; encoded the way glibc's
        // makedev does it.
        let (major, minor) = (5000u64, 70_000u64);
        let dev = ((major & 0xfff) << 8)
            | ((major & !0xfff) << 32)
            | (minor & 0xff)
            | ((minor & !0xff) << 12);
        assert_eq!(split_dev(dev), (5000, 70_000));
    }

    #[test]
    fn a_request_on_a_non_media_descriptor_is_refused() {
        let file = File::open("/dev/null").unwrap();
        assert!(Request::allocate(&file).is_err());
    }

    #[test]
    fn a_plain_node_has_no_media_device() {
        // /dev/null is not an entity of any media device.
        assert_eq!(media_device_for("/dev/null").unwrap(), None);
    }
}
