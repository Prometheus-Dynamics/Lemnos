//! The media controller's entities and request API (`linux/media.h`).
//!
//! A stateless codec takes its parameters through requests: a request is
//! allocated on the media device, carries a buffer and controls, and is queued
//! for the device to run once. The request is a file descriptor of its own.
//!
//! The structures' field names are the kernel's, as in [`crate::v4l2`].
//!
//! Provenance: the Linux uAPI headers are GPL-2.0 WITH Linux-syscall-note. The names,
//! numbers and layouts below are interface facts transcribed from them; no kernel
//! source is copied.

use std::io;
use std::mem::offset_of;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

#[cfg(test)]
use crate::ioctl::zeroed;
use crate::ioctl::{ioc, ioctl_int, ioctl_ptr, ioctls, ior, iowr, pod};

pub use uapi::*;

/// The `linux/media.h` structures.
#[allow(non_camel_case_types, missing_docs)]
mod uapi {
    /// `struct media_device_info`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct media_device_info {
        pub driver: [u8; 16],
        pub model: [u8; 32],
        pub serial: [u8; 40],
        pub bus_info: [u8; 32],
        pub media_version: u32,
        pub hw_revision: u32,
        pub driver_version: u32,
        pub reserved: [u32; 31],
    }

    /// `struct media_v2_entity`.
    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    pub struct media_v2_entity {
        pub id: u32,
        pub name: [u8; 64],
        pub function: u32,
        pub flags: u32,
        pub reserved: [u32; 5],
    }

    /// `struct media_v2_interface`; for device nodes `raw[0..2]` is the devnode major/minor.
    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    pub struct media_v2_interface {
        pub id: u32,
        pub intf_type: u32,
        pub flags: u32,
        pub reserved: [u32; 9],
        pub raw: [u32; 16],
    }

    /// `struct media_v2_pad`.
    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    pub struct media_v2_pad {
        pub id: u32,
        pub entity_id: u32,
        pub flags: u32,
        pub index: u32,
        pub reserved: [u32; 4],
    }

    /// `struct media_v2_link`.
    #[repr(C, packed)]
    #[derive(Clone, Copy)]
    pub struct media_v2_link {
        pub id: u32,
        pub source_id: u32,
        pub sink_id: u32,
        pub flags: u32,
        pub reserved: [u32; 6],
    }

    /// `struct media_v2_topology`. The `ptr_*` fields are private: the kernel follows
    /// them, so only [`super::topology`] sets them, from borrowed slices.
    #[repr(C, packed)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct media_v2_topology {
        pub topology_version: u64,
        pub num_entities: u32,
        pub reserved1: u32,
        pub(super) ptr_entities: u64,
        pub num_interfaces: u32,
        pub reserved2: u32,
        pub(super) ptr_interfaces: u64,
        pub num_pads: u32,
        pub reserved3: u32,
        pub(super) ptr_pads: u64,
        pub num_links: u32,
        pub reserved4: u32,
        pub(super) ptr_links: u64,
    }

    /// `struct media_pad_desc`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct media_pad_desc {
        pub entity: u32,
        pub index: u16,
        pub flags: u32,
        pub reserved: [u32; 2],
    }

    /// `struct media_link_desc`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct media_link_desc {
        pub source: media_pad_desc,
        pub sink: media_pad_desc,
        pub flags: u32,
        pub reserved: [u32; 2],
    }
}

// `media_v2_topology` holds pointers the kernel follows, so it is left out: only
// [`topology`] fills it.
pod!(
    media_device_info,
    media_v2_entity,
    media_v2_interface,
    media_v2_pad,
    media_v2_link,
    media_pad_desc,
    media_link_desc,
);

ioctls! {
    MEDIA_IOC_DEVICE_INFO: media_device_info = iowr(b'|', 0x00);
    MEDIA_IOC_SETUP_LINK: media_link_desc = iowr(b'|', 0x03);
}

/// `MEDIA_IOC_G_TOPOLOGY`: `struct media_v2_topology` holds array pointers, so it
/// has [`topology_counts`] and [`topology`] instead of a typed request.
const G_TOPOLOGY: u32 = iowr::<media_v2_topology>(b'|', 0x04);

const _: () = {
    assert!(size_of::<media_device_info>() == 256);
    assert!(size_of::<media_v2_entity>() == 96);
    assert!(size_of::<media_v2_interface>() == 112);
    assert!(size_of::<media_v2_pad>() == 32);
    assert!(size_of::<media_v2_link>() == 40);
    assert!(size_of::<media_v2_topology>() == 72);
    assert!(offset_of!(media_v2_topology, ptr_links) == 64);
    assert!(size_of::<media_pad_desc>() == 20);
    assert!(size_of::<media_link_desc>() == 52);
    assert!(offset_of!(media_link_desc, sink) == 20);
    assert!(offset_of!(media_link_desc, flags) == 40);
};

/// The graph's element counts and version (`MEDIA_IOC_G_TOPOLOGY` with no arrays).
pub fn topology_counts(fd: BorrowedFd<'_>) -> io::Result<media_v2_topology> {
    let mut raw = media_v2_topology::default();
    // SAFETY: the request takes a `media_v2_topology`, which `raw` is; with null
    // array pointers the kernel only fills in the counts.
    unsafe { ioctl_ptr(fd, G_TOPOLOGY, &raw mut raw)? };
    Ok(raw)
}

/// Reads the graph into the four slices (`MEDIA_IOC_G_TOPOLOGY`) and returns the
/// counts and version it was read at, the counts checked against the slices. `ENOSPC`
/// when the graph grew past a slice.
pub fn topology(
    fd: BorrowedFd<'_>,
    entities: &mut [media_v2_entity],
    interfaces: &mut [media_v2_interface],
    pads: &mut [media_v2_pad],
    links: &mut [media_v2_link],
) -> io::Result<media_v2_topology> {
    let lens = [entities.len(), interfaces.len(), pads.len(), links.len()];
    let mut raw = media_v2_topology {
        num_entities: count_of(lens[0])?,
        ptr_entities: address_of(entities),
        num_interfaces: count_of(lens[1])?,
        ptr_interfaces: address_of(interfaces),
        num_pads: count_of(lens[2])?,
        ptr_pads: address_of(pads),
        num_links: count_of(lens[3])?,
        ptr_links: address_of(links),
        ..Default::default()
    };
    // SAFETY: the request takes a `media_v2_topology`, which `raw` is. Each pointer
    // is null or points at a slice with room for the matching count, alive for the call.
    unsafe { ioctl_ptr(fd, G_TOPOLOGY, &raw mut raw)? };
    check_counts(&raw, lens)?;
    // The caller has its slices back; keep no pointers to them.
    raw.ptr_entities = 0;
    raw.ptr_interfaces = 0;
    raw.ptr_pads = 0;
    raw.ptr_links = 0;
    Ok(raw)
}

fn count_of(len: usize) -> io::Result<u32> {
    u32::try_from(len).map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))
}

/// A slice's address for the kernel, null when empty so nothing can be read through it.
fn address_of<T>(slice: &mut [T]) -> u64 {
    if slice.is_empty() {
        0
    } else {
        slice.as_mut_ptr() as u64
    }
}

/// Refuses a topology whose counts run past the slices it was read into: the graph grew,
/// or the numbers are not to be believed (`ENOSPC`, as when it grew between the calls).
fn check_counts(raw: &media_v2_topology, lens: [usize; 4]) -> io::Result<()> {
    let counts = [
        raw.num_entities,
        raw.num_interfaces,
        raw.num_pads,
        raw.num_links,
    ];
    let fits = counts
        .iter()
        .zip(lens)
        .all(|(&count, len)| usize::try_from(count).is_ok_and(|n| n <= len));
    if fits {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(libc::ENOSPC))
    }
}

/// The process holding a record lock on `fd`, by `fcntl(F_GETLK)` for a write lock
/// over the whole file. Takes no lock itself; this process's own locks are not
/// reported.
pub fn lock_holder(fd: BorrowedFd<'_>) -> io::Result<Option<i32>> {
    // SAFETY: `flock` is plain data, and all zeros is a valid value.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as libc::c_short;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    // SAFETY: `F_GETLK` takes a pointer to a `flock`, alive for the call.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETLK, &raw mut lock) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((i32::from(lock.l_type) != libc::F_UNLCK).then_some(lock.l_pid))
}

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
    /// The entity's name.
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
        out.push(entity_of(&desc));
        next = desc.id | ENT_ID_FLAG_NEXT;
    }
    Err(io::Error::other(
        "the media device lists more entities than expected",
    ))
}

/// Decodes a kernel entity descriptor. The name stops at the field's end even when the
/// kernel left no NUL in it.
fn entity_of(desc: &EntityDesc) -> Entity {
    let end = desc
        .name
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(desc.name.len());
    Entity {
        id: desc.id,
        name: String::from_utf8_lossy(&desc.name[..end]).into_owned(),
        function: desc.kind,
        dev: (desc.dev_major != 0 || desc.dev_minor != 0)
            .then_some((desc.dev_major, desc.dev_minor)),
    }
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
    fn device_requests_match_the_kernel_header() {
        assert_eq!(MEDIA_IOC_DEVICE_INFO.nr(), 0xc100_7c00);
        assert_eq!(MEDIA_IOC_SETUP_LINK.nr(), 0xc034_7c03);
        assert_eq!(G_TOPOLOGY, 0xc048_7c04);
    }

    #[test]
    fn a_plain_file_has_no_lock_holder_and_no_topology() {
        let file = crate::v4l2::test_file();
        let fd = std::os::fd::AsFd::as_fd(&file);
        let no_tty = |e: io::Error| e.raw_os_error() == Some(libc::ENOTTY);
        assert!(no_tty(topology_counts(fd).unwrap_err()));
        assert!(no_tty(
            topology(fd, &mut [], &mut [], &mut [], &mut []).unwrap_err()
        ));
        assert_eq!(lock_holder(fd).unwrap(), None);
    }

    #[test]
    fn a_non_media_descriptor_is_refused_without_panicking() {
        let file = crate::v4l2::test_file();
        let fd = std::os::fd::AsFd::as_fd(&file);
        let no_tty = |e: io::Error| e.raw_os_error() == Some(libc::ENOTTY);
        assert!(no_tty(request_alloc(fd).unwrap_err()));
        assert!(no_tty(request_queue(fd).unwrap_err()));
        assert!(no_tty(request_reinit(fd).unwrap_err()));
        assert!(no_tty(entities(fd).unwrap_err()));
    }

    #[test]
    fn empty_slices_give_the_kernel_no_address() {
        let mut none: [media_v2_entity; 0] = [];
        assert_eq!(address_of(&mut none), 0);
        let mut one = [zeroed::<media_v2_pad>(); 1];
        assert_eq!(address_of(&mut one), one.as_mut_ptr() as u64);
        assert_eq!(count_of(2).unwrap(), 2);
    }

    #[test]
    fn crafted_topology_counts_are_refused_past_the_slices() {
        let ok = media_v2_topology {
            num_entities: 2,
            num_interfaces: 1,
            num_pads: 4,
            num_links: 3,
            ..Default::default()
        };
        let lens = [2, 1, 4, 3];
        assert!(check_counts(&ok, lens).is_ok());
        assert!(check_counts(&media_v2_topology::default(), lens).is_ok());
        for grown in [
            media_v2_topology {
                num_entities: 3,
                ..ok
            },
            media_v2_topology {
                num_interfaces: 2,
                ..ok
            },
            media_v2_topology { num_pads: 5, ..ok },
            media_v2_topology {
                num_links: u32::MAX,
                ..ok
            },
        ] {
            let err = check_counts(&grown, lens).unwrap_err();
            assert_eq!(err.raw_os_error(), Some(libc::ENOSPC));
        }
    }

    #[test]
    fn a_full_entity_name_with_no_nul_stops_at_the_field() {
        let mut desc: EntityDesc = EntityDesc::zeroed();
        desc.name = [b'x'; 32];
        desc.id = 7;
        desc.kind = 0x4009;
        desc.dev_major = 81;
        let entity = entity_of(&desc);
        assert_eq!(entity.name.len(), 32);
        assert_eq!(entity.id, 7);
        assert_eq!(entity.function, 0x4009);
        assert_eq!(entity.dev, Some((81, 0)));
        desc.dev_major = 0;
        assert_eq!(entity_of(&desc).dev, None);
    }
}
