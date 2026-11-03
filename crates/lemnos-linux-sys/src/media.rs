//! The media controller's entities and request API (`linux/media.h`).
//!
//! A stateless codec takes its parameters through requests: a request is
//! allocated on the media device, carries a buffer and controls, and is queued
//! for the device to run once. The request is a file descriptor of its own.

use std::io;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd, RawFd};

use crate::ioctl::{ioc, ioctl_int, ioctl_ptr, ior, iowr};

/// `MEDIA_ENT_ID_FLAG_NEXT`: asks for the entity after the one named.
const ENT_ID_FLAG_NEXT: u32 = 1 << 31;

/// `MEDIA_IOC_ENUM_ENTITIES`.
const ENUM_ENTITIES: u32 = iowr::<EntityDesc>(b'|', 0x01);
/// `MEDIA_IOC_REQUEST_ALLOC`: writes the new request's descriptor to the argument.
const REQUEST_ALLOC: u32 = ior::<i32>(b'|', 0x05);
/// `MEDIA_REQUEST_IOC_QUEUE`.
const REQUEST_QUEUE: u32 = ioc(0, b'|', 0x80, 0);
/// `MEDIA_REQUEST_IOC_REINIT`.
const REQUEST_REINIT: u32 = ioc(0, b'|', 0x81, 0);

/// `struct media_entity_desc` (256 bytes). Only the device numbers of a
/// device node are read; the rest is kept for layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct EntityDesc {
    id: u32,
    name: [u8; 32],
    kind: u32,
    revision: u32,
    flags: u32,
    group_id: u32,
    pads: u16,
    links: u16,
    reserved: [u32; 4],
    dev_major: u32,
    dev_minor: u32,
    union_tail: [u8; 176],
}

impl EntityDesc {
    fn zeroed() -> Self {
        // SAFETY: every field is an integer or a byte array, so all-zero is valid.
        unsafe { std::mem::zeroed() }
    }
}

/// An entity of a media device, as the device lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    /// The entity's id on this device.
    pub id: u32,
    /// The entity's name, e.g. `"rpi-hevc-dec"`.
    pub name: String,
    /// The entity's `MEDIA_ENT_F_*` function.
    pub function: u32,
    /// For a device node, the device number's major and minor; `None` otherwise.
    pub dev: Option<(u32, u32)>,
}

/// Lists the entities of the media device behind `fd` (`MEDIA_IOC_ENUM_ENTITIES`
/// with the next-entity flag, until the device says there are no more).
pub fn entities(fd: BorrowedFd<'_>) -> io::Result<Vec<Entity>> {
    let mut out = Vec::new();
    let mut next = ENT_ID_FLAG_NEXT;
    // A device has far fewer entities than this; the bound stops a device that
    // never reports the end.
    for _ in 0..4096 {
        let mut desc = EntityDesc::zeroed();
        desc.id = next;
        // SAFETY: `ENUM_ENTITIES` takes a `media_entity_desc`, which `desc` is.
        match unsafe { ioctl_ptr(fd, ENUM_ENTITIES, &raw mut desc) } {
            Ok(_) => {}
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => return Ok(out),
            Err(e) => return Err(e),
        }
        let end = desc
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(desc.name.len());
        out.push(Entity {
            id: desc.id,
            name: String::from_utf8_lossy(&desc.name[..end]).into_owned(),
            function: desc.kind,
            dev: (desc.dev_major != 0 || desc.dev_minor != 0)
                .then_some((desc.dev_major, desc.dev_minor)),
        });
        next = desc.id | ENT_ID_FLAG_NEXT;
    }
    Err(io::Error::other(
        "the media device lists more entities than expected",
    ))
}

/// Allocates a request on the media device behind `fd` (`MEDIA_IOC_REQUEST_ALLOC`).
/// The request is closed by dropping the returned descriptor.
pub fn request_alloc(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    let mut raw: i32 = -1;
    // SAFETY: `REQUEST_ALLOC` writes one `int` through the pointer.
    unsafe { ioctl_ptr(fd, REQUEST_ALLOC, &raw mut raw)? };
    if raw < 0 {
        return Err(io::Error::other("the media device returned no request"));
    }
    // SAFETY: a successful allocation returns a new descriptor that no other owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(raw as RawFd) })
}

/// Queues the request for the device to run (`MEDIA_REQUEST_IOC_QUEUE`).
pub fn request_queue(fd: BorrowedFd<'_>) -> io::Result<()> {
    ioctl_int(fd, REQUEST_QUEUE, 0).map(|_| ())
}

/// Returns a completed request to its initial state, so it can take new buffers
/// and controls (`MEDIA_REQUEST_IOC_REINIT`).
pub fn request_reinit(fd: BorrowedFd<'_>) -> io::Result<()> {
    ioctl_int(fd, REQUEST_REINIT, 0).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entity_descriptor_has_the_kernel_layout() {
        assert_eq!(std::mem::size_of::<EntityDesc>(), 256);
    }

    #[test]
    fn request_numbers_match_the_kernel_header() {
        // _IOWR('|', 0x01, 256), _IOR('|', 0x05, int), _IO('|', 0x80), _IO('|', 0x81).
        assert_eq!(ENUM_ENTITIES, 0xc100_7c01);
        assert_eq!(REQUEST_ALLOC, 0x8004_7c05);
        assert_eq!(REQUEST_QUEUE, 0x0000_7c80);
        assert_eq!(REQUEST_REINIT, 0x0000_7c81);
    }

    #[test]
    fn a_non_media_descriptor_is_refused_without_panicking() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsFd::as_fd(&file);
        assert!(request_alloc(fd).is_err());
        assert!(request_queue(fd).is_err());
        assert!(request_reinit(fd).is_err());
        assert!(entities(fd).is_err());
    }
}
