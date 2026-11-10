//! dma-buf descriptors: CPU-access sync, names, and sync-file fences.

use crate::ioctl::{ioctl_ptr, iow, iowr};
use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// `DMA_BUF_SYNC_READ`: the CPU reads the mapping.
pub const SYNC_READ: u64 = 1;
/// `DMA_BUF_SYNC_WRITE`: the CPU writes the mapping.
pub const SYNC_WRITE: u64 = 2;
/// `DMA_BUF_SYNC_START`: the start of a CPU access.
pub const SYNC_START: u64 = 0;
/// `DMA_BUF_SYNC_END`: the end of a CPU access.
pub const SYNC_END: u64 = 1 << 2;

/// `struct dma_buf_sync`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Sync {
    flags: u64,
}

/// `struct dma_buf_export_sync_file`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct ExportSyncFile {
    flags: u32,
    fd: i32,
}

/// `struct dma_buf_import_sync_file`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct ImportSyncFile {
    flags: u32,
    fd: i32,
}

/// `DMA_BUF_IOCTL_SYNC`.
const SYNC_REQUEST: u32 = iow::<Sync>(b'b', 0);
/// `DMA_BUF_SET_NAME_B`: the name's address, as a 64-bit value.
const SET_NAME: u32 = iow::<u64>(b'b', 1);
/// `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`.
const EXPORT_SYNC_FILE: u32 = iowr::<ExportSyncFile>(b'b', 2);
/// `DMA_BUF_IOCTL_IMPORT_SYNC_FILE`.
const IMPORT_SYNC_FILE: u32 = iow::<ImportSyncFile>(b'b', 3);

/// Brackets CPU access to a dma-buf (`DMA_BUF_IOCTL_SYNC`): `flags` is
/// [`SYNC_START`] or [`SYNC_END`] ored with [`SYNC_READ`] and/or
/// [`SYNC_WRITE`].
pub fn sync(dmabuf: BorrowedFd<'_>, flags: u64) -> io::Result<()> {
    let mut request = Sync { flags };
    // SAFETY: `SYNC_REQUEST` takes a `dma_buf_sync`, which `request` is.
    unsafe { ioctl_ptr(dmabuf, SYNC_REQUEST, &raw mut request)? };
    Ok(())
}

/// Names a dma-buf for debugging (`DMA_BUF_SET_NAME`; the kernel keeps at most
/// 31 bytes of it).
pub fn set_name(dmabuf: BorrowedFd<'_>, name: &CStr) -> io::Result<()> {
    // SAFETY: `SET_NAME` takes the address of a NUL-terminated string as a
    // 64-bit value. The kernel copies the string from it and does not write.
    unsafe { ioctl_ptr(dmabuf, SET_NAME, name.as_ptr().cast_mut().cast::<u8>())? };
    Ok(())
}

/// Exports a sync file that signals when the dma-buf's fences for `flags`
/// (`SYNC_READ`: its writers; with `SYNC_WRITE` too or alone: all users) have
/// signalled. Kernels without sync-file export report
/// [`io::ErrorKind::Unsupported`].
pub fn export_sync_file(dmabuf: BorrowedFd<'_>, flags: u32) -> io::Result<OwnedFd> {
    let mut export = ExportSyncFile { flags, fd: -1 };
    // SAFETY: `EXPORT_SYNC_FILE` takes a `dma_buf_export_sync_file`, which
    // `export` is; the kernel writes its `fd` field.
    unsafe { ioctl_ptr(dmabuf, EXPORT_SYNC_FILE, &raw mut export) }.map_err(unsupported)?;
    // SAFETY: a successful export returns a new sync-file descriptor, which
    // no other owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(export.fd as RawFd) })
}

/// Attaches a sync file as a fence on the dma-buf: with `SYNC_READ` alone it
/// is a read fence (later writers wait on it), with `SYNC_WRITE` a write fence
/// (later users of any kind wait on it). The caller keeps the sync file.
pub fn import_sync_file(
    dmabuf: BorrowedFd<'_>,
    flags: u32,
    sync_file: BorrowedFd<'_>,
) -> io::Result<()> {
    let mut import = ImportSyncFile {
        flags,
        fd: sync_file.as_raw_fd(),
    };
    // SAFETY: `IMPORT_SYNC_FILE` takes a `dma_buf_import_sync_file`, which
    // `import` is; the kernel reads it and does not keep the descriptor.
    unsafe { ioctl_ptr(dmabuf, IMPORT_SYNC_FILE, &raw mut import) }.map_err(unsupported)?;
    Ok(())
}

/// A new `memfd` called `name`, close-on-exec and empty: plain memory that is
/// shared and mapped the way a dma-buf is, for tests and software producers.
pub fn memfd(name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: `name` is a valid NUL-terminated string for the call.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `memfd_create` returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A new sealed `memfd` of `len` bytes, close-on-exec: the size can't be changed by anyone holding
/// it, including whoever receives the descriptor, so a mapping of it can never fault past the end.
/// The contents stay writable.
pub fn sealed_memfd(name: &CStr, len: u64) -> io::Result<OwnedFd> {
    let flags = libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING;
    // SAFETY: `name` is a valid NUL-terminated string for the call.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `memfd_create` returned a new descriptor that nothing else owns.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let file = std::fs::File::from(owned);
    file.set_len(len)?;
    let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    // SAFETY: `F_ADD_SEALS` takes an integer of seals, and `file` is open for the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedFd::from(file))
}

/// A dma-buf's size, from the kernel. The descriptor's file offset is left
/// where it was: the offset is read, the end found with `lseek`, and the old
/// offset restored. No duplicate is made, since a duplicate shares the offset.
///
/// A dma-buf answers only `lseek(SEEK_END, 0)` and `lseek(SEEK_SET, 0)`
/// (`SEEK_CUR` is `EINVAL`), so for one the offset cannot be read: a fresh
/// descriptor's offset is 0, and that is what is restored.
pub fn size(dmabuf: BorrowedFd<'_>) -> io::Result<u64> {
    let fd = dmabuf.as_raw_fd();
    // SAFETY: `lseek` on a descriptor the caller holds only moves its offset.
    let offset = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
    if offset < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error);
        }
        // SAFETY: as above; the dma-buf form of the size query.
        let end = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
        if end < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above.
        if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(end as u64);
    }
    // SAFETY: as above.
    let end = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
    let end_error = (end < 0).then(io::Error::last_os_error);
    // SAFETY: as above; `offset` came from this descriptor.
    let restored = unsafe { libc::lseek(fd, offset, libc::SEEK_SET) };
    if let Some(error) = end_error {
        return Err(error);
    }
    if restored < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(end as u64)
}

/// The filesystem magic of a dma-buf's file (`DMA_BUF_MAGIC` in `linux/magic.h`, "DMAB").
const DMA_BUF_MAGIC: u64 = 0x444d_4142;

/// Whether `fd` is a dma-buf, by the filesystem its file is on (`fstatfs`). A dma-buf's size is
/// fixed when it is made, so nobody holding it can make a mapping of it fault past the end.
/// Anything else, a memfd included, answers `false`.
pub fn is_dma_buf(fd: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: an all-zero `statfs` is a valid value; `fstatfs` fills it in.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `fstatfs` writes one `statfs` through the pointer, which is live.
    if unsafe { libc::fstatfs(fd.as_raw_fd(), &raw mut st) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // `f_type` is signed or unsigned, 32 or 64 bits wide, by platform and C library.
    #[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
    let magic = st.f_type as u64;
    Ok(magic == DMA_BUF_MAGIC)
}

/// The buffer's identity, its device and inode numbers (`fstat`): the same for
/// every descriptor of one dma-buf. No duplicate is made.
pub fn identity(dmabuf: BorrowedFd<'_>) -> io::Result<(u64, u64)> {
    // SAFETY: an all-zero `stat` is a valid value; `fstat` fills it in.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fstat` writes one `stat` through the pointer, which is live.
    if unsafe { libc::fstat(dmabuf.as_raw_fd(), &raw mut st) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((st.st_dev as u64, st.st_ino as u64))
}

/// ENOTTY from the sync-file ioctls means the kernel lacks them.
fn unsupported(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(libc::ENOTTY) {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "kernel lacks dma-buf sync files",
        )
    } else {
        error
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn layouts_and_requests_match_the_kernel_header() {
        assert_eq!(std::mem::size_of::<Sync>(), 8);
        assert_eq!(std::mem::size_of::<ExportSyncFile>(), 8);
        assert_eq!(std::mem::size_of::<ImportSyncFile>(), 8);
        assert_eq!(SYNC_REQUEST, 0x4008_6200); // DMA_BUF_IOCTL_SYNC
        assert_eq!(SET_NAME, 0x4008_6201); // DMA_BUF_SET_NAME_B
        assert_eq!(EXPORT_SYNC_FILE, 0xc008_6202);
        assert_eq!(IMPORT_SYNC_FILE, 0x4008_6203);
        assert_eq!(SYNC_START | SYNC_READ, 1);
        assert_eq!(SYNC_END | SYNC_WRITE, 6);
    }

    #[test]
    fn a_memfd_is_not_a_dma_buf() {
        use std::os::fd::AsFd;
        let memory = sealed_memfd(c"not-a-dma-buf", 4096).unwrap();
        assert!(!is_dma_buf(memory.as_fd()).unwrap());
    }

    /// Only with `LEMNOS_TEST_DMA_HEAP` naming a heap this process may open.
    #[test]
    fn a_heap_allocation_is_a_dma_buf_when_a_heap_is_named() {
        use std::os::fd::AsFd;
        let Some(heap) = std::env::var_os("LEMNOS_TEST_DMA_HEAP") else {
            return;
        };
        let heap = std::fs::File::open(heap).unwrap();
        let buffer = crate::dma_heap::allocate(heap.as_fd(), 4096).unwrap();
        assert!(is_dma_buf(buffer.as_fd()).unwrap());
        assert!(size(buffer.as_fd()).unwrap() >= 4096);
    }
}
