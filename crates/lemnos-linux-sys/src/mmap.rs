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
    writable: bool,
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
        Ok(Self {
            ptr,
            len,
            writable: write,
        })
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
    /// Reads `N` bytes at `offset` with one volatile load, for memory the device writes at any
    /// time. `None` when the bytes do not all lie inside the mapping.
    pub fn read_volatile<const N: usize>(&self, offset: usize) -> Option<[u8; N]> {
        let end = offset.checked_add(N)?;
        if end > self.len {
            return None;
        }
        let at = self.ptr.as_ptr().wrapping_add(offset).cast::<[u8; N]>();
        // SAFETY: `at` starts `offset` bytes into the mapping and `end <= len` keeps the `N`
        // bytes inside the `len` that `new` mapped; `[u8; N]` needs no alignment; the mapping
        // stays mapped while `&self` lives.
        Some(unsafe { at.read_volatile() })
    }

    /// Writes `value` at `offset` with one volatile store. `None`, and nothing written, when
    /// the bytes do not all lie inside the mapping or the mapping is read-only.
    pub fn write_volatile<const N: usize>(&mut self, offset: usize, value: [u8; N]) -> Option<()> {
        let end = offset.checked_add(N)?;
        if end > self.len || !self.writable {
            return None;
        }
        let at = self.ptr.as_ptr().wrapping_add(offset).cast::<[u8; N]>();
        // SAFETY: as for `read_volatile`; the store also needs a writable mapping, which the
        // check above ruled in, and `&mut self` makes the store exclusive.
        unsafe { at.write_volatile(value) };
        Some(())
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
    fn volatile_accessors_are_bounds_checked_and_respect_writability() {
        let path =
            std::env::temp_dir().join(format!("lemnos-mmap-volatile-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(4096).unwrap();
        {
            let mut map = Mapping::new(file.as_fd(), 4096, 0, true).unwrap();
            assert_eq!(map.write_volatile(4080, [7; 16]), Some(()));
            assert_eq!(map.read_volatile::<16>(4080), Some([7; 16]));
            assert_eq!(map.as_slice()[4095], 7);
            // Past the end, and at an offset that overflows: refused, nothing written.
            assert_eq!(map.write_volatile(4081, [1; 16]), None);
            assert_eq!(map.read_volatile::<16>(4081), None);
            assert_eq!(map.read_volatile::<4>(usize::MAX - 1), None);
            assert_eq!(map.as_slice()[4080], 7);
        }
        {
            let mut map = Mapping::new(file.as_fd(), 4096, 0, false).unwrap();
            assert_eq!(map.read_volatile::<16>(4080), Some([7; 16]));
            assert_eq!(map.write_volatile(0, [1; 16]), None, "read-only");
            assert_eq!(map.as_slice()[0], 0);
        }
        std::fs::remove_file(path).unwrap();
    }

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
