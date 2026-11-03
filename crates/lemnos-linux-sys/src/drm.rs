//! DRM render nodes (`/dev/dri/renderD*`): the sync objects a job signals, and
//! their export as sync files. The v3d job calls are in [`v3d`](crate::v3d).
//!
//! A sync object is per file descriptor: a handle means nothing on another
//! open of the node. Closing the descriptor releases its handles.

use crate::ioctl::{ioctl_ptr, iowr};
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// `struct drm_syncobj_create`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct SyncobjCreate {
    handle: u32,
    flags: u32,
}

/// `struct drm_syncobj_destroy`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct SyncobjDestroy {
    handle: u32,
    pad: u32,
}

/// `struct drm_syncobj_handle`: the handle-to-fd and fd-to-handle form.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct SyncobjHandle {
    handle: u32,
    flags: u32,
    fd: i32,
    pad: u32,
    point: u64,
}

/// `DRM_IOCTL_SYNCOBJ_CREATE`.
const SYNCOBJ_CREATE: u32 = iowr::<SyncobjCreate>(b'd', 0xBF);
/// `DRM_IOCTL_SYNCOBJ_DESTROY`.
const SYNCOBJ_DESTROY: u32 = iowr::<SyncobjDestroy>(b'd', 0xC0);
/// `DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD`.
const SYNCOBJ_HANDLE_TO_FD: u32 = iowr::<SyncobjHandle>(b'd', 0xC1);

/// `DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE`: export the fence the
/// object holds now as a sync file (it signals with that fence).
pub const SYNCOBJ_EXPORT_SYNC_FILE: u32 = 1 << 0;

/// Opens a DRM render node read-write and close-on-exec (`/dev/dri/renderD128`).
pub fn open_render_node(path: &Path) -> io::Result<OwnedFd> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)?;
    Ok(file.into())
}

/// Creates an unsignalled binary sync object on the descriptor
/// (`DRM_IOCTL_SYNCOBJ_CREATE`) and returns its handle.
pub fn syncobj_create(fd: BorrowedFd<'_>) -> io::Result<u32> {
    let mut create = SyncobjCreate::default();
    // SAFETY: `SYNCOBJ_CREATE` takes a `drm_syncobj_create`, which `create`
    // is; the kernel writes its `handle` field.
    unsafe { ioctl_ptr(fd, SYNCOBJ_CREATE, &raw mut create)? };
    Ok(create.handle)
}

/// Destroys a sync object on the descriptor (`DRM_IOCTL_SYNCOBJ_DESTROY`).
/// A fence already exported as a sync file stays valid.
pub fn syncobj_destroy(fd: BorrowedFd<'_>, handle: u32) -> io::Result<()> {
    let mut destroy = SyncobjDestroy { handle, pad: 0 };
    // SAFETY: `SYNCOBJ_DESTROY` takes a `drm_syncobj_destroy`, which `destroy`
    // is; the kernel reads it and does not write.
    unsafe { ioctl_ptr(fd, SYNCOBJ_DESTROY, &raw mut destroy)? };
    Ok(())
}

/// Exports the fence a sync object holds as a sync file. The file signals when
/// that fence does: a job that signals the object after the export has not yet
/// finished gives a pending sync file. An object with no fence attached fails
/// with `InvalidInput` (the kernel's `EINVAL`).
pub fn syncobj_export_sync_file(fd: BorrowedFd<'_>, handle: u32) -> io::Result<OwnedFd> {
    let mut request = SyncobjHandle {
        handle,
        flags: SYNCOBJ_EXPORT_SYNC_FILE,
        fd: -1,
        ..SyncobjHandle::default()
    };
    // SAFETY: `SYNCOBJ_HANDLE_TO_FD` takes a `drm_syncobj_handle`, which
    // `request` is; the kernel writes its `fd` field.
    unsafe { ioctl_ptr(fd, SYNCOBJ_HANDLE_TO_FD, &raw mut request)? };
    // SAFETY: a successful export returns a new descriptor that no other
    // owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(request.fd as RawFd) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structs_match_the_kernel_layout() {
        assert_eq!(size_of::<SyncobjCreate>(), 8);
        assert_eq!(size_of::<SyncobjDestroy>(), 8);
        assert_eq!(size_of::<SyncobjHandle>(), 24);
    }

    #[test]
    fn requests_encode_as_the_kernel_numbers() {
        assert_eq!(SYNCOBJ_CREATE, 0xC008_64BF);
        assert_eq!(SYNCOBJ_DESTROY, 0xC008_64C0);
        assert_eq!(SYNCOBJ_HANDLE_TO_FD, 0xC018_64C1);
    }
}
