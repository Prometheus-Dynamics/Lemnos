//! The `/dev/vg_lite` kernel interface of the VGLite 2.5D GPU (a VeriSilicon GCNanoUltraV), and
//! the only safe way to hand it work: a command [`Stream`] built through typed, checked methods.
//!
//! # Sources
//!
//! The request (`VG_LITE_IOCTL`), the command and error numbers and the structures are
//! transcribed from `VGLiteKernel/vg_lite_kernel.h` and `VGLiteKernel/linux/vg_lite_ioctl.h` of
//! the K230 Linux SDK (`kendryte/k230_linux_sdk` at `fc6cc21`,
//! `buildroot-overlay/package/vg_lite/`; dual MIT and GPL-2.0, used under MIT). The command words
//! and register addresses are from `VGLite/vg_lite.c` and `VGLite/vg_lite_path.c` of the same tree.
//! The sizes and offsets the layout tests pin were printed by the host C compiler (LP64, the
//! riscv64 Linux ABI) from that header. No code is copied.
//!
//! # What reaches the GPU
//!
//! The kernel takes a command buffer and runs it; the words in it carry the addresses, strides
//! and sizes the GPU reads and writes. So nothing here lets a caller write a word: a [`Stream`]
//! has no raw-word or raw-register method. Every address comes from a [`Buffer`] the stream
//! borrows and checks; every register whose bits change how the GPU reaches memory (the target
//! and source formats, the configuration word, the path control word) is written from a typed
//! value or checked against the bits the encoders use. See [`Stream`] for the full list.
//!
//! The kernel driver maps a dma-buf by the address of its last scatter-list entry only, so a
//! dma-buf that is not one physically contiguous block would be read and written in the wrong
//! place, and user space cannot tell which it is. `VG_LITE_MAP` is therefore not offered: the
//! GPU reads and writes only memory the kernel allocated for it ([`Device::allocate`]).
//!
//! # Status
//!
//! Untested on board. The kernel side of each request is not in the vendor tree (the SDK's
//! `VGLiteKernel` has only the user side), so what the kernel does with a request is read from
//! what the vendor's user library expects of it. The board check is `testing/board-runs/E3.md`.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-linux-sys/src/vg_lite.rs`), with the
//! dma-buf map, the raw-word and raw-register writes left out, and the stream's register writes
//! made typed.

#![cfg(target_os = "linux")]

mod stream;

pub use stream::{
    Buffer, CONFIG_BITS, Layout, PATH_CONTROL_BITS, SOURCE_REGISTER_BITS, Scratch, SourceFormat,
    SourceOptions, Stream, TargetFormat,
};

use std::ffi::{CString, c_void};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

/// `VG_LITE_IOCTL`: the request number of the one generic command (not an `_IOC` encoding).
pub const IOCTL: u32 = 30_000;
/// The device node the vendor library opens.
pub const DEVICE: &str = "/dev/vg_lite";
/// `MAX_CONTIGUOUS_SIZE`: the window the vendor library maps from the device at offset 0 before
/// its first request. Nothing here reads or writes it; it is mapped because the library maps it
/// (what the kernel does with the mapping is not in the vendor tree).
pub const WINDOW_BYTES: usize = 0x0200_0000;
/// `VG_LITE_INFINITE`: a wait with no timeout.
pub const INFINITE: u32 = 0xFFFF_FFFF;
/// `CMDBUF_COUNT`: command buffers the kernel allocates per context.
pub const CMDBUF_COUNT: usize = 2;
/// `VG_LITE_HAL_MAP_DMABUF`. Not used: see the crate docs.
pub const MAP_DMABUF: u32 = 0x4;

/// `vg_lite_kernel_command_t`: the `command` of [`IoctlData`].
pub mod command {
    /// `VG_LITE_INITIALIZE`.
    pub const INITIALIZE: u32 = 0;
    /// `VG_LITE_TERMINATE`.
    pub const TERMINATE: u32 = 1;
    /// `VG_LITE_ALLOCATE`.
    pub const ALLOCATE: u32 = 2;
    /// `VG_LITE_FREE`.
    pub const FREE: u32 = 3;
    /// `VG_LITE_SUBMIT`.
    pub const SUBMIT: u32 = 4;
    /// `VG_LITE_WAIT`.
    pub const WAIT: u32 = 5;
    /// `VG_LITE_MAP`. Not issued here.
    pub const MAP: u32 = 8;
    /// `VG_LITE_UNMAP`. Not issued here.
    pub const UNMAP: u32 = 9;
    /// `VG_LITE_CHECK`: reads one register (`vg_lite_kernel_info_t`).
    pub const CHECK: u32 = 10;
}

/// `vg_lite_error_t`: the `error` the kernel writes back.
pub mod error {
    /// `VG_LITE_SUCCESS`.
    pub const SUCCESS: i32 = 0;
    /// `VG_LITE_INVALID_ARGUMENT`.
    pub const INVALID_ARGUMENT: i32 = 1;
    /// `VG_LITE_OUT_OF_MEMORY`.
    pub const OUT_OF_MEMORY: i32 = 2;
    /// `VG_LITE_NO_CONTEXT`.
    pub const NO_CONTEXT: i32 = 3;
    /// `VG_LITE_TIMEOUT`.
    pub const TIMEOUT: i32 = 4;
    /// `VG_LITE_OUT_OF_RESOURCES`.
    pub const OUT_OF_RESOURCES: i32 = 5;
    /// `VG_LITE_GENERIC_IO`.
    pub const GENERIC_IO: i32 = 6;
    /// `VG_LITE_NOT_SUPPORT`.
    pub const NOT_SUPPORT: i32 = 7;
    /// `VG_LITE_ALREADY_EXISTS`.
    pub const ALREADY_EXISTS: i32 = 8;
    /// `VG_LITE_NOT_ALIGNED`.
    pub const NOT_ALIGNED: i32 = 9;
    /// `VG_LITE_FLEXA_TIME_OUT`.
    pub const FLEXA_TIME_OUT: i32 = 10;
    /// `VG_LITE_FLEXA_HANDSHAKE_FAIL`.
    pub const FLEXA_HANDSHAKE_FAIL: i32 = 11;
    /// `VG_LITE_SYSTEM_CALL_FAIL`.
    pub const SYSTEM_CALL_FAIL: i32 = 12;
}

/// `struct ioctl_data`: the envelope [`IOCTL`] takes. `buffer` points at the command's structure
/// and `bytes` is its size.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IoctlData {
    /// A [`command`] number.
    pub command: u32,
    /// Written back: an [`error`] code.
    pub error: i32,
    /// The size of the structure `buffer` points at (`unsigned long`).
    pub bytes: usize,
    /// The command's structure.
    pub buffer: *mut c_void,
}

/// `vg_lite_kernel_initialize_t`: the GPU's context, its command buffers and tessellation
/// buffer, written back by the kernel.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Initialize {
    /// Command buffer size, in bytes.
    pub command_buffer_size: u32,
    /// Tessellation buffer width.
    pub tess_width: i32,
    /// Tessellation buffer height.
    pub tess_height: i32,
    /// The context storage the kernel fills ([`KernelContext`]).
    pub context: *mut c_void,
    /// Written back: the `capabilities` union, as its `u32` (`tiled` in the low two bits).
    pub capabilities: u32,
    /// Written back: the mapped command buffers.
    pub command_buffer: [*mut c_void; CMDBUF_COUNT],
    /// Written back: the GPU addresses of the command buffers.
    pub command_buffer_gpu: [u32; CMDBUF_COUNT],
    /// Written back: the tessellation buffer's physical address.
    pub physical_addr: u32,
    /// Written back: the tessellation buffer's logical address.
    pub logical_addr: *mut u8,
    /// Written back: the size of each level of the tessellation buffer.
    pub tessbuf_size: u32,
    /// Written back: the size of each level of the vg count buffer.
    pub countbuf_size: u32,
    /// Written back: the tessellation buffer's width and height.
    pub tess_w_h: u32,
}

/// `vg_lite_kernel_allocate_t`: a buffer the GPU can address.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Allocate {
    /// The size, in bytes.
    pub bytes: u32,
    /// Whether the memory must be physically contiguous (non-zero).
    pub contiguous: i32,
    /// Where to allocate; zero here (the vendor leaves it unset).
    pub flags: u32,
    /// Written back: the kernel's handle, to free it with.
    pub memory_handle: *mut c_void,
    /// Written back: the mapped CPU address.
    pub memory: *mut c_void,
    /// Written back: the kernel's own mapping.
    pub kmemory: *mut c_void,
    /// Written back: the GPU address.
    pub memory_gpu: u32,
}

/// `vg_lite_kernel_free_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Free {
    /// The handle [`Allocate`] wrote back.
    pub memory_handle: *mut c_void,
}

/// `vg_lite_kernel_submit_t`: a command buffer to run on a context.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Submit {
    /// The context [`Initialize`] filled.
    pub context: *mut c_void,
    /// The command bytes the kernel reads.
    pub commands: *mut c_void,
    /// The number of command bytes.
    pub command_size: u32,
    /// The command buffer's id.
    pub command_id: u32,
}

/// `vg_lite_kernel_wait_t`: waits for the GPU's events on a context.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Wait {
    /// The context to wait on.
    pub context: *mut c_void,
    /// The timeout, in milliseconds.
    pub timeout_ms: u32,
    /// The events to wait for.
    pub event_mask: u32,
    /// Written back: the events that came.
    pub event_got: u32,
}

/// `vg_lite_kernel_map_t`. Transcribed for the layout tests only; never issued (see the crate
/// docs).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Map {
    /// The flags: [`MAP_DMABUF`] for a dma-buf.
    pub flags: u32,
    /// The size, in bytes.
    pub bytes: u32,
    /// The logical address.
    pub logical: *mut c_void,
    /// The physical address.
    pub physical: u32,
    /// The dma-buf's descriptor.
    pub dma_buf_fd: i32,
    /// Written back: the kernel's handle.
    pub memory_handle: *mut c_void,
    /// Written back: the GPU address.
    pub memory_gpu: u32,
}

/// `vg_lite_kernel_unmap_t`. Transcribed for the layout tests only.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Unmap {
    /// The handle [`Map`] wrote back.
    pub memory_handle: *mut c_void,
}

/// `vg_lite_kernel_terminate_t`: the context to release.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Terminate {
    /// The context [`Initialize`] filled.
    pub context: *mut c_void,
}

/// `vg_lite_kernel_info_t`: one register, read by `VG_LITE_CHECK`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// The register's address (input).
    pub addr: u32,
    /// The register's value (written back).
    pub reg: u32,
}

/// `struct vg_lite_kernel_context`: the context the kernel fills in at `VG_LITE_INITIALIZE`.
/// The library places it in its own storage, so the caller allocates it: this is that storage,
/// and only the kernel writes its fields.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KernelContext {
    command_buffer: [*mut c_void; CMDBUF_COUNT],
    command_buffer_logical: [*mut c_void; CMDBUF_COUNT],
    command_buffer_klogical: [*mut c_void; CMDBUF_COUNT],
    command_buffer_physical: [u32; CMDBUF_COUNT],
    tess_buffer: *mut c_void,
    tessbuf_logical: *mut c_void,
    tessbuf_klogical: *mut c_void,
    tessbuf_physical: u32,
    power_context: *mut c_void,
    power_context_logical: *mut c_void,
    power_context_klogical: *mut c_void,
    power_context_physical: u32,
    power_context_size: u32,
    power_context_capacity: u32,
}

impl KernelContext {
    fn zeroed() -> Self {
        let null = std::ptr::null_mut();
        Self {
            command_buffer: [null; CMDBUF_COUNT],
            command_buffer_logical: [null; CMDBUF_COUNT],
            command_buffer_klogical: [null; CMDBUF_COUNT],
            command_buffer_physical: [0; CMDBUF_COUNT],
            tess_buffer: null,
            tessbuf_logical: null,
            tessbuf_klogical: null,
            tessbuf_physical: 0,
            power_context: null,
            power_context_logical: null,
            power_context_klogical: null,
            power_context_physical: 0,
            power_context_size: 0,
            power_context_capacity: 0,
        }
    }
}

/// Maps a `vg_lite_error_t` the kernel wrote back to an error; `SUCCESS` is `Ok`.
fn status(code: i32) -> io::Result<()> {
    use io::ErrorKind;
    let (kind, what) = match code {
        error::SUCCESS => return Ok(()),
        error::INVALID_ARGUMENT => (ErrorKind::InvalidInput, "invalid argument"),
        error::OUT_OF_MEMORY | error::OUT_OF_RESOURCES => (ErrorKind::OutOfMemory, "out of memory"),
        error::TIMEOUT => (ErrorKind::TimedOut, "timed out"),
        error::NOT_SUPPORT => (ErrorKind::Unsupported, "not supported"),
        _ => (ErrorKind::Other, "kernel error"),
    };
    Err(io::Error::new(
        kind,
        format!("vg_lite kernel: {what} (vg_lite_error_t {code})"),
    ))
}

/// The open device and the window the vendor library maps from it. Shared by everything made
/// from it: a [`Context`] or a [`Memory`] keeps it open.
struct Window {
    fd: OwnedFd,
    base: *mut c_void,
}

// SAFETY: `base` is a process-wide mapping nothing here reads or writes; the descriptor is used
// only to issue requests. Nothing in it is tied to a thread.
unsafe impl Send for Window {}
// SAFETY: as above: a shared reference does no more than issue requests on the descriptor.
unsafe impl Sync for Window {}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: `base` is the mapping `Device::open` made, of `WINDOW_BYTES`, never handed out;
        // everything that issues requests on the device holds this window alive.
        let _ = unsafe { libc::munmap(self.base, WINDOW_BYTES) };
    }
}

/// An open `/dev/vg_lite` with the vendor library's window mapped.
#[derive(Clone)]
pub struct Device {
    window: Arc<Window>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("fd", &self.window.fd.as_raw_fd())
            .finish()
    }
}

impl Device {
    /// Opens the device at `path` ([`DEVICE`]) read-write and maps its window at offset zero
    /// (`MAP_SHARED`), as the vendor library does before its first request.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = CString::new(path.as_ref().as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL byte"))?;
        // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
        let raw = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a descriptor `open` just returned, owned by nothing else.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: a fresh shared mapping at a kernel-chosen address; the result is compared with
        // `MAP_FAILED` before use, and nothing here dereferences it.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                WINDOW_BYTES,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            window: Arc::new(Window { fd, base }),
        })
    }

    /// One `VG_LITE_IOCTL` for `command` with `data` as its structure. Private: each caller
    /// below pairs a command with its structure and sets every pointer in it.
    fn request<T>(&self, command: u32, data: &mut T) -> io::Result<()> {
        let mut envelope = IoctlData {
            command,
            error: error::SUCCESS,
            bytes: size_of::<T>(),
            buffer: std::ptr::from_mut(data).cast(),
        };
        ioctl(self.fd(), &mut envelope)?;
        status(envelope.error)
    }

    fn fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.window.fd.as_fd()
    }

    /// Reads one of the chip's identification registers (`VG_LITE_CHECK`). Only the three the
    /// vendor reads at initialize are reachable, so no other address is sent to the kernel.
    pub fn identification(&self, which: Identification) -> io::Result<u32> {
        let mut info = Info {
            addr: which.address(),
            reg: 0,
        };
        self.request(command::CHECK, &mut info)?;
        Ok(info.reg)
    }

    /// `VG_LITE_INITIALIZE`: a context with `command_buffer_size` bytes per command buffer and a
    /// tessellation buffer of `tess_width` by `tess_height`.
    pub fn initialize(
        &self,
        command_buffer_size: u32,
        tess_width: i32,
        tess_height: i32,
    ) -> io::Result<Context> {
        let mut kernel = Box::new(KernelContext::zeroed());
        let mut init = Initialize {
            command_buffer_size,
            tess_width,
            tess_height,
            context: std::ptr::from_mut(&mut *kernel).cast(),
            capabilities: 0,
            command_buffer: [std::ptr::null_mut(); CMDBUF_COUNT],
            command_buffer_gpu: [0; CMDBUF_COUNT],
            physical_addr: 0,
            logical_addr: std::ptr::null_mut(),
            tessbuf_size: 0,
            countbuf_size: 0,
            tess_w_h: 0,
        };
        self.request(command::INITIALIZE, &mut init)?;
        let tessellation = Tessellation {
            physical: init.physical_addr,
            size: init.tessbuf_size,
            count_size: init.countbuf_size,
            tess_w_h: init.tess_w_h,
        };
        // Built before the buffers are checked: a context that fails below is still released
        // (`VG_LITE_TERMINATE` on drop), since the kernel holds the storage's address.
        let mut context = Context {
            device: self.clone(),
            kernel,
            buffers: [CommandBuffer::null(); CMDBUF_COUNT],
            current: 0,
            tessellation,
            capabilities: init.capabilities,
            spare: None,
            poisoned: false,
        };
        // The size is ours, not a value the kernel wrote back (`Initialize` carries it in).
        let len = command_buffer_size as usize;
        for (buffer, &ptr) in context.buffers.iter_mut().zip(&init.command_buffer) {
            if ptr.is_null() || len == 0 {
                return Err(io::Error::other(
                    "vg_lite kernel: initialize gave no command buffer",
                ));
            }
            *buffer = CommandBuffer {
                ptr: ptr.cast(),
                len,
            };
        }
        context.spare = Some(Stream::new(len, tessellation));
        Ok(context)
    }

    /// `VG_LITE_ALLOCATE`: `bytes` (at least one) of physically contiguous memory, mapped for the
    /// CPU and addressable by the GPU.
    pub fn allocate(&self, bytes: u32) -> io::Result<Memory> {
        if bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an empty allocation",
            ));
        }
        let mut allocate = Allocate {
            bytes,
            contiguous: 1,
            flags: 0,
            memory_handle: std::ptr::null_mut(),
            memory: std::ptr::null_mut(),
            kmemory: std::ptr::null_mut(),
            memory_gpu: 0,
        };
        self.request(command::ALLOCATE, &mut allocate)?;
        if allocate.memory_handle.is_null() {
            return Err(io::Error::other("vg_lite kernel: allocate gave no handle"));
        }
        // Built before the address is checked, so a refused allocation is still freed.
        let memory = Memory {
            device: self.clone(),
            handle: allocate.memory_handle,
            cpu: allocate.memory.cast(),
            len: bytes as usize,
            gpu: allocate.memory_gpu,
        };
        if memory.cpu.is_null() {
            return Err(io::Error::other("vg_lite kernel: allocate gave no mapping"));
        }
        if u64::from(memory.gpu) + u64::from(bytes) > 1 << 32 {
            return Err(io::Error::other(
                "vg_lite kernel: an allocation past the 32-bit GPU address space",
            ));
        }
        Ok(memory)
    }
}

/// `ioctl(fd, VG_LITE_IOCTL, envelope)`, retrying on `EINTR`. Private: only
/// [`Device::request`] calls it.
fn ioctl(fd: BorrowedFd<'_>, envelope: &mut IoctlData) -> io::Result<libc::c_int> {
    let arg = std::ptr::from_mut(envelope);
    loop {
        // SAFETY: `arg` is a live, exclusive `IoctlData` whose `buffer` points at a live,
        // exclusive structure of `bytes` bytes that `Device::request`'s caller paired with
        // `command`. Every pointer inside that structure was set by this crate: the kernel
        // context storage a `Context` owns, a handle the kernel gave, or a command buffer the
        // kernel mapped, filled with the words of a checked `Stream` (see `Context::run`).
        // `fd` is live.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), IOCTL as _, arg) };
        if ret >= 0 {
            return Ok(ret);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// The identification registers the vendor library reads at initialize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identification {
    /// The chip id (0x20).
    ChipId,
    /// The revision (0x24).
    Revision,
    /// The CID (0x30).
    Cid,
}

impl Identification {
    /// The register's address.
    pub const fn address(self) -> u32 {
        match self {
            Self::ChipId => 0x20,
            Self::Revision => 0x24,
            Self::Cid => 0x30,
        }
    }
}

/// The tessellation buffer the kernel wrote back at initialize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tessellation {
    /// The buffer's physical address.
    pub physical: u32,
    /// The size of the tessellation buffer, in bytes.
    pub size: u32,
    /// The size of the count buffer, in bytes.
    pub count_size: u32,
    /// The width in the low 16 bits and the height in the high 16.
    pub tess_w_h: u32,
}

/// One command buffer the kernel mapped: where its bytes are and how many there are.
#[derive(Clone, Copy)]
struct CommandBuffer {
    ptr: *mut u8,
    len: usize,
}

impl CommandBuffer {
    fn null() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }
}

/// Why [`Context::run`] did not complete a stream.
#[derive(Debug)]
pub enum RunError {
    /// Nothing reached the GPU: the stream was refused, or the submit failed. Every buffer the
    /// stream names may be freed.
    NotRun(io::Error),
    /// The stream was submitted and the wait for it failed: the GPU may still be reading and
    /// writing the buffers the stream names. The context refuses further streams; drop it, and
    /// do not free or reuse those buffers (leak them) unless the kernel is known to have stopped
    /// the GPU.
    Wait(io::Error),
}

impl RunError {
    /// The error underneath.
    pub fn error(&self) -> &io::Error {
        match self {
            Self::NotRun(e) | Self::Wait(e) => e,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRun(e) => write!(f, "not run: {e}"),
            Self::Wait(e) => write!(f, "submitted, and the wait failed: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

/// A VGLite context: its command buffers, which [`Context::run`] fills and runs, and the
/// tessellation buffer. Released with `VG_LITE_TERMINATE` when dropped.
pub struct Context {
    device: Device,
    kernel: Box<KernelContext>,
    buffers: [CommandBuffer; CMDBUF_COUNT],
    current: usize,
    tessellation: Tessellation,
    capabilities: u32,
    /// The words of the last stream, kept so that a stream allocates only once per context.
    spare: Option<Stream<'static>>,
    /// A wait failed: the GPU may still be running, so nothing more is submitted.
    poisoned: bool,
}

// SAFETY: the raw pointers are the kernel's mappings and storage for this context, used only
// through `&mut self` (`run`) or read as plain values; nothing in them is tied to a thread.
unsafe impl Send for Context {}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("tessellation", &self.tessellation)
            .field("capabilities", &self.capabilities)
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl Context {
    /// The device this context runs on, for [`Device::allocate`].
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The bytes in each command buffer.
    pub fn command_buffer_size(&self) -> usize {
        self.buffers[0].len
    }

    /// The tessellation buffer the kernel set up.
    pub fn tessellation(&self) -> Tessellation {
        self.tessellation
    }

    /// The `capabilities` word the kernel wrote back (`tiled` in its low two bits).
    pub fn capabilities(&self) -> u32 {
        self.capabilities
    }

    /// An empty stream for this context: its capacity is one command buffer, and its
    /// tessellation addresses are this context's own. After the first, a stream reuses the
    /// words of the one before it and allocates nothing.
    pub fn stream<'a>(&mut self) -> Stream<'a> {
        match self.spare.take() {
            Some(spare) => spare.into_fresh(),
            None => Stream::new(self.command_buffer_size(), self.tessellation),
        }
    }

    /// Takes back a stream that will not run, so that the next [`Context::stream`] reuses its
    /// words.
    pub fn recycle(&mut self, stream: Stream<'_>) {
        self.spare = Some(stream.into_fresh());
    }

    /// Runs a finished stream and waits for the GPU to complete it (`VG_LITE_SUBMIT`, then
    /// `VG_LITE_WAIT`). The wait is inside the call and the stream borrows every buffer it names
    /// until the call returns, so on success the GPU is done with them. See [`RunError`] for a
    /// failed wait.
    pub fn run(&mut self, stream: Stream<'_>) -> Result<(), RunError> {
        let result = self.submit_and_wait(&stream);
        self.spare = Some(stream.into_fresh());
        result
    }

    fn submit_and_wait(&mut self, stream: &Stream<'_>) -> Result<(), RunError> {
        if self.poisoned {
            return Err(RunError::NotRun(io::Error::other(
                "an earlier wait failed; this context submits nothing more",
            )));
        }
        stream
            .submittable(self.command_buffer_size())
            .map_err(RunError::NotRun)?;
        // A stream's tessellation addresses are trusted only when they are this context's own.
        if stream.tessellation() != self.tessellation {
            return Err(RunError::NotRun(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the stream's tessellation buffer is not this context's",
            )));
        }
        let words = stream.words();
        let buffer = self.buffers[self.current];
        let bytes = size_of_val(words);
        // SAFETY: `buffer.ptr` is the `buffer.len` bytes the kernel mapped for this command
        // buffer at initialize, and `submittable` checked `bytes <= buffer.len`. `words` is
        // `bytes` readable bytes. Nothing else writes the buffer: `&mut self` excludes other
        // users of this context, and the previous run waited for the GPU.
        unsafe { std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), buffer.ptr, bytes) };
        // SAFETY: the `bytes` just written, in the mapping described above, are valid to read
        // and nothing writes them while this slice lives.
        let stored = unsafe { std::slice::from_raw_parts(buffer.ptr, bytes) };
        clean_invalidate_dcache(stored);
        let mut submit = Submit {
            context: std::ptr::from_mut(&mut *self.kernel).cast(),
            commands: buffer.ptr.cast(),
            command_size: bytes as u32,
            command_id: self.current as u32,
        };
        self.device
            .request(command::SUBMIT, &mut submit)
            .map_err(RunError::NotRun)?;
        self.current = (self.current + 1) % CMDBUF_COUNT;
        let mut wait = Wait {
            context: std::ptr::from_mut(&mut *self.kernel).cast(),
            timeout_ms: INFINITE,
            event_mask: u32::MAX,
            event_got: 0,
        };
        self.device.request(command::WAIT, &mut wait).map_err(|e| {
            self.poisoned = true;
            RunError::Wait(e)
        })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let mut terminate = Terminate {
            context: std::ptr::from_mut(&mut *self.kernel).cast(),
        };
        let _ = self.device.request(command::TERMINATE, &mut terminate);
    }
}

/// Memory the kernel allocated for the GPU: physically contiguous, mapped for the CPU. Released
/// with `VG_LITE_FREE` when dropped.
pub struct Memory {
    device: Device,
    handle: *mut c_void,
    cpu: *mut u8,
    len: usize,
    gpu: u32,
}

// SAFETY: the allocation is owned by this value until it is dropped; its CPU view is reached
// only through `&mut self`, and nothing in it is tied to a thread.
unsafe impl Send for Memory {}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory")
            .field("len", &self.len)
            .field("gpu", &format_args!("{:#x}", self.gpu))
            .finish()
    }
}

impl Memory {
    /// The bytes the memory spans.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True for an empty memory (never: [`Device::allocate`] refuses zero bytes).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The CPU's view of the allocation.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `cpu` is the `len` bytes the kernel mapped for this allocation (checked
        // non-null when it was made); it is unmapped only when `self` drops, and `&mut self`
        // excludes other access through it. The GPU touches it only inside `Context::run`,
        // whose stream borrows this value, so no CPU slice exists then.
        unsafe { std::slice::from_raw_parts_mut(self.cpu, self.len) }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        let mut free = Free {
            memory_handle: self.handle,
        };
        let _ = self.device.request(command::FREE, &mut free);
    }
}

/// Writes the CPU's stores in `bytes` back to memory and invalidates the lines, before the GPU
/// reads them or the CPU reads what the GPU wrote: the vendor's
/// `thead_csi_dcache_clean_invalid_range` (`th.dcache.civa` per 64-byte line, between fences)
/// on riscv64. Nothing happens on other architectures.
pub fn clean_invalidate_dcache(bytes: &[u8]) {
    #[cfg(target_arch = "riscv64")]
    {
        let mut line = bytes.as_ptr() as usize;
        let mut remaining = bytes.len() as i64 + (line % 64) as i64;
        // SAFETY: a fence orders memory accesses and touches no memory.
        unsafe { core::arch::asm!("fence iorw, iorw") };
        while remaining > 0 {
            // SAFETY: cache maintenance on the line holding `line`, inside memory `bytes`
            // borrows; it writes no memory. The T-Head `th.dcache.civa a0` (XTheadCmo), encoding
            // 0x0275000b as the dev branch transcribed it (unverified on board).
            unsafe { core::arch::asm!(".long 0x0275000b", in("a0") line) };
            line += 64;
            remaining -= 64;
        }
        // SAFETY: fences, as the vendor's helper ends with; they touch no memory.
        unsafe { core::arch::asm!("fence iorw, iorw", "fence.i", "fence r, r") };
    }
    #[cfg(not(target_arch = "riscv64"))]
    let _ = bytes;
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests;

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
