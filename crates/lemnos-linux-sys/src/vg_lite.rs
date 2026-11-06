//! The `/dev/vg_lite` kernel interface of the K230's VGLite 2.5D GPU: the generic request
//! (`VG_LITE_IOCTL`), the command structures the kernel reads and writes, and the calls the
//! VGLite driver makes on them, from `vg_lite_kernel.h` and `linux/vg_lite_ioctl.h` of the K230
//! Linux SDK v0.6.1 (Vivante, MIT).
//!
//! The structures hold pointers the kernel follows: a command buffer, a user mapping, a memory
//! handle. [`Device`], [`Context`] and [`Memory`] are the safe calls over them. Each request
//! takes its pointers from memory the call borrows or from a value the owner keeps alive, with
//! the length checked, and each raw access carries a `SAFETY` comment.
//!
//! Untested on board: the layouts are checked against the header on a host C compiler (LP64,
//! the riscv64 K230 Linux ABI), and the kernel's behaviour is not in the vendor tree. The
//! vendor's `VGLiteKernel` has only the user side of the request; the kernel driver is not
//! shipped in it, so what the kernel does with a request is unverified.

use std::ffi::c_void;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::Arc;

use crate::ioctl::ioctl_ptr;

/// `VG_LITE_IOCTL`: the request number of the one generic command, not an `_IO` encoding.
pub const IOCTL: u32 = 30_000;
/// The device node the vendor library opens.
pub const DEVICE_PATH: &str = "/dev/vg_lite";
/// `MAX_CONTIGUOUS_SIZE`: the window the vendor library maps from the device at offset 0 before
/// its first request. What the kernel does with that mapping is not in the vendor tree; it is
/// made here as the library makes it (unverified).
pub const WINDOW_BYTES: usize = 0x0200_0000;
/// `VG_LITE_INFINITE`: a wait with no timeout.
pub const INFINITE: u32 = 0xFFFF_FFFF;

/// `vg_lite_kernel_command_t`: the `command` of [`IoctlData`] and the operations used here.
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
    /// `VG_LITE_MAP`.
    pub const MAP: u32 = 8;
    /// `VG_LITE_UNMAP`.
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

/// `VG_LITE_HAL_MAP_DMABUF`: the `flags` of [`Map`] for a dma-buf.
pub const MAP_DMABUF: u32 = 0x4;
/// `CMDBUF_COUNT`: command buffers the kernel allocates per context.
pub const CMDBUF_COUNT: usize = 2;

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
/// buffers, written back by the kernel.
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
    /// Written back: the tessellation buffers' physical address.
    pub physical_addr: u32,
    /// Written back: the tessellation buffers' logical address.
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
    /// Where to allocate (a `VG_LITE_RESERVED_ALLOCATOR`-style flag word).
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
    /// The context [`Initialize`] wrote back.
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

/// `vg_lite_kernel_map_t`: maps a dma-buf (with [`MAP_DMABUF`]) or user memory for the GPU.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Map {
    /// The flags: [`MAP_DMABUF`] for a dma-buf.
    pub flags: u32,
    /// The size, in bytes.
    pub bytes: u32,
    /// The logical address, for user memory; null for a dma-buf.
    pub logical: *mut c_void,
    /// The physical address, for physical memory; zero otherwise.
    pub physical: u32,
    /// The dma-buf's descriptor, for a dma-buf.
    pub dma_buf_fd: i32,
    /// Written back: the kernel's handle, to unmap with.
    pub memory_handle: *mut c_void,
    /// Written back: the GPU address.
    pub memory_gpu: u32,
}

/// `vg_lite_kernel_unmap_t`.
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
    /// The context [`Initialize`] wrote back.
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
/// The library declares it in the header and places it in its own storage, so the caller
/// allocates it: this mirror is that storage, and the kernel writes its fields.
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
        Self {
            command_buffer: [std::ptr::null_mut(); CMDBUF_COUNT],
            command_buffer_logical: [std::ptr::null_mut(); CMDBUF_COUNT],
            command_buffer_klogical: [std::ptr::null_mut(); CMDBUF_COUNT],
            command_buffer_physical: [0; CMDBUF_COUNT],
            tess_buffer: std::ptr::null_mut(),
            tessbuf_logical: std::ptr::null_mut(),
            tessbuf_klogical: std::ptr::null_mut(),
            tessbuf_physical: 0,
            power_context: std::ptr::null_mut(),
            power_context_logical: std::ptr::null_mut(),
            power_context_klogical: std::ptr::null_mut(),
            power_context_physical: 0,
            power_context_size: 0,
            power_context_capacity: 0,
        }
    }
}

// SAFETY: the pointers in the storage are the kernel's mappings for this context. The storage
// is owned by one `Context`, which is only used through `&mut` or behind a lock in its owner.
unsafe impl Send for KernelContext {}

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

/// The open device and the window the vendor library maps from it. Shared by everything that
/// was made from it: a [`Context`] or a [`Memory`] keeps it open.
struct Window {
    fd: OwnedFd,
    base: *mut c_void,
}

// SAFETY: the mapping is process-wide and read and written only through the kernel's requests
// (`Device`'s methods) and the mapped memory the owners borrow. Nothing here is thread-bound.
unsafe impl Send for Window {}
// SAFETY: as above: shared references do no more than issue requests on the descriptor.
unsafe impl Sync for Window {}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: `base` is the mapping `Device::open` made, of `WINDOW_BYTES`, and every
        // reference into it (through a `Context` or `Memory`) holds this window alive.
        let _ = unsafe { libc::munmap(self.base, WINDOW_BYTES) };
    }
}

/// An open `/dev/vg_lite` with the vendor library's window mapped.
#[derive(Clone)]
pub struct Device {
    window: Arc<Window>,
}

impl Device {
    /// Opens `/dev/vg_lite` read-write and maps its window at offset zero (`MAP_SHARED`), as the
    /// vendor library does before its first request.
    pub fn open() -> io::Result<Self> {
        let path = std::ffi::CString::new(DEVICE_PATH).expect("the device path has no NUL");
        // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
        let raw = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a descriptor `open` just returned, owned by nothing else.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: a fresh mapping at a kernel-chosen address of the device, read-write and
        // shared; the result is checked against `MAP_FAILED` before use.
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

    /// One `VG_LITE_IOCTL` for `command`, with `data` as its structure.
    fn request<T>(&self, command: u32, data: &mut T) -> io::Result<()> {
        let mut envelope = IoctlData {
            command,
            error: error::SUCCESS,
            bytes: size_of::<T>(),
            buffer: (data as *mut T).cast(),
        };
        // SAFETY: `envelope` is a live `IoctlData` and `data` a live `T` the caller borrows
        // exclusively. The kernel reads and writes `size_of::<T>()` bytes at `data`; any pointer
        // inside `T` was set by the safe caller from memory it borrows or owns for the call.
        unsafe {
            ioctl_ptr(
                self.window.fd.as_fd(),
                IOCTL,
                &mut envelope as *mut IoctlData,
            )?
        };
        status(envelope.error)
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

    /// `VG_LITE_INITIALIZE`: a context with `command_buffer_size` bytes of command buffer per
    /// buffer and a tessellation buffer of `tess_width` by `tess_height` (zero for none).
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
            context: (&mut *kernel as *mut KernelContext).cast(),
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
        // Built before the buffers are checked: a context that fails here is still released
        // (`VG_LITE_TERMINATE` on drop), since the kernel holds the storage's address.
        let mut context = Context {
            device: self.clone(),
            kernel,
            buffers: [CommandBuffer::null(); CMDBUF_COUNT],
            current: 0,
            tessellation: Tessellation {
                physical: init.physical_addr,
                size: init.tessbuf_size,
                count_size: init.countbuf_size,
                tess_w_h: init.tess_w_h,
            },
            capabilities: init.capabilities,
        };
        for (buffer, &ptr) in context.buffers.iter_mut().zip(&init.command_buffer) {
            if ptr.is_null() || init.command_buffer_size == 0 {
                return Err(io::Error::other(
                    "vg_lite kernel: initialize gave no command buffer",
                ));
            }
            *buffer = CommandBuffer {
                ptr: ptr.cast(),
                len: init.command_buffer_size as usize,
            };
        }
        Ok(context)
    }

    /// `VG_LITE_ALLOCATE`: `bytes` of physically contiguous memory, mapped for the CPU and
    /// addressable by the GPU.
    pub fn allocate(&self, bytes: u32) -> io::Result<Memory> {
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
        if allocate.memory_handle.is_null() || allocate.memory.is_null() {
            return Err(io::Error::other("vg_lite kernel: allocate gave no memory"));
        }
        Ok(Memory {
            device: self.clone(),
            handle: allocate.memory_handle,
            cpu: allocate.memory.cast(),
            len: bytes as usize,
            gpu: allocate.memory_gpu,
            allocated: true,
        })
    }

    /// `VG_LITE_MAP` of a dma-buf `fd` of `bytes` bytes, with `logical` as its CPU address.
    ///
    /// The kernel keeps `logical` for as long as the returned [`Memory`] lives, so the caller
    /// keeps that mapping alive for the same span, and drops the `Memory` before the mapping.
    /// The length is checked here.
    pub fn map_dmabuf(
        &self,
        fd: BorrowedFd<'_>,
        logical: &mut [u8],
        bytes: u32,
    ) -> io::Result<Memory> {
        if bytes as usize > logical.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{bytes} bytes mapped from {} bytes", logical.len()),
            ));
        }
        let mut map = Map {
            flags: MAP_DMABUF,
            bytes,
            logical: logical.as_mut_ptr().cast(),
            physical: 0,
            dma_buf_fd: fd.as_raw_fd(),
            memory_handle: std::ptr::null_mut(),
            memory_gpu: 0,
        };
        self.request(command::MAP, &mut map)?;
        if map.memory_handle.is_null() {
            return Err(io::Error::other("vg_lite kernel: map gave no handle"));
        }
        Ok(Memory {
            device: self.clone(),
            handle: map.memory_handle,
            cpu: std::ptr::null_mut(),
            len: bytes as usize,
            gpu: map.memory_gpu,
            allocated: false,
        })
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

/// The tessellation buffer the kernel wrote back at initialize: its physical address, the size
/// of each level, and its width and height packed as the library packs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tessellation {
    /// The buffer's physical address.
    pub physical: u32,
    /// The size of each level of the tessellation buffer, in bytes.
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

// SAFETY: the mapping is the kernel's for this context and is accessed only by the owning
// `Context`, through `&mut` or behind the owner's lock.
unsafe impl Send for CommandBuffer {}

/// A VGLite context: its command buffers, which [`Context::run`] fills and runs, and the
/// tessellation buffer. Released with `VG_LITE_TERMINATE` when dropped.
pub struct Context {
    device: Device,
    kernel: Box<KernelContext>,
    buffers: [CommandBuffer; CMDBUF_COUNT],
    current: usize,
    tessellation: Tessellation,
    capabilities: u32,
}

impl Context {
    /// The device this context runs on, for [`Device::allocate`] and [`Device::map_dmabuf`].
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
    /// tessellation addresses are this context's own.
    pub fn stream<'a>(&self) -> Stream<'a> {
        Stream::new(self.command_buffer_size(), self.tessellation)
    }

    /// Runs a finished stream and waits for the GPU to complete it (`VG_LITE_SUBMIT`, then
    /// `VG_LITE_WAIT`). The wait is inside the call, so every buffer the stream names is still
    /// borrowed while the GPU may touch it.
    pub fn run(&mut self, stream: Stream<'_>) -> io::Result<()> {
        stream.submittable(self.command_buffer_size())?;
        // `Stream::new` is public, so a stream's tessellation addresses are trusted only when
        // they are this context's own.
        if stream.tessellation != self.tessellation {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the stream's tessellation buffer is not this context's",
            ));
        }
        let words = stream.words();
        let buffer = self.buffers[self.current];
        let bytes = std::mem::size_of_val(words);
        // SAFETY: `buffer.ptr` is the `buffer.len` bytes the kernel mapped for this command
        // buffer at initialize, and `submittable` checked `bytes <= buffer.len`. `words` is
        // `bytes` readable bytes. Nothing else writes the buffer: `&mut self` excludes other
        // users of this context, and the previous run waited for its GPU work.
        unsafe {
            std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), buffer.ptr, bytes);
        }
        // SAFETY: the `bytes` just written, in the mapping described above, are valid to read.
        let stored = unsafe { std::slice::from_raw_parts(buffer.ptr, bytes) };
        clean_invalidate_dcache(stored);
        let mut submit = Submit {
            context: (&mut *self.kernel as *mut KernelContext).cast(),
            commands: buffer.ptr.cast(),
            command_size: bytes as u32,
            command_id: self.current as u32,
        };
        self.device.request(command::SUBMIT, &mut submit)?;
        self.current = (self.current + 1) % CMDBUF_COUNT;
        let mut wait = Wait {
            context: (&mut *self.kernel as *mut KernelContext).cast(),
            timeout_ms: INFINITE,
            event_mask: u32::MAX,
            event_got: 0,
        };
        self.device.request(command::WAIT, &mut wait)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let mut terminate = Terminate {
            context: (&mut *self.kernel as *mut KernelContext).cast(),
        };
        let _ = self.device.request(command::TERMINATE, &mut terminate);
    }
}

/// Something a [`Stream`] may name as a GPU buffer. Sealed: the kernel's [`Memory`] and the
/// host-only [`Scratch`] are the only implementations, so a safe caller cannot make a stream
/// point at an address it did not get from the kernel for a real submit.
pub trait Buffer: sealed::Sealed {
    /// The GPU address of the buffer's first byte.
    fn gpu_address(&self) -> u32;
    /// The bytes the buffer spans.
    fn byte_len(&self) -> usize;
    /// Whether the kernel owns the memory. A stream naming a buffer that is not kernel-backed
    /// is refused by [`Context::run`].
    fn kernel_backed(&self) -> bool;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Memory {}
    impl Sealed for super::Scratch {}
}

impl Buffer for Memory {
    fn gpu_address(&self) -> u32 {
        self.gpu
    }
    fn byte_len(&self) -> usize {
        self.len
    }
    fn kernel_backed(&self) -> bool {
        true
    }
}

/// An address and a length with no kernel memory behind them: for building and checking streams
/// on a host without a device. [`Context::run`] refuses a stream that names one, so a `Scratch`
/// never reaches the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scratch {
    gpu: u32,
    bytes: usize,
}

impl Scratch {
    /// A stand-in buffer at `gpu_address` spanning `bytes` bytes. Not kernel memory.
    pub fn new(gpu_address: u32, bytes: usize) -> Self {
        Self {
            gpu: gpu_address,
            bytes,
        }
    }
}

impl Buffer for Scratch {
    fn gpu_address(&self) -> u32 {
        self.gpu
    }
    fn byte_len(&self) -> usize {
        self.bytes
    }
    fn kernel_backed(&self) -> bool {
        false
    }
}

/// The layout a render target or a source takes in a stream: its pixel size, dimensions and
/// stride in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Bytes per pixel.
    pub bytes_per_pixel: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in rows.
    pub height: u32,
    /// Bytes from one row to the next.
    pub stride: u32,
}

impl Layout {
    /// Checks the layout against `buffer` and returns `stride * height`, the bytes the layout
    /// covers from the buffer's first byte. The rows must hold a row of pixels, the dimensions
    /// must fit the 16-bit fields the registers take, and the whole span must lie inside the
    /// buffer and the 32-bit GPU address space.
    fn extent<B: Buffer + ?Sized>(&self, buffer: &B) -> io::Result<u32> {
        let refused = |why: String| io::Error::new(io::ErrorKind::InvalidInput, why);
        if self.bytes_per_pixel == 0 || self.width == 0 || self.height == 0 {
            return Err(refused(format!("an empty layout {self:?}")));
        }
        if self.width > u16::MAX as u32 || self.height > u16::MAX as u32 {
            return Err(refused(format!(
                "{self:?} exceeds the 16-bit register fields"
            )));
        }
        self.width
            .checked_mul(self.bytes_per_pixel)
            .filter(|&row| row <= self.stride)
            .ok_or_else(|| refused(format!("stride {} holds no row of {self:?}", self.stride)))?;
        let span = (self.stride as u64) * (self.height as u64);
        if span > buffer.byte_len() as u64 {
            return Err(refused(format!(
                "{span} bytes of {self:?} exceed the {}-byte buffer",
                buffer.byte_len()
            )));
        }
        if buffer.gpu_address() as u64 + span > 1 << 32 {
            return Err(refused(format!("{self:?} runs past the GPU address space")));
        }
        Ok(span as u32)
    }
}

/// Registers a stream may write with a value of the caller's choosing. None of them holds an
/// address or an extent. Their meaning beyond the vendor's use is unverified on board.
const PLAIN_REGISTERS: &[u32] = &[
    0x0A00, 0x0A02, 0x0A18, 0x0A19, 0x0A1A, 0x0A1B, 0x0A1C, 0x0A1D, 0x0A1E, 0x0A1F, 0x0A20, 0x0A21,
    0x0A22, 0x0A25,
];
/// Colour-key registers: written with values of the caller's choosing, as the vendor's set-up
/// does (zero).
const COLOUR_KEY_REGISTERS: std::ops::RangeInclusive<u32> = 0x0A90..=0x0A97;
/// Registers the vendor writes as zero and a stream writes only as zero.
const ZERO_REGISTERS: &[u32] = &[0x0A27, 0x0A2D, 0x0A34, 0x0A39, 0x0AD1];

/// The opcode of a state write, `VG_LITE_STATE(address)`.
const STATE: u32 = 0x3001_0000;
/// `VG_LITE_SEMAPHORE(7)` and `VG_LITE_STALL(7)`: the pixel engine's flush.
const SEMAPHORE_7: u32 = 0x1000_0007;
const STALL_7: u32 = 0x2000_0007;
/// `VG_LITE_DATA(1)`, the header of a rectangle or a pixel-engine clear.
const DATA_1: u32 = 0x4000_0001;
/// `VG_LITE_STATES(256, 0x0B00)`: the colour table's run header.
const CLUT_HEADER: u32 = 0x3100_0B00;
/// `VG_LITE_NOP()`: pads the colour table to an even count.
const NOP: u32 = 0x8000_0000;

/// A stream of command words, checked as it is built. Every address a stream carries comes from
/// a [`Buffer`] borrowed for `'a`, through a typed method that checks the buffer's extent. Raw
/// words are accepted only for the plain registers and the flush sequence ([`Stream::push_raw`]).
/// A finished stream runs with [`Context::run`].
pub struct Stream<'a> {
    words: Vec<u32>,
    tessellation: Tessellation,
    /// The render target's width, height and bytes, once one is set.
    target: Option<(u32, u32, u32)>,
    kernel_only: bool,
    finished: bool,
    buffers: std::marker::PhantomData<&'a ()>,
}

impl<'a> Stream<'a> {
    /// An empty stream. `capacity` is the command buffer's size in bytes; `tessellation` is the
    /// context's tessellation buffer, which is the only source of tessellation addresses:
    /// [`Context::run`] refuses a stream whose tessellation is not the running context's.
    pub fn new(capacity: usize, tessellation: Tessellation) -> Self {
        // The capacity is checked at run; reserving only avoids regrowth for a usual stream.
        Self {
            words: Vec::with_capacity(capacity.min(4096) / 4),
            tessellation,
            target: None,
            kernel_only: true,
            finished: false,
            buffers: std::marker::PhantomData,
        }
    }

    /// The words so far.
    pub fn words(&self) -> &[u32] {
        &self.words
    }

    fn open(&self) -> io::Result<()> {
        if self.finished {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the stream is finished",
            ));
        }
        Ok(())
    }

    fn state(&mut self, address: u32, value: u32) {
        self.words.extend_from_slice(&[STATE | address, value]);
    }

    /// Writes `value` to a plain register (see [`PLAIN_REGISTERS`]).
    pub fn plain(&mut self, address: u32, value: u32) -> io::Result<()> {
        self.open()?;
        if !(PLAIN_REGISTERS.contains(&address) || COLOUR_KEY_REGISTERS.contains(&address)) {
            return Err(refused_register(address));
        }
        self.state(address, value);
        Ok(())
    }

    /// Writes zero to a register the vendor writes as zero (see [`ZERO_REGISTERS`]).
    pub fn zero(&mut self, address: u32) -> io::Result<()> {
        self.open()?;
        if !ZERO_REGISTERS.contains(&address) {
            return Err(refused_register(address));
        }
        self.state(address, 0);
        Ok(())
    }

    /// Sets the render target: its format word, and `buffer` as the target, with `layout`. The
    /// target's extent is checked against `buffer`, and later rectangles and clears are checked
    /// against its size.
    pub fn render_target<B: Buffer>(
        &mut self,
        buffer: &'a B,
        format_word: u32,
        layout: Layout,
    ) -> io::Result<()> {
        self.open()?;
        let bytes = layout.extent(buffer)?;
        self.kernel_only &= buffer.kernel_backed();
        self.state(0x0A10, format_word);
        self.state(0x0A11, buffer.gpu_address());
        self.state(0x0A12, layout.stride);
        self.state(0x0A13, layout.width | (layout.height << 16));
        self.target = Some((layout.width, layout.height, bytes));
        Ok(())
    }

    /// Sets the source: `buffer` with `layout`, after the source format word (`0x0A25`).
    pub fn source_surface<B: Buffer>(&mut self, buffer: &'a B, layout: Layout) -> io::Result<()> {
        self.open()?;
        layout.extent(buffer)?;
        self.kernel_only &= buffer.kernel_backed();
        self.zero(0x0A27)?;
        self.state(0x0A29, buffer.gpu_address());
        self.zero(0x0A34)?;
        self.state(0x0A2B, layout.stride);
        self.zero(0x0A2D)?;
        self.state(0x0A2F, layout.width | (layout.height << 16));
        Ok(())
    }

    /// A rectangle of the render target, (x, y) and `width` by `height`. It must lie inside the
    /// target's size.
    pub fn rectangle(&mut self, x: u32, y: u32, width: u32, height: u32) -> io::Result<()> {
        self.open()?;
        let (tw, th, _) = self.target_size()?;
        let inside = width > 0
            && height > 0
            && x.checked_add(width).is_some_and(|r| r <= tw)
            && y.checked_add(height).is_some_and(|b| b <= th);
        if !inside {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("rectangle {x},{y} {width}x{height} is outside the {tw}x{th} target"),
            ));
        }
        let pair = |a: u32, b: u32| a | (b << 16);
        self.words
            .extend_from_slice(&[DATA_1, 0, pair(x, y), pair(width, height)]);
        Ok(())
    }

    /// A pixel-engine clear of the first `bytes` bytes of the render target. `bytes` may not
    /// exceed the target's extent.
    pub fn pe_clear(&mut self, bytes: u32) -> io::Result<()> {
        self.open()?;
        let (_, _, extent) = self.target_size()?;
        if bytes > extent {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a clear of {bytes} bytes is past the {extent}-byte target"),
            ));
        }
        self.words.extend_from_slice(&[DATA_1, 0, bytes, 0]);
        Ok(())
    }

    fn target_size(&self) -> io::Result<(u32, u32, u32)> {
        self.target
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no render target is set"))
    }

    /// The 256-entry colour table at its fixed address, padded to an even count.
    pub fn clut(&mut self, colours: &[u32]) -> io::Result<()> {
        self.open()?;
        if colours.len() != 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a colour table of {} entries; the stream takes 256",
                    colours.len()
                ),
            ));
        }
        self.words.push(CLUT_HEADER);
        self.words.extend_from_slice(colours);
        self.words.push(NOP);
        Ok(())
    }

    /// The tessellation buffer's addresses and sizes, from the context (`program_tessellation`
    /// on the C908 build).
    pub fn tessellation_setup(&mut self) -> io::Result<()> {
        self.open()?;
        let tess = self.tessellation;
        self.state(0x0A35, tess.physical);
        self.state(0x0AC8, tess.size);
        self.state(0x0ACB, tess.physical.wrapping_add(tess.size));
        self.state(0x0ACC, tess.count_size);
        Ok(())
    }

    /// The pixel engine's flush: the target flushed, then a stall on module 7.
    pub fn flush(&mut self) -> io::Result<()> {
        self.open()?;
        self.state(0x0A1B, 1);
        self.words.extend_from_slice(&[SEMAPHORE_7, 0, STALL_7, 0]);
        Ok(())
    }

    /// Ends the stream: the flush, then the end command. Nothing may be added after.
    pub fn finish(&mut self) -> io::Result<()> {
        self.flush()?;
        self.words.extend_from_slice(&[0, 0]);
        self.finished = true;
        Ok(())
    }

    /// Adds raw words, accepting only: a state write to a plain register, or to a zero register
    /// with a zero value; the pixel engine's flush sequence; and nothing else. An address
    /// register, a data header, a colour-table run, an end command and an unknown opcode are
    /// refused, and nothing is added when any word is refused.
    pub fn push_raw(&mut self, words: &[u32]) -> io::Result<()> {
        self.open()?;
        let mut at = 0;
        while at < words.len() {
            let word = words[at];
            if word == SEMAPHORE_7 || word == STALL_7 {
                let quad = words.get(at..at + 4);
                if quad != Some(&[SEMAPHORE_7, 0, STALL_7, 0]) {
                    return Err(refused_word(at, word));
                }
                at += 4;
            } else if word & 0xFFFF_0000 == STATE {
                let address = word & 0xFFFF;
                let Some(&value) = words.get(at + 1) else {
                    return Err(refused_word(at, word));
                };
                let plain =
                    PLAIN_REGISTERS.contains(&address) || COLOUR_KEY_REGISTERS.contains(&address);
                let zero = ZERO_REGISTERS.contains(&address) && value == 0;
                if !(plain || zero) {
                    return Err(refused_word(at, word));
                }
                at += 2;
            } else {
                return Err(refused_word(at, word));
            }
        }
        self.words.extend_from_slice(words);
        Ok(())
    }

    /// Whether [`Context::run`] may submit this stream: it is finished, it names only kernel
    /// memory, and it fits one command buffer of `capacity` bytes.
    pub(crate) fn submittable(&self, capacity: usize) -> io::Result<()> {
        if !self.finished {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the stream is not finished",
            ));
        }
        if !self.kernel_only {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the stream names a buffer the kernel does not own",
            ));
        }
        let bytes = std::mem::size_of_val(self.words.as_slice());
        if bytes > capacity {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("{bytes} bytes of commands for a {capacity}-byte command buffer"),
            ));
        }
        Ok(())
    }
}

fn refused_register(address: u32) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("register {address:#06x} is not one a stream writes this way"),
    )
}

fn refused_word(at: usize, word: u32) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("word {word:#010x} at {at} is not an allowed command"),
    )
}

/// Memory the GPU addresses: a contiguous allocation the CPU maps, or a dma-buf the kernel
/// mapped. Released with `VG_LITE_FREE` or `VG_LITE_UNMAP` when dropped.
pub struct Memory {
    device: Device,
    handle: *mut c_void,
    cpu: *mut u8,
    len: usize,
    gpu: u32,
    allocated: bool,
}

impl Memory {
    /// The GPU address of the memory.
    pub fn gpu_address(&self) -> u32 {
        self.gpu
    }

    /// The bytes the memory spans.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True for an empty memory.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The CPU's view of an allocation; `None` for a mapped dma-buf, whose CPU view the caller
    /// holds.
    pub fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        if !self.allocated {
            return None;
        }
        // SAFETY: `cpu` is the `len` bytes the kernel mapped for this allocation (checked
        // non-null when it was made); the allocation is unmapped only when `self` drops, and
        // `&mut self` excludes other access through it.
        Some(unsafe { std::slice::from_raw_parts_mut(self.cpu, self.len) })
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        let _ = if self.allocated {
            self.device.request(
                command::FREE,
                &mut Free {
                    memory_handle: self.handle,
                },
            )
        } else {
            self.device.request(
                command::UNMAP,
                &mut Unmap {
                    memory_handle: self.handle,
                },
            )
        };
    }
}

/// Writes the CPU's stores in `bytes` back to memory and invalidates the lines, before the GPU
/// reads them or the CPU reads what the GPU wrote. The vendor's `thead_csi_dcache_clean_invalid_range`
/// on the C910 (`th.dcache.civa` per 64-byte line, between fences). Nothing happens off riscv64.
pub fn clean_invalidate_dcache(bytes: &[u8]) {
    #[cfg(target_arch = "riscv64")]
    {
        let mut line = bytes.as_ptr() as usize;
        let mut remaining = bytes.len() as i64 + (line % 64) as i64;
        // SAFETY: a fence orders memory accesses and touches no memory.
        unsafe { core::arch::asm!("fence iorw, iorw") };
        while remaining > 0 {
            // SAFETY: cache maintenance on the line holding `line`, inside memory `bytes`
            // borrows. It writes no memory and is the C910's `th.dcache.civa a0`
            // (XTheadCmo): encoding 0x0275000b, as the earlier crate transcribed it (unverified
            // on board).
            unsafe { core::arch::asm!(".long 0x0275000b", in("a0") line) };
            line += 64;
            remaining -= 64;
        }
        // SAFETY: fences, as the vendor's helper ends with.
        unsafe { core::arch::asm!("fence iorw, iorw", "fence.i", "fence r, r") };
    }
    #[cfg(not(target_arch = "riscv64"))]
    let _ = bytes;
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn numbers_match_the_header() {
        assert_eq!(IOCTL, 30_000);
        assert_eq!(
            [
                command::INITIALIZE,
                command::TERMINATE,
                command::ALLOCATE,
                command::FREE,
                command::SUBMIT,
                command::WAIT,
                command::MAP,
                command::UNMAP,
                command::CHECK,
            ],
            [0, 1, 2, 3, 4, 5, 8, 9, 10]
        );
        assert_eq!(error::SUCCESS, 0);
        assert_eq!(error::NOT_SUPPORT, 7);
        assert_eq!(error::SYSTEM_CALL_FAIL, 12);
        assert_eq!(MAP_DMABUF, 0x4);
        assert_eq!(CMDBUF_COUNT, 2);
        assert_eq!(INFINITE, 0xFFFF_FFFF);
        assert_eq!(WINDOW_BYTES, 0x0200_0000);
    }

    #[test]
    fn envelope_matches_the_header() {
        assert_eq!(size_of::<IoctlData>(), 24);
        assert_eq!(offset_of!(IoctlData, error), 4);
        assert_eq!(offset_of!(IoctlData, bytes), 8);
        assert_eq!(offset_of!(IoctlData, buffer), 16);
    }

    #[test]
    fn initialize_matches_the_header() {
        assert_eq!(size_of::<Initialize>(), 88);
        assert_eq!(offset_of!(Initialize, tess_height), 8);
        assert_eq!(offset_of!(Initialize, context), 16);
        assert_eq!(offset_of!(Initialize, capabilities), 24);
        assert_eq!(offset_of!(Initialize, command_buffer), 32);
        assert_eq!(offset_of!(Initialize, command_buffer_gpu), 48);
        assert_eq!(offset_of!(Initialize, physical_addr), 56);
        assert_eq!(offset_of!(Initialize, logical_addr), 64);
        assert_eq!(offset_of!(Initialize, tessbuf_size), 72);
        assert_eq!(offset_of!(Initialize, countbuf_size), 76);
        assert_eq!(offset_of!(Initialize, tess_w_h), 80);
    }

    #[test]
    fn kernel_context_matches_the_header() {
        // `sizeof (struct vg_lite_kernel_context)` on LP64, as the host C compiler gives it.
        assert_eq!(size_of::<KernelContext>(), 128);
        assert_eq!(offset_of!(KernelContext, command_buffer_physical), 48);
        assert_eq!(offset_of!(KernelContext, tess_buffer), 56);
        assert_eq!(offset_of!(KernelContext, tessbuf_physical), 80);
        assert_eq!(offset_of!(KernelContext, power_context), 88);
        assert_eq!(offset_of!(KernelContext, power_context_physical), 112);
        assert_eq!(offset_of!(KernelContext, power_context_capacity), 120);
    }

    #[test]
    fn memory_commands_match_the_header() {
        assert_eq!(size_of::<Allocate>(), 48);
        assert_eq!(offset_of!(Allocate, contiguous), 4);
        assert_eq!(offset_of!(Allocate, memory_handle), 16);
        assert_eq!(offset_of!(Allocate, kmemory), 32);
        assert_eq!(offset_of!(Allocate, memory_gpu), 40);
        assert_eq!(size_of::<Free>(), 8);
        assert_eq!(size_of::<Map>(), 40);
        assert_eq!(offset_of!(Map, logical), 8);
        assert_eq!(offset_of!(Map, physical), 16);
        assert_eq!(offset_of!(Map, dma_buf_fd), 20);
        assert_eq!(offset_of!(Map, memory_handle), 24);
        assert_eq!(offset_of!(Map, memory_gpu), 32);
        assert_eq!(size_of::<Unmap>(), 8);
    }

    #[test]
    fn submit_and_wait_match_the_header() {
        assert_eq!(size_of::<Submit>(), 24);
        assert_eq!(offset_of!(Submit, commands), 8);
        assert_eq!(offset_of!(Submit, command_size), 16);
        assert_eq!(offset_of!(Submit, command_id), 20);
        assert_eq!(size_of::<Wait>(), 24);
        assert_eq!(offset_of!(Wait, timeout_ms), 8);
        assert_eq!(offset_of!(Wait, event_mask), 12);
        assert_eq!(offset_of!(Wait, event_got), 16);
    }

    #[test]
    fn terminate_and_info_match_the_header() {
        assert_eq!(size_of::<Terminate>(), 8);
        assert_eq!(size_of::<Info>(), 8);
        assert_eq!(offset_of!(Info, reg), 4);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn tess() -> Tessellation {
        Tessellation {
            physical: 0x4000_0000,
            size: 0x1_0000,
            count_size: 0x1000,
            tess_w_h: 0,
        }
    }

    fn rgba(width: u32, height: u32, stride: u32) -> Layout {
        Layout {
            bytes_per_pixel: 4,
            width,
            height,
            stride,
        }
    }

    fn refused<T: std::fmt::Debug>(r: io::Result<T>) -> io::ErrorKind {
        r.expect_err("must be refused").kind()
    }

    #[test]
    fn raw_words_reach_only_plain_registers_and_the_flush() {
        let mut s = Stream::new(1 << 17, tess());
        // A plain register with any value, the colour key zero, and the flush are allowed.
        s.push_raw(&[STATE | 0x0A00, 0x1000_1001]).unwrap();
        s.push_raw(&[STATE | 0x0A90, 0]).unwrap();
        s.push_raw(&[SEMAPHORE_7, 0, STALL_7, 0]).unwrap();
        // An address register is refused: a raw address is never accepted.
        for address in [
            0x0A11, 0x0A29, 0x0A35, 0x0AC8, 0x0ACB, 0x0A12, 0x0A2F, 0x0B00,
        ] {
            assert_eq!(
                refused(s.push_raw(&[STATE | address, 0x8000_0000])),
                io::ErrorKind::InvalidInput,
                "{address:#x}"
            );
        }
        // A zero register takes only zero.
        assert_eq!(
            refused(s.push_raw(&[STATE | 0x0A34, 1])),
            io::ErrorKind::InvalidInput
        );
        s.push_raw(&[STATE | 0x0A34, 0]).unwrap();
        // An unknown register, and an unknown opcode, are refused.
        assert!(s.push_raw(&[STATE | 0x0ACC, 0]).is_err());
        assert!(s.push_raw(&[0x5000_0000, 0]).is_err());
        // Data headers, colour-table runs and the end command are not raw.
        assert!(s.push_raw(&[DATA_1, 0, 0, 0]).is_err());
        assert!(s.push_raw(&[CLUT_HEADER, 0]).is_err());
        assert!(s.push_raw(&[0, 0]).is_err());
        // A truncated state write, and a flush with a wrong tail, are refused.
        assert!(s.push_raw(&[STATE | 0x0A00]).is_err());
        assert!(s.push_raw(&[SEMAPHORE_7, 0, STALL_7, 1]).is_err());
    }

    #[test]
    fn a_refused_raw_slice_adds_nothing() {
        let mut s = Stream::new(1 << 17, tess());
        assert!(s.push_raw(&[STATE | 0x0A00, 1, STATE | 0x0A11, 2]).is_err());
        assert!(s.words().is_empty());
    }

    #[test]
    fn addresses_come_from_buffers_and_their_extent_is_checked() {
        let mut s = Stream::new(1 << 17, tess());
        let small = Scratch::new(0x8000_0000, 255);
        // 64 bytes a row times 4 rows is 256: one byte short of the buffer.
        assert_eq!(
            refused(s.render_target(&small, 0x123, rgba(16, 4, 64))),
            io::ErrorKind::InvalidInput
        );
        let exact = Scratch::new(0x8000_0000, 256);
        // A stride that holds no row, a zero dimension, and a span past the GPU address space.
        assert!(s.render_target(&exact, 0x123, rgba(16, 4, 60)).is_err());
        assert!(s.render_target(&exact, 0x123, rgba(0, 4, 64)).is_err());
        let high = Scratch::new(0xFFFF_FF01, 1 << 20);
        assert!(s.source_surface(&high, rgba(16, 4, 64)).is_err());
        s.render_target(&exact, 0x123, rgba(16, 4, 64)).unwrap();
        assert_eq!(
            &s.words()[..8],
            &[
                STATE | 0x0A10,
                0x123,
                STATE | 0x0A11,
                0x8000_0000,
                STATE | 0x0A12,
                64,
                STATE | 0x0A13,
                16 | (4 << 16),
            ]
        );
    }

    #[test]
    fn rectangles_and_clears_stay_inside_the_target() {
        let mut s = Stream::new(1 << 17, tess());
        assert!(s.rectangle(0, 0, 1, 1).is_err(), "no target yet");
        let target = Scratch::new(0x8000_0000, 256);
        s.render_target(&target, 0x123, rgba(16, 4, 64)).unwrap();
        s.rectangle(1, 1, 3, 2).unwrap();
        assert!(s.rectangle(14, 0, 3, 1).is_err(), "runs past the width");
        assert!(s.rectangle(0, 3, 1, 2).is_err(), "runs past the height");
        assert!(s.rectangle(0, 0, 0, 1).is_err(), "empty");
        s.pe_clear(256).unwrap();
        assert!(s.pe_clear(257).is_err(), "past the target's extent");
    }

    #[test]
    fn colour_table_and_tessellation_are_fixed_addresses() {
        let mut s = Stream::new(1 << 17, tess());
        s.tessellation_setup().unwrap();
        assert_eq!(
            s.words(),
            &[
                STATE | 0x0A35,
                0x4000_0000,
                STATE | 0x0AC8,
                0x1_0000,
                STATE | 0x0ACB,
                0x4001_0000,
                STATE | 0x0ACC,
                0x1000,
            ]
        );
        assert!(s.clut(&[0; 16]).is_err(), "only 256 entries");
        let mut t = Stream::new(1 << 17, tess());
        t.clut(&[7; 256]).unwrap();
        assert_eq!(t.words()[0], CLUT_HEADER);
        assert_eq!(t.words()[257], NOP);
    }

    #[test]
    fn only_finished_kernel_streams_are_submittable() {
        let mut s: Stream<'_> = Stream::new(1 << 17, tess());
        assert!(s.submittable(1 << 17).is_err(), "not finished");
        s.finish().unwrap();
        s.submittable(1 << 17).unwrap();
        assert!(s.submittable(8).is_err(), "larger than the command buffer");
        assert!(s.finish().is_err(), "nothing may follow the end");
        assert!(s.plain(0x0A00, 0).is_err(), "nothing may follow the end");

        let mut scratch = Stream::new(1 << 17, tess());
        let buffer = Scratch::new(0x8000_0000, 256);
        scratch
            .render_target(&buffer, 0x123, rgba(16, 4, 64))
            .unwrap();
        scratch.finish().unwrap();
        assert!(
            scratch.submittable(1 << 17).is_err(),
            "scratch memory is refused"
        );
    }

    #[test]
    fn plain_writes_refuse_addresses_and_identification_has_three_registers() {
        let mut s = Stream::new(1 << 17, tess());
        assert!(s.plain(0x0A29, 0).is_err());
        assert!(s.zero(0x0A11).is_err());
        assert_eq!(Identification::ChipId.address(), 0x20);
        assert_eq!(Identification::Revision.address(), 0x24);
        assert_eq!(Identification::Cid.address(), 0x30);
    }
}
