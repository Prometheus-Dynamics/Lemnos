//! The `/dev/vg_lite` kernel interface of the K230's VGLite 2.5D GPU: the generic request
//! (`VG_LITE_IOCTL`) and the command structures the kernel reads and writes, from
//! `vg_lite_kernel.h` and `linux/vg_lite_ioctl.h` of the K230 Linux SDK v0.6.1 (Vivante, MIT).
//!
//! These are numbers and layouts only. Several structures hold pointers the kernel follows
//! (a command buffer, a user mapping, a memory handle), so the calls that use them belong to
//! the owner of those pointers, not to a plain structure. Each layout is checked against the
//! header in the tests: the offsets and sizes are those a C compiler gives on LP64, the layout
//! of the K230's riscv64 Linux.

use std::ffi::c_void;

/// `VG_LITE_IOCTL`: the request number of the one generic command, not an `_IO` encoding.
pub const IOCTL: u32 = 30_000;

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
    /// Written back: the context pointer.
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
            ],
            [0, 1, 2, 3, 4, 5, 8, 9]
        );
        assert_eq!(error::SUCCESS, 0);
        assert_eq!(error::NOT_SUPPORT, 7);
        assert_eq!(error::SYSTEM_CALL_FAIL, 12);
        assert_eq!(MAP_DMABUF, 0x4);
        assert_eq!(CMDBUF_COUNT, 2);
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
}
