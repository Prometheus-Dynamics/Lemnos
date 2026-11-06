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

/// The kernel's memory accounting, read for the capacity check.
const MEMINFO: &str = "/proc/meminfo";
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

/// A system-heap request may take at most this share of `MemAvailable` at the
/// time of the call (one quarter). Above it, reclaim starts, which is what a
/// request this large would otherwise push the kernel into.
pub const SYSTEM_HEAP_SHARE_DIVISOR: u64 = 4;

/// The capacity of a heap, in bytes, from `/proc/meminfo` text: a contiguous
/// (CMA) heap may take what is free in its pool (`CmaFree`); any other heap
/// (the page-backed `system` heap) may take a share of `MemAvailable`.
///
/// Fails when the figure is missing, so a caller never allocates against an
/// unknown capacity.
pub fn heap_capacity_from_meminfo(name: &str, meminfo: &str) -> io::Result<u64> {
    let (key, share) = if name.contains("cma") {
        ("CmaFree", 1)
    } else {
        ("MemAvailable", SYSTEM_HEAP_SHARE_DIVISOR)
    };
    meminfo_bytes(meminfo, key)
        .map(|bytes| bytes / share)
        .ok_or_else(|| io::Error::other(format!("{key} is not in {MEMINFO}")))
}

/// The capacity of the heap called `name` right now (see
/// [`heap_capacity_from_meminfo`]).
pub fn heap_capacity(name: &str) -> io::Result<u64> {
    heap_capacity_from_meminfo(name, &std::fs::read_to_string(MEMINFO)?)
}

/// The value of `key` in `/proc/meminfo` text, in bytes.
fn meminfo_bytes(meminfo: &str, key: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        let kib: u64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
        Some(kib * 1024)
    })
}

/// An open dma-heap (`/dev/dma_heap/<name>`).
#[derive(Debug)]
pub struct DmaHeap {
    file: File,
    path: PathBuf,
    /// An explicit capacity (see [`DmaHeap::with_capacity`]), in bytes.
    limit: Option<u64>,
}

impl DmaHeap {
    /// Opens the heap called `name` under [`HEAP_DIR`].
    pub fn open(name: &str) -> io::Result<Self> {
        if name.is_empty() || name.contains('/') {
            return Err(invalid(format!("invalid dma-heap name {name:?}")));
        }
        Self::open_path(Path::new(HEAP_DIR).join(name))
    }

    /// Opens a heap node by path, read-only: enough to allocate from it.
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_path_with(path, false)
    }

    /// Opens a heap node by path read-write, as a tool does that reports
    /// whether the node's permissions allow it (`PermissionDenied` when the
    /// caller lacks the node's group). Allocation needs only [`Self::open_path`].
    pub fn open_path_read_write(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_path_with(path, true)
    }

    fn open_path_with(path: impl AsRef<Path>, write: bool) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::options().read(true).write(write).open(&path)?;
        Ok(Self {
            file,
            path,
            limit: None,
        })
    }

    /// Caps every allocation from this heap at `bytes`, below the heap's own
    /// capacity (see [`heap_capacity`]). A request above the cap fails before
    /// the kernel sees it.
    pub fn with_capacity(mut self, bytes: u64) -> Self {
        self.limit = Some(bytes);
        self
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
    ///
    /// The request is checked first against the heap's capacity (the CMA pool's
    /// free bytes, or a share of `MemAvailable` for the system heap) and against
    /// any cap set with [`DmaHeap::with_capacity`]. A request above either fails
    /// with [`io::ErrorKind::OutOfMemory`] and the kernel is not asked, so a
    /// large request cannot push it into reclaim or a failed contiguous search.
    pub fn allocate(&self, len: usize) -> io::Result<DmaBuf> {
        if len == 0 {
            return Err(invalid("cannot allocate an empty dma-buf".into()));
        }
        let want = len as u64;
        let mut capacity = heap_capacity(self.name())?;
        if let Some(limit) = self.limit {
            capacity = capacity.min(limit);
        }
        if want > capacity {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!(
                    "a {len}-byte request exceeds the {} heap's capacity of {capacity} bytes",
                    self.name()
                ),
            ));
        }
        let fd = dma_heap::allocate(self.file.as_fd(), want)?;
        Ok(DmaBuf { fd, len })
    }

    /// Allocates a dma-buf another driver can import whole, as
    /// [`Self::allocate`] does, but at least [`IMPORT_SLACK`] bytes past `len`
    /// and rounded up to whole pages. [`DmaBuf::len`] is that padded size,
    /// which is also the size the kernel reports for the buffer.
    ///
    /// Use it for a buffer a GPU may import: Mesa's v3dv takes a linear
    /// dma-buf as a storage buffer only when the dma-buf is at least 256 bytes
    /// larger than what it binds. A frame whose size is a whole number of pages
    /// (NV12 1280x800 is 375 pages of 4 KiB) would otherwise be refused.
    pub fn allocate_importable(&self, len: usize) -> io::Result<DmaBuf> {
        self.allocate(importable_len(len, lemnos_linux_sys::mmap::page_size())?)
    }
}

/// The bytes past a buffer's end that [`DmaHeap::allocate_importable`] adds
/// before rounding to pages.
pub const IMPORT_SLACK: usize = 256;

/// `len` plus [`IMPORT_SLACK`], rounded up to whole pages of `page` bytes.
pub fn importable_len(len: usize, page: usize) -> io::Result<usize> {
    len.checked_add(IMPORT_SLACK)
        .and_then(|n| n.checked_next_multiple_of(page.max(1)))
        .ok_or_else(|| invalid(format!("a {len}-byte dma-buf is too large to pad")))
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

    /// Maps the whole buffer read-only; bracket CPU access as for [`Self::map`].
    pub fn map_read_only(&self) -> io::Result<Mapping> {
        Mapping::new(self.fd.as_fd(), self.len, 0, false)
    }

    /// Starts a CPU access (`DMA_BUF_SYNC_START`): makes the device's writes
    /// visible to the CPU, or flushes the CPU's before a read by a device.
    pub fn begin_cpu_access(&self, access: Access) -> io::Result<()> {
        sync(self.fd.as_fd(), Phase::Begin, access)
    }

    /// Ends a CPU access (`DMA_BUF_SYNC_END`), handing the buffer back to the
    /// devices.
    pub fn end_cpu_access(&self, access: Access) -> io::Result<()> {
        sync(self.fd.as_fd(), Phase::End, access)
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
    /// Starts a CPU access through this borrowed descriptor; see [`DmaBuf::begin_cpu_access`].
    pub fn begin_cpu_access(&self, access: Access) -> io::Result<()> {
        sync(self.fd, Phase::Begin, access)
    }

    /// Ends a CPU access through this borrowed descriptor; see [`DmaBuf::end_cpu_access`].
    pub fn end_cpu_access(&self, access: Access) -> io::Result<()> {
        sync(self.fd, Phase::End, access)
    }

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

    /// Maps the whole buffer read-only; bracket CPU access as for [`DmaBuf::map`].
    pub fn map_read_only(&self) -> io::Result<Mapping> {
        Mapping::new(self.fd, self.len, 0, false)
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

/// Which end of a CPU access a [`sync`] call brackets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// The start of the access: the device's writes become visible to the CPU
    /// (for a read), or the CPU's are flushed before a device reads.
    Begin,
    /// The end of the access: the buffer goes back to the devices.
    End,
}

static SYNC_CALLS: AtomicU64 = AtomicU64::new(0);
static SYNC_NS: AtomicU64 = AtomicU64::new(0);

/// Brackets a CPU access to a dma-buf through a borrowed descriptor
/// (`DMA_BUF_IOCTL_SYNC`): [`Phase::Begin`] before the CPU touches the mapping,
/// [`Phase::End`] after. A descriptor that is not a dma-buf is an error.
///
/// Every call, failed or not, is counted with its time in [`sync_stats`]; the
/// `begin_cpu_access` and `end_cpu_access` of [`DmaBuf`] and [`DmaBufRef`]
/// come through here.
pub fn sync(fd: BorrowedFd<'_>, phase: Phase, access: Access) -> io::Result<()> {
    let start_or_end = match phase {
        Phase::Begin => dma_buf::SYNC_START,
        Phase::End => dma_buf::SYNC_END,
    };
    let started = Instant::now();
    let result = dma_buf::sync(fd, start_or_end | access.bits());
    let ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    SYNC_CALLS.fetch_add(1, Ordering::Relaxed);
    SYNC_NS.fetch_add(ns, Ordering::Relaxed);
    result
}

/// The process's dma-buf syncs so far ([`sync`], counted for the whole
/// process): `(calls, nanoseconds spent in them)`. The two counters are read
/// one after the other, so a sync running meanwhile may show in one only.
pub fn sync_stats() -> (u64, u64) {
    (
        SYNC_CALLS.load(Ordering::Relaxed),
        SYNC_NS.load(Ordering::Relaxed),
    )
}

/// Waits on a sync file (or any descriptor that polls as one) until it
/// signals or `timeout` passes (`None`: forever). Returns whether it signalled.
/// A fence whose work failed is an error. Borrowed form of [`SyncFence::wait`].
///
/// A descriptor that is not a sync file can hang up or be invalid without ever
/// becoming readable (`POLLHUP` or `POLLNVAL` with no `POLLIN`). That is an
/// error at once, not "not signalled" for the whole timeout.
pub fn wait_fence(fd: BorrowedFd<'_>, timeout: Option<Duration>) -> io::Result<bool> {
    let revents = poll::poll_one(fd, poll::POLLIN, timeout)?;
    if revents & poll::POLLERR != 0 {
        return Err(io::Error::other("the fence's work failed"));
    }
    if revents & poll::POLLIN != 0 {
        return Ok(true);
    }
    if revents & (poll::POLLHUP | poll::POLLNVAL) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a sync_file: the descriptor hung up or is invalid",
        ));
    }
    Ok(false)
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
    #[test]
    fn an_importable_buffer_has_the_slack_and_whole_pages() {
        use super::{IMPORT_SLACK, importable_len};
        // NV12 1280x800: exactly 375 pages of 4 KiB, so one page more.
        let nv12 = 1280 * 800 * 3 / 2;
        assert_eq!(importable_len(nv12, 4096).unwrap(), nv12 + 4096);
        assert_eq!(importable_len(nv12, 16384).unwrap(), 1_540_096);
        assert_eq!(importable_len(1, 4096).unwrap(), 4096);
        assert_eq!(importable_len(4096 - IMPORT_SLACK, 4096).unwrap(), 4096);
        assert_eq!(importable_len(4096 - IMPORT_SLACK + 1, 4096).unwrap(), 8192);
        assert!(importable_len(usize::MAX - 100, 4096).is_err());
    }

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
    fn a_read_write_open_reports_a_node_it_may_not_write() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("lemnos-dma-ro-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let read = DmaHeap::open_path(&path);
        let write = DmaHeap::open_path_read_write(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(read.is_ok(), "{read:?}");
        // Root may write any file; anyone else is refused.
        if let Err(e) = write {
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        }
    }

    #[test]
    fn a_read_only_mapping_reads_the_buffer() {
        let mut file = File::from(plain_file("map-ro", 4096));
        file.write_all(b"lemnos").unwrap();
        let buf = DmaBuf::from_fd(OwnedFd::from(file)).unwrap();
        let mapping = buf.map_read_only().unwrap();
        assert_eq!(&mapping.as_slice()[..6], b"lemnos");
        assert_eq!(
            &buf.borrow().map_read_only().unwrap().as_slice()[..6],
            b"lemnos"
        );
    }

    #[test]
    fn a_plain_file_is_not_a_dma_buf() {
        let buf = DmaBuf::from_fd(plain_file("plain", 4096)).unwrap();
        assert!(buf.begin_cpu_access(Access::Read).is_err());
        assert!(buf.export_fence(Access::Read).is_err());
    }

    #[test]
    fn meminfo_figures_are_read_in_bytes() {
        let text = "MemTotal:        4063952 kB\nMemAvailable:    3834104 kB\nCmaTotal:          65536 kB\nCmaFree:           43004 kB\n";
        assert_eq!(meminfo_bytes(text, "CmaFree"), Some(43004 * 1024));
        assert_eq!(meminfo_bytes(text, "CmaTotalX"), None);
        assert_eq!(meminfo_bytes("MemAvailable: 12 MB\n", "MemAvailable"), None);
    }

    #[test]
    fn a_cma_heap_is_capped_by_its_free_pool() {
        let text = "MemAvailable:    4000000 kB\nCmaFree:           65536 kB\n";
        for name in ["linux,cma", "default_cma_region"] {
            assert_eq!(
                heap_capacity_from_meminfo(name, text).unwrap(),
                65536 * 1024
            );
        }
    }

    #[test]
    fn the_system_heap_is_capped_by_a_share_of_available_memory() {
        let text = "MemAvailable:    4000000 kB\nCmaFree:           65536 kB\n";
        assert_eq!(
            heap_capacity_from_meminfo("system", text).unwrap(),
            4_000_000 * 1024 / SYSTEM_HEAP_SHARE_DIVISOR
        );
    }

    #[test]
    fn a_missing_figure_is_an_error_not_a_zero_or_no_cap() {
        assert!(heap_capacity_from_meminfo("system", "MemTotal: 1 kB\n").is_err());
        assert!(heap_capacity_from_meminfo("linux,cma", "MemAvailable: 1 kB\n").is_err());
    }

    #[test]
    fn a_request_above_the_capacity_fails_before_the_kernel() {
        // The cap is set far below any real heap, so the request is refused by
        // the library's check whether or not the heap exists here.
        let heap = match DmaHeap::open("system") {
            Ok(heap) => heap.with_capacity(64 * 1024),
            Err(e) => {
                crate::tests::skip(&format!("no system dma-heap ({e})"));
                return;
            }
        };
        let err = heap.allocate(1 << 20).expect_err("above the cap");
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        let buf = heap.allocate(64 * 1024).unwrap();
        assert_eq!(buf.len(), 64 * 1024);
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
        // The borrowed forms bracket the same access on the same buffer.
        {
            let view = buf.borrow();
            view.begin_cpu_access(Access::Read).unwrap();
            assert!(map.as_slice().iter().all(|&b| b == 0xa5));
            view.end_cpu_access(Access::Read).unwrap();
            sync(buf.as_fd(), Phase::Begin, Access::Write).unwrap();
            sync(buf.as_fd(), Phase::End, Access::Write).unwrap();
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

    /// A sync file that can never signal (a pipe whose writer has gone) fails
    /// at once: it hangs up without becoming readable, and that is not "not
    /// signalled". The owned and borrowed forms agree.
    #[test]
    fn fence_wait_outcomes_for_a_hung_up_descriptor() {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(writer);
        let started = std::time::Instant::now();
        let err = wait_fence(reader.as_fd(), Some(Duration::from_secs(5)))
            .expect_err("a hung-up descriptor is not a fence");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(started.elapsed() < Duration::from_secs(2), "fails at once");
        assert!(fence_is_signaled(reader.as_fd()).is_err());
        let owned = SyncFence::from_fd(OwnedFd::from(reader));
        assert!(owned.wait(Some(Duration::from_secs(5))).is_err());
        assert!(owned.is_signaled().is_err());
    }

    /// Data still buffered behind a closed writer is readable: a fence that
    /// reads true, not an error.
    #[test]
    fn a_hung_up_pipe_with_data_is_readable_not_an_error() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"x").unwrap();
        drop(writer);
        assert!(wait_fence(reader.as_fd(), Some(Duration::ZERO)).unwrap());
    }

    /// Bracketing a CPU access on a descriptor that is not a dma-buf is an error.
    #[test]
    fn sync_on_a_plain_descriptor_is_an_error() {
        let (reader, _writer) = std::io::pipe().unwrap();
        assert!(sync(reader.as_fd(), Phase::Begin, Access::Read).is_err());
        assert!(sync(reader.as_fd(), Phase::End, Access::Read).is_err());
    }

    #[test]
    fn every_sync_is_counted_failed_or_not() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let (calls, _) = sync_stats();
        let _ = sync(reader.as_fd(), Phase::Begin, Access::Read);
        let _ = sync(reader.as_fd(), Phase::End, Access::Read);
        // Other tests sync in parallel, so the count grows by at least two.
        assert!(sync_stats().0 >= calls + 2);
    }
}
