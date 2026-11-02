//! DMA buffers: dma-heap allocation, dma-buf CPU access, mappings, names, and
//! sync-file fences.
//!
//! Lemnos allocates and synchronises the buffers and hands their descriptors
//! over. It does not pool them, decide which one holds which frame, or move
//! data: a [`DmaBuf`] is passed to whoever reads or writes it (a V4L2 queue, a
//! DMA engine, a GPU), as the same descriptor, never a copy.
//!
//! ```no_run
//! use lemnos_linux::dma::{Access, DmaHeap};
//!
//! let heap = DmaHeap::open("system")?;
//! let buf = heap.allocate(1 << 20)?;
//! let mut map = buf.map()?;
//! buf.begin_cpu_access(Access::Write)?;
//! map.as_mut_slice().fill(0);
//! buf.end_cpu_access(Access::Write)?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Heaps (`/dev/dma_heap/<name>`) differ per board: `system` (page-backed) is
//! always there on current kernels; contiguous ones (`linux,cma`) and vendor
//! heaps are named by the board. Sync files need kernel support; where it is
//! missing the calls return [`io::ErrorKind::Unsupported`].

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use lemnos_linux_sys::{dma_buf, dma_heap, poll};

pub use lemnos_linux_sys::mmap::Mapping;

/// Where dma-heaps live.
pub const HEAP_DIR: &str = "/dev/dma_heap";

/// The kernel keeps at most this many bytes of a dma-buf name, with its NUL.
pub const NAME_CAPACITY: usize = 32;

/// The direction of a CPU access to a dma-buf, or of a fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    /// The CPU (or the fence's reader) reads.
    Read,
    /// The CPU (or the fence's writer) writes.
    Write,
    /// Both.
    ReadWrite,
}

impl Access {
    /// The `DMA_BUF_SYNC_*` bits for this direction.
    fn bits(self) -> u64 {
        match self {
            Self::Read => dma_buf::SYNC_READ,
            Self::Write => dma_buf::SYNC_WRITE,
            Self::ReadWrite => dma_buf::SYNC_READ | dma_buf::SYNC_WRITE,
        }
    }
}

/// An open dma-heap (`/dev/dma_heap/<name>`).
#[derive(Debug)]
pub struct DmaHeap {
    file: File,
    path: PathBuf,
}

impl DmaHeap {
    /// Opens the heap called `name` under [`HEAP_DIR`].
    pub fn open(name: &str) -> io::Result<Self> {
        if name.is_empty() || name.contains('/') {
            return Err(invalid(format!("invalid dma-heap name {name:?}")));
        }
        Self::open_path(Path::new(HEAP_DIR).join(name))
    }

    /// Opens a heap node by path.
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        Ok(Self { file, path })
    }

    /// The heap's name (its node's file name).
    pub fn name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    }

    /// Allocates a dma-buf of `len` bytes, read-write and close-on-exec. The
    /// kernel may round the size up; [`DmaBuf::len`] is what was asked for.
    pub fn allocate(&self, len: usize) -> io::Result<DmaBuf> {
        if len == 0 {
            return Err(invalid("cannot allocate an empty dma-buf".into()));
        }
        let fd = dma_heap::allocate(self.file.as_fd(), len as u64)?;
        Ok(DmaBuf { fd, len })
    }
}

impl AsFd for DmaHeap {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// The dma-heaps under [`HEAP_DIR`], sorted by name. Empty when the directory
/// does not exist (no DMA heaps on this kernel).
pub fn list_heaps() -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(HEAP_DIR) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    Ok(names)
}

/// A dma-buf: a shared buffer of memory the CPU and devices can both reach.
#[derive(Debug)]
pub struct DmaBuf {
    fd: OwnedFd,
    len: usize,
}

impl DmaBuf {
    /// Wraps a dma-buf descriptor from elsewhere (an allocation, a V4L2 export,
    /// a received descriptor). Its size is read from the kernel.
    /// The descriptor's file offset is left as it was.
    pub fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        let len = dmabuf_len(fd.as_fd())?;
        Ok(Self { fd, len })
    }

    /// Borrows this buffer's descriptor, with its size already known: the
    /// same operations, no dup.
    pub fn borrow(&self) -> DmaBufRef<'_> {
        DmaBufRef {
            fd: self.fd.as_fd(),
            len: self.len,
        }
    }

    /// The buffer's size in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty (never, for an allocated buffer).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Maps the whole buffer read-write. Bracket CPU access to the mapping with
    /// [`Self::begin_cpu_access`] and [`Self::end_cpu_access`].
    pub fn map(&self) -> io::Result<Mapping> {
        Mapping::new(self.fd.as_fd(), self.len, 0, true)
    }

    /// Starts a CPU access (`DMA_BUF_SYNC_START`): makes the device's writes
    /// visible to the CPU, or flushes the CPU's before a read by a device.
    pub fn begin_cpu_access(&self, access: Access) -> io::Result<()> {
        dma_buf::sync(self.fd.as_fd(), dma_buf::SYNC_START | access.bits())
    }

    /// Ends a CPU access (`DMA_BUF_SYNC_END`), handing the buffer back to the
    /// devices.
    pub fn end_cpu_access(&self, access: Access) -> io::Result<()> {
        dma_buf::sync(self.fd.as_fd(), dma_buf::SYNC_END | access.bits())
    }

    /// Names the buffer for debugging (shown in the kernel's dma-buf debug
    /// files). At most 31 bytes, no NUL.
    pub fn set_name(&self, name: &str) -> io::Result<()> {
        if name.len() >= NAME_CAPACITY {
            return Err(invalid(format!(
                "dma-buf name of {} bytes (at most {} allowed)",
                name.len(),
                NAME_CAPACITY - 1
            )));
        }
        let c = CString::new(name).map_err(|_| invalid("dma-buf name contains NUL".into()))?;
        dma_buf::set_name(self.fd.as_fd(), &c)
    }

    /// Exports a fence that signals when the buffer's pending accesses in
    /// `access` direction are done: for [`Access::Read`], its pending writes;
    /// for the others, every pending use. An idle buffer exports one that is
    /// already signalled.
    pub fn export_fence(&self, access: Access) -> io::Result<SyncFence> {
        export_fence_of(self.fd.as_fd(), access)
    }

    /// Attaches `fence` to the buffer: a read fence (`Access::Read`) makes later
    /// writers wait for it, a write fence makes later users of any kind wait.
    /// The fence stays with the caller.
    pub fn import_fence(&self, access: Access, fence: &SyncFence) -> io::Result<()> {
        import_fence_to(self.fd.as_fd(), access, fence.as_fd())
    }

    /// Which buffer this is, as its device and inode numbers: the same for every
    /// descriptor of one dma-buf, so a caller can recognise a buffer it has
    /// seen before (a V4L2 queue keeping the buffer attached, say).
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        dma_buf::identity(self.fd.as_fd())
    }

    /// Gives up the descriptor.
    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsFd for DmaBuf {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// A dma-buf descriptor someone else owns (a frame's, say), used in place: the
/// same reads of size, identity and fences as [`DmaBuf`], with no dup and no
/// close. Its size is read once, at [`DmaBufRef::from_fd`].
#[derive(Clone, Copy, Debug)]
pub struct DmaBufRef<'a> {
    fd: BorrowedFd<'a>,
    len: usize,
}

impl<'a> DmaBufRef<'a> {
    /// Borrows a dma-buf descriptor, reading its size from the kernel. The
    /// descriptor's file offset is left as it was.
    pub fn from_fd(fd: BorrowedFd<'a>) -> io::Result<Self> {
        let len = dmabuf_len(fd)?;
        Ok(Self { fd, len })
    }

    /// The buffer's size in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty (never, for an allocated buffer).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The descriptor, borrowed for as long as the owner keeps it.
    pub fn as_fd(&self) -> BorrowedFd<'a> {
        self.fd
    }

    /// The buffer's identity (device and inode numbers); see [`DmaBuf::identity`].
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        dma_buf::identity(self.fd)
    }

    /// Exports a fence for the buffer's pending accesses; see [`DmaBuf::export_fence`].
    pub fn export_fence(&self, access: Access) -> io::Result<SyncFence> {
        export_fence_of(self.fd, access)
    }

    /// Attaches a fence to the buffer; see [`DmaBuf::import_fence`].
    pub fn import_fence(&self, access: Access, fence: &SyncFence) -> io::Result<()> {
        import_fence_to(self.fd, access, fence.as_fd())
    }

    /// Maps the whole buffer read-write; bracket CPU access as for [`DmaBuf::map`].
    pub fn map(&self) -> io::Result<Mapping> {
        Mapping::new(self.fd, self.len, 0, true)
    }
}

/// A sync file: a fence that signals when some device work finishes. Wait on
/// it with [`Self::wait`], or give it to another API that takes a fence.
#[derive(Debug)]
pub struct SyncFence {
    fd: OwnedFd,
}

impl SyncFence {
    /// Wraps a sync-file descriptor from elsewhere (a driver's out-fence).
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Waits until the fence signals or `timeout` passes (`None`: forever).
    /// Returns whether it signalled. A fence whose work failed is an error.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        wait_fence(self.fd.as_fd(), timeout)
    }

    /// Whether the fence has signalled, without waiting.
    pub fn is_signaled(&self) -> io::Result<bool> {
        fence_is_signaled(self.fd.as_fd())
    }

    /// The descriptor, to poll with other descriptors.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Gives up the descriptor.
    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsFd for SyncFence {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// Waits on a sync file (or any descriptor that polls as one) until it
/// signals or `timeout` passes (`None`: forever). Returns whether it signalled.
/// A fence whose work failed is an error. Borrowed form of [`SyncFence::wait`].
pub fn wait_fence(fd: BorrowedFd<'_>, timeout: Option<Duration>) -> io::Result<bool> {
    let revents = poll::poll_one(fd, poll::POLLIN, timeout)?;
    if revents & poll::POLLERR != 0 {
        return Err(io::Error::other("the fence's work failed"));
    }
    Ok(revents & poll::POLLIN != 0)
}

/// Whether a sync file has signalled, without waiting. Borrowed form of
/// [`SyncFence::is_signaled`].
pub fn fence_is_signaled(fd: BorrowedFd<'_>) -> io::Result<bool> {
    wait_fence(fd, Some(Duration::ZERO))
}

fn export_fence_of(fd: BorrowedFd<'_>, access: Access) -> io::Result<SyncFence> {
    let fd = dma_buf::export_sync_file(fd, access.bits() as u32)?;
    Ok(SyncFence { fd })
}

fn import_fence_to(fd: BorrowedFd<'_>, access: Access, fence: BorrowedFd<'_>) -> io::Result<()> {
    dma_buf::import_sync_file(fd, access.bits() as u32, fence)
}

/// A dma-buf's size, from the kernel, without moving the descriptor's file
/// offset (see `dma_buf::size`).
fn dmabuf_len(fd: BorrowedFd<'_>) -> io::Result<usize> {
    let end = dma_buf::size(fd)?;
    usize::try_from(end).map_err(|_| invalid(format!("dma-buf of {end} bytes")))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A plain file standing in for a descriptor: the kernel refuses dma-buf
    /// ioctls on it, which the tests use as the not-a-dma-buf case.
    fn plain_file(name: &str, len: u64) -> OwnedFd {
        let path = std::env::temp_dir().join(format!("lemnos-dma-{name}-{}", std::process::id()));
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(len).unwrap();
        std::fs::remove_file(path).unwrap();
        OwnedFd::from(file)
    }

    #[test]
    fn a_descriptor_reports_its_size() {
        let mut file = File::from(plain_file("size", 12_345));
        file.write_all(b"abc").unwrap();
        let fd = OwnedFd::from(file);
        let buf = DmaBuf::from_fd(fd).unwrap();
        assert_eq!(buf.len(), 12_345);
        assert!(!buf.is_empty());
    }

    #[test]
    fn identity_is_shared_by_descriptors_of_one_buffer() {
        let buf = DmaBuf::from_fd(plain_file("id-a", 4096)).unwrap();
        let other = DmaBuf::from_fd(plain_file("id-b", 4096)).unwrap();
        let dup = DmaBuf::from_fd(buf.fd.try_clone().unwrap()).unwrap();
        assert_eq!(buf.identity().unwrap(), dup.identity().unwrap());
        assert_ne!(buf.identity().unwrap(), other.identity().unwrap());
    }

    #[test]
    fn names_are_checked_before_the_kernel_sees_them() {
        let buf = DmaBuf::from_fd(plain_file("name", 4096)).unwrap();
        assert!(buf.set_name(&"x".repeat(NAME_CAPACITY)).is_err());
        assert!(buf.set_name("nul\0inside").is_err());
    }

    #[test]
    fn a_plain_file_is_not_a_dma_buf() {
        let buf = DmaBuf::from_fd(plain_file("plain", 4096)).unwrap();
        assert!(buf.begin_cpu_access(Access::Read).is_err());
        assert!(buf.export_fence(Access::Read).is_err());
    }

    #[test]
    fn heap_names_are_checked() {
        assert!(DmaHeap::open("").is_err());
        assert!(DmaHeap::open("../etc").is_err());
    }

    #[test]
    fn list_heaps_is_empty_or_sorted() {
        let names = list_heaps().unwrap();
        assert!(names.windows(2).all(|w| w[0] <= w[1]));
    }

    /// The system heap (`/dev/dma_heap/system`): allocate, map, sync, name, and
    /// fence. Skipped where there is no such heap or no permission for it.
    #[test]
    fn the_system_heap_allocates_maps_and_syncs() {
        let heap = match DmaHeap::open("system") {
            Ok(heap) => heap,
            Err(e) => {
                crate::tests::skip(&format!("no system dma-heap ({e})"));
                return;
            }
        };
        assert_eq!(heap.name(), "system");
        assert!(heap.allocate(0).is_err());
        let buf = heap.allocate(4096).unwrap();
        assert_eq!(buf.len(), 4096);
        let mut map = buf.map().unwrap();
        buf.begin_cpu_access(Access::Write).unwrap();
        map.as_mut_slice().fill(0xa5);
        buf.end_cpu_access(Access::Write).unwrap();
        buf.begin_cpu_access(Access::Read).unwrap();
        assert!(map.as_slice().iter().all(|&b| b == 0xa5));
        buf.end_cpu_access(Access::Read).unwrap();
        buf.set_name("lemnos-dma-test").unwrap();

        // An idle buffer's fence is already signalled; importing it back works.
        match buf.export_fence(Access::ReadWrite) {
            Ok(fence) => {
                assert!(fence.is_signaled().unwrap());
                buf.import_fence(Access::Read, &fence).unwrap();
            }
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                crate::tests::skip(&format!("fence part: {e}"));
            }
            Err(e) => panic!("export_fence: {e}"),
        }
        let again = DmaBuf::from_fd(buf.into_fd()).unwrap();
        assert_eq!(again.len(), 4096);
    }

    #[test]
    fn sizing_a_borrowed_descriptor_keeps_its_offset_and_makes_no_dup() {
        use std::io::{Seek, SeekFrom};
        let mut file = File::from(plain_file("offset", 12_345));
        file.seek(SeekFrom::Start(5)).unwrap();
        let buf = DmaBufRef::from_fd(file.as_fd()).unwrap();
        assert_eq!(buf.len(), 12_345);
        assert_eq!(
            file.stream_position().unwrap(),
            5,
            "the caller's offset is where it was"
        );
        let owned = DmaBuf::from_fd(OwnedFd::from(file.try_clone().unwrap())).unwrap();
        assert_eq!(owned.len(), 12_345);
        assert_eq!(file.stream_position().unwrap(), 5, "from_fd too");
    }

    #[test]
    fn a_borrowed_view_has_the_owned_buffers_identity() {
        let buf = DmaBuf::from_fd(plain_file("borrow-id", 4096)).unwrap();
        let view = buf.borrow();
        assert_eq!(view.len(), 4096);
        assert_eq!(view.identity().unwrap(), buf.identity().unwrap());
    }

    #[test]
    fn borrowed_fence_polling_works_on_any_pollable_descriptor() {
        use std::io::Write;
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        assert!(!wait_fence(reader.as_fd(), Some(Duration::ZERO)).unwrap());
        assert!(!fence_is_signaled(reader.as_fd()).unwrap());
        writer.write_all(b"x").unwrap();
        assert!(fence_is_signaled(reader.as_fd()).unwrap());
        assert!(wait_fence(reader.as_fd(), Some(Duration::from_millis(10))).unwrap());
        let mut byte = [0u8; 1];
        std::io::Read::read_exact(&mut reader, &mut byte).unwrap();
        assert!(!fence_is_signaled(reader.as_fd()).unwrap());
    }
}
