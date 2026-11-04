//! Shared memory mappings of descriptors (`mmap`/`munmap`): dma-buf and V4L2
//! buffer memory. A [`Mapping`] unmaps itself on drop.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr::NonNull;

/// A shared mapping of a descriptor's bytes, unmapped on drop.
#[derive(Debug)]
pub struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is memory this value owns until it is dropped; nothing
// in it is tied to the thread that created it.
unsafe impl Send for Mapping {}

// SAFETY: `&Mapping` only ever yields `&[u8]`. A device may write the memory
// while the CPU reads it; the caller brackets that with the dma-buf sync or
// the V4L2 buffer ownership rules, as for any shared mapping.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Maps `len` bytes of `fd` from `offset` (page-aligned, as `mmap` needs),
    /// shared so that a device sees CPU writes, read-only unless `write` is set.
    pub fn new(fd: BorrowedFd<'_>, len: usize, offset: u64, write: bool) -> io::Result<Self> {
        if len == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let prot = libc::PROT_READ | if write { libc::PROT_WRITE } else { 0 };
        // SAFETY: a null hint lets the kernel choose the address; `fd` is open
        // for the duration of the call; `len` is non-zero. The result is
        // compared with MAP_FAILED before use.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr = NonNull::new(raw.cast::<u8>())
            .ok_or_else(|| io::Error::other("mmap returned a null address"))?;
        Ok(Self { ptr, len })
    }

    /// Bytes mapped.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping is empty (never, for a mapping made by [`Self::new`]).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The start address.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }

    /// The mapped bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` maps `len` readable bytes for as long as `self` lives,
        // and `&self` keeps it from being unmapped.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// The mapped bytes, for writing. Only a mapping made read-write may be
    /// written; a read-only one faults.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as for `as_slice`, and `&mut self` makes the slice unique.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the range was mapped by `new` and is unmapped only here. An
        // error here cannot be reported from drop and leaves nothing to undo.
        let _ = unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn maps_a_file_shared_and_unmaps_it() {
        let path = std::env::temp_dir().join(format!("lemnos-mmap-test-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(8192).unwrap();
        {
            let mut map = Mapping::new(file.as_fd(), 8192, 0, true).unwrap();
            assert_eq!(map.len(), 8192);
            map.as_mut_slice()[4096] = 0x5a;
        }
        {
            // A second, read-only view sees the write through the file.
            let map = Mapping::new(file.as_fd(), 4096, 4096, false).unwrap();
            assert_eq!(map.as_slice()[0], 0x5a);
        }
        assert!(Mapping::new(file.as_fd(), 0, 0, false).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
