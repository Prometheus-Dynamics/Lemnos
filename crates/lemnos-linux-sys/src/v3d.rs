//! The v3d uAPI calls a buffer-copy job needs: GEM buffers in the GPU's address
//! space, their CPU mappings, and one texture-formatting (TFU) job. Only that
//! job is wrapped: it needs no shader, so no compiler is involved.
//!
//! Addresses are in the V3D address space the descriptor's buffers share. A
//! buffer's [`CreatedBo::offset`] is its base there; a TFU job names addresses
//! inside those buffers. The kernel checks nothing about a TFU's addresses
//! beyond the buffers it is given, and the GPU's MMU faults on any access
//! outside a buffer.

use crate::ioctl::{ioctl_ptr, iowr};
use std::io;
use std::os::fd::BorrowedFd;

/// `struct drm_v3d_create_bo`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CreateBo {
    size: u32,
    flags: u32,
    handle: u32,
    offset: u32,
}

/// `struct drm_v3d_mmap_bo`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct MmapBo {
    handle: u32,
    flags: u32,
    offset: u64,
}

/// `struct drm_v3d_submit_tfu`, as the kernel reads it (68 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmitTfu {
    /// Input and output configuration (format and type fields).
    pub icfg: u32,
    /// Input image address (V3D address space).
    pub iia: u32,
    /// Input image stride, in bytes.
    pub iis: u32,
    /// Chroma input address (unused for one plane).
    pub ica: u32,
    /// Second input address (unused for one plane).
    pub iua: u32,
    /// Output image address (V3D address space).
    pub ioa: u32,
    /// Output size: `height << 16 | width`.
    pub ios: u32,
    /// Filter coefficients (unused for a copy).
    pub coef: [u32; 4],
    /// GEM handles: the output buffer first, then inputs, zero for unused.
    pub bo_handles: [u32; 4],
    /// Sync object the job waits on before it runs, zero for none.
    pub in_sync: u32,
    /// Sync object signalled when the job is done, zero for none.
    pub out_sync: u32,
}

/// `DRM_IOCTL_V3D_CREATE_BO`.
const CREATE_BO: u32 = iowr::<CreateBo>(b'd', 0x42);
/// `DRM_IOCTL_V3D_MMAP_BO`.
const MMAP_BO: u32 = iowr::<MmapBo>(b'd', 0x43);
/// `DRM_IOCTL_V3D_SUBMIT_TFU`: write-only in the header (the kernel does not
/// write the struct back).
const SUBMIT_TFU: u32 = crate::ioctl::iow::<SubmitTfu>(b'd', 0x46);

/// A GEM buffer made by [`create_bo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreatedBo {
    /// The GEM handle, valid on the descriptor that made it.
    pub handle: u32,
    /// The buffer's base address in the V3D address space (never zero).
    pub offset: u32,
}

/// Creates a GEM buffer of `size` bytes (`DRM_IOCTL_V3D_CREATE_BO`). The kernel
/// rounds the size up to whole pages.
pub fn create_bo(fd: BorrowedFd<'_>, size: u32) -> io::Result<CreatedBo> {
    let mut create = CreateBo {
        size,
        ..CreateBo::default()
    };
    // SAFETY: `CREATE_BO` takes a `drm_v3d_create_bo`, which `create` is; the
    // kernel writes its `handle` and `offset` fields.
    unsafe { ioctl_ptr(fd, CREATE_BO, &raw mut create)? };
    Ok(CreatedBo {
        handle: create.handle,
        offset: create.offset,
    })
}

/// The offset to pass to `mmap` on this descriptor to map a buffer
/// (`DRM_IOCTL_V3D_MMAP_BO`).
pub fn mmap_offset(fd: BorrowedFd<'_>, handle: u32) -> io::Result<u64> {
    let mut mmap = MmapBo {
        handle,
        ..MmapBo::default()
    };
    // SAFETY: `MMAP_BO` takes a `drm_v3d_mmap_bo`, which `mmap` is; the kernel
    // writes its `offset` field.
    unsafe { ioctl_ptr(fd, MMAP_BO, &raw mut mmap)? };
    Ok(mmap.offset)
}

/// Submits one TFU job (`DRM_IOCTL_V3D_SUBMIT_TFU`). It returns once the job is
/// queued; `out_sync`, if set, signals when it finishes.
pub fn submit_tfu(fd: BorrowedFd<'_>, job: &SubmitTfu) -> io::Result<()> {
    let mut job = *job;
    // SAFETY: `SUBMIT_TFU` takes a `drm_v3d_submit_tfu`, which `job` is. The
    // kernel reads it; the handles and addresses it names are the caller's
    // buffers on this descriptor.
    unsafe { ioctl_ptr(fd, SUBMIT_TFU, &raw mut job)? };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structs_match_the_kernel_layout() {
        assert_eq!(size_of::<CreateBo>(), 16);
        assert_eq!(size_of::<MmapBo>(), 16);
        assert_eq!(size_of::<SubmitTfu>(), 68);
        assert_eq!(std::mem::offset_of!(SubmitTfu, bo_handles), 44);
        assert_eq!(std::mem::offset_of!(SubmitTfu, in_sync), 60);
    }

    #[test]
    fn requests_encode_as_the_kernel_numbers() {
        assert_eq!(CREATE_BO, 0xC010_6442);
        assert_eq!(MMAP_BO, 0xC010_6443);
        assert_eq!(SUBMIT_TFU, 0x4044_6446);
    }
}
