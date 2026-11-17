//! dma-heaps (`/dev/dma_heap/<name>`): allocating dma-bufs.

use crate::ioctl::{ioctl_ptr, iowr};
use std::io;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// `struct dma_heap_allocation_data`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct AllocationData {
    len: u64,
    fd: u32,
    fd_flags: u32,
    heap_flags: u64,
}

/// `DMA_HEAP_IOCTL_ALLOC`.
const ALLOC: u32 = iowr::<AllocationData>(b'H', 0);

/// Allocates a dma-buf of `len` bytes from the heap open as `heap`, with
/// read-write and close-on-exec descriptor flags and no heap flags. The kernel
/// rounds the size up to whole pages.
pub fn allocate(heap: BorrowedFd<'_>, len: u64) -> io::Result<OwnedFd> {
    allocate_with(heap, len, false)
}

/// As [`allocate`], but with `read_only` the descriptor is a read-only file: the kernel gives the
/// buffer no writable mapping through it (`EACCES`), and nothing else can reopen the buffer
/// read-write (`/proc/self/fd` reopen fails for a dma-buf). The mode is fixed here: a dma-buf
/// cannot be made writable later. Hardware that fills the buffer doesn't need a writable
/// descriptor of it.
pub fn allocate_with(heap: BorrowedFd<'_>, len: u64, read_only: bool) -> io::Result<OwnedFd> {
    let access = if read_only {
        libc::O_RDONLY
    } else {
        libc::O_RDWR
    };
    let mut data = AllocationData {
        len,
        fd_flags: (access | libc::O_CLOEXEC) as u32,
        ..Default::default()
    };
    // SAFETY: `ALLOC` takes a `dma_heap_allocation_data`, which `data` is. The
    // kernel writes only the `fd` field of it.
    unsafe { ioctl_ptr(heap, ALLOC, &raw mut data)? };
    // SAFETY: a successful allocation returns a new dma-buf descriptor, which
    // no other owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(data.fd as RawFd) })
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    #[test]
    fn layout_and_request_match_the_kernel_header() {
        assert_eq!(std::mem::size_of::<AllocationData>(), 24);
        assert_eq!(std::mem::offset_of!(AllocationData, fd), 8);
        assert_eq!(std::mem::offset_of!(AllocationData, heap_flags), 16);
        assert_eq!(ALLOC, 0xc018_4800); // DMA_HEAP_IOCTL_ALLOC
    }
}
