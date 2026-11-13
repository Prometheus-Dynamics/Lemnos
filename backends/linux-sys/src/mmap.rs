//! Shared memory mappings of descriptors (`mmap`/`munmap`): dma-buf and V4L2
//! buffer memory. A [`Mapping`] unmaps itself on drop.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr::NonNull;
use std::sync::atomic::AtomicU64;

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
    /// The mapped words, as atomics shared with whoever else maps them. `None` unless the mapping
    /// is writable and a whole number of aligned words.
    pub fn as_atomic_words(&self) -> Option<&[AtomicU64]> {
        let words = self.len / size_of::<AtomicU64>();
        let first = self.ptr.as_ptr().cast::<AtomicU64>();
        if !self.writable || !self.len.is_multiple_of(size_of::<AtomicU64>()) || !first.is_aligned()
        {
            return None;
        }
        // SAFETY: `ptr` maps `len` readable and writable bytes for as long as `&self` lives,
        // holding `words` whole `AtomicU64`s, aligned as just checked. Every bit pattern is a
        // valid `AtomicU64`, and the only access this slice gives is atomic operations, which are
        // well defined when another process writes the same memory.
        Some(unsafe { std::slice::from_raw_parts(first, words) })
    }

    /// The mapped words, for loads only, on a mapping of either kind: a read-only one can be read
    /// this way too. `None` unless the mapping is a whole number of aligned words.
    pub fn load_words(&self) -> Option<Loads<'_>> {
        let words = self.len / size_of::<AtomicU64>();
        let first = self.ptr.as_ptr().cast::<AtomicU64>();
        if !self.len.is_multiple_of(size_of::<AtomicU64>()) || !first.is_aligned() {
            return None;
        }
        // The slice is wrapped in `Loads`, which gives loads and no stores, so a read-only mapping
        // is never written through it.
        // SAFETY: `ptr` maps `len` readable bytes for as long as `&self` lives, holding `words`
        // whole `AtomicU64`s, aligned as just checked. Every bit pattern is a valid `AtomicU64`.
        let words = unsafe { std::slice::from_raw_parts(first, words) };
        Some(Loads { words })
    }

    /// Copies `data` to `offset` in the mapping. `None`, and nothing written, outside the mapping
    /// or when the mapping is read-only.
    pub fn write_bytes(&self, offset: usize, data: &[u8]) -> Option<()> {
        let end = offset.checked_add(data.len())?;
        if end > self.len || !self.writable {
            return None;
        }
        let at = self.ptr.as_ptr().wrapping_add(offset);
        // SAFETY: `at..at + data.len()` lies inside the `len` writable bytes mapped, as just
        // checked; `data` is a borrowed slice that can't overlap memory this mapping owns unless
        // the caller passed the mapping itself, which `&self` allows, so use the copy that
        // tolerates overlap.
        unsafe { std::ptr::copy(data.as_ptr(), at, data.len()) };
        Some(())
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

impl Mapping {
    /// Reads the 32-bit word at `offset` with one aligned volatile load, as a device register
    /// needs. `None` when the word is not wholly inside the mapping or `offset` is not a multiple
    /// of 4.
    pub fn read32(&self, offset: usize) -> Option<u32> {
        let at = self.word(offset)?;
        // SAFETY: `word` checked that the four bytes at `at` lie inside the `len` bytes `new`
        // mapped and that `at` is aligned for `u32` (the mapping starts on a page); the mapping
        // stays mapped while `&self` lives. Any bit pattern is a `u32`.
        Some(unsafe { at.read_volatile() })
    }

    /// Writes `value` to the 32-bit word at `offset` with one aligned volatile store. `None`, and
    /// nothing written, when the word is not wholly inside the mapping, `offset` is not a
    /// multiple of 4, or the mapping is read-only.
    pub fn write32(&mut self, offset: usize, value: u32) -> Option<()> {
        if !self.writable {
            return None;
        }
        let at = self.word(offset)?;
        // SAFETY: as for `read32`; the store also needs a writable mapping, checked above, and
        // `&mut self` makes it exclusive.
        unsafe { at.write_volatile(value) };
        Some(())
    }

    /// The word at `offset`, if it is aligned and wholly inside the mapping.
    fn word(&self, offset: usize) -> Option<*mut u32> {
        let end = offset.checked_add(size_of::<u32>())?;
        let at = self.ptr.as_ptr().wrapping_add(offset).cast::<u32>();
        (end <= self.len && at.is_aligned()).then_some(at)
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the range was mapped by `new` and is unmapped only here. An
        // error here cannot be reported from drop and leaves nothing to undo.
        let _ = unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

/// The system's page size in bytes (4096 when it cannot be read).
pub fn page_size() -> usize {
    // SAFETY: `sysconf` takes no pointers and only reads the page size.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(page).unwrap_or(4096)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn words_are_aligned_inside_and_respect_writability() {
        let path = std::env::temp_dir().join(format!("lemnos-mmap-words-{}", std::process::id()));
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
            assert_eq!(map.write32(4092, 0x1234_5678), Some(()));
            assert_eq!(map.read32(4092), Some(0x1234_5678));
            assert_eq!(map.as_slice()[4092..], 0x1234_5678u32.to_ne_bytes());
            // Past the end, unaligned, and an offset that overflows: refused, nothing written.
            assert_eq!(map.write32(4096, 1), None);
            assert_eq!(map.write32(2, 1), None);
            assert_eq!(map.read32(2), None);
            assert_eq!(map.read32(usize::MAX - 1), None);
            assert_eq!(map.as_slice()[..8], [0; 8]);
        }
        {
            let mut map = Mapping::new(file.as_fd(), 4096, 0, false).unwrap();
            assert_eq!(map.read32(4092), Some(0x1234_5678));
            assert_eq!(map.write32(0, 1), None, "read-only");
        }
        std::fs::remove_file(path).unwrap();
    }

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

    #[test]
    fn atomic_words_need_a_writable_whole_word_mapping() {
        use crate::dma_buf::sealed_memfd;
        use std::sync::atomic::Ordering;
        let fd = sealed_memfd(c"words", 4096).unwrap();
        let map = Mapping::new(fd.as_fd(), 4096, 0, true).unwrap();
        let words = map.as_atomic_words().unwrap();
        assert_eq!(words.len(), 512);
        words[3].store(0xfeed, Ordering::SeqCst);
        assert_eq!(map.as_slice()[3 * 8..3 * 8 + 2], [0xed, 0xfe]);
        let read_only = Mapping::new(fd.as_fd(), 4096, 0, false).unwrap();
        assert!(read_only.as_atomic_words().is_none());
        let short = Mapping::new(fd.as_fd(), 4092, 0, true).unwrap();
        assert!(short.as_atomic_words().is_none());
    }

    #[test]
    fn write_bytes_is_bounded_and_needs_a_writable_mapping() {
        use crate::dma_buf::sealed_memfd;
        let fd = sealed_memfd(c"bytes", 4096).unwrap();
        let map = Mapping::new(fd.as_fd(), 4096, 0, true).unwrap();
        assert_eq!(map.write_bytes(4090, &[7; 6]), Some(()));
        assert_eq!(map.as_slice()[4095], 7);
        assert_eq!(map.write_bytes(4091, &[1; 6]), None);
        assert_eq!(map.write_bytes(usize::MAX, &[1]), None);
        let read_only = Mapping::new(fd.as_fd(), 4096, 0, false).unwrap();
        assert_eq!(read_only.write_bytes(0, &[1]), None);
    }

    #[test]
    fn a_sealed_memfd_cant_be_resized_through_any_descriptor() {
        use crate::dma_buf::sealed_memfd;
        let fd = sealed_memfd(c"sealed", 8192).unwrap();
        let duplicate = fd.try_clone().unwrap();
        let file = std::fs::File::from(duplicate);
        assert!(file.set_len(0).is_err());
        assert!(file.set_len(16384).is_err());
        assert_eq!(file.metadata().unwrap().len(), 8192);
        // Contents stay writable.
        let map = Mapping::new(fd.as_fd(), 8192, 0, true).unwrap();
        assert_eq!(map.write_bytes(0, &[9]), Some(()));
    }
}

/// A mapping's words, for loads only (see [`Mapping::load_words`]).
#[derive(Clone, Copy)]
pub struct Loads<'a> {
    words: &'a [AtomicU64],
}

impl Loads<'_> {
    /// How many words there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Word `index`, or `None` past the end.
    #[must_use]
    pub fn load(&self, index: usize, order: std::sync::atomic::Ordering) -> Option<u64> {
        self.words.get(index).map(|word| word.load(order))
    }
}
