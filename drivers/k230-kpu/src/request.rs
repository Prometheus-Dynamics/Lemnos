//! The memory zone requests, built and checked here.
//!
//! The memory zone node (`drivers/misc/canaan/mmz/mmz.c` in ruyisdk/linux-xuantie-kernel at
//! `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529`) is where the block's buffers would come from: it
//! answers physical addresses, which is what the block takes. Every field it takes from user
//! space, and the check here:
//!
//! - `MMZ_ALLOC_MEM` `length`: passed to `dma_alloc_coherent` unchecked. [`alloc`] refuses 0, a
//!   length that is not whole pages, and one past [`MAX_ALLOC`]; `user_virt_addr` is sent as 0.
//! - The answer (`kernel_virt_addr`, `mmz_phys`): taken by [`Allocated::new`] only when both are
//!   set, the physical address is page aligned, the range does not wrap, and the length is the
//!   one asked for.
//! - `MMZ_FREE_MEM` `kernel_virt_addr`, `mmz_phys`, `length`: freed as given, checked against
//!   nothing the kernel allocated, so a wrong one frees memory the caller never held. [`free`]
//!   builds the request only from an [`Allocated`], field for field.
//! - `mmap` offset and length: any physical range. [`map`] names only pages of an [`Allocated`].
//!
//! No run is offered (see [`crate::confine`]), so nothing in this crate sends these requests; they
//! are the checked half of what a run would need.

use crate::uapi::MmzInfo;

/// The page size the memory zone maps in.
pub const PAGE: u64 = 4096;

/// The largest allocation built here, in bytes.
pub const MAX_ALLOC: u64 = 1 << 30;

/// Why a request was not built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// No bytes.
    Empty,
    /// Not whole pages.
    Unaligned,
    /// Past the allocation, or past [`MAX_ALLOC`].
    TooLarge,
    /// The kernel's answer is not an allocation of what was asked.
    Answer,
}

/// An allocation of `length` bytes.
pub fn alloc(length: u64) -> Result<MmzInfo, RequestError> {
    if length == 0 {
        return Err(RequestError::Empty);
    }
    if !length.is_multiple_of(PAGE) {
        return Err(RequestError::Unaligned);
    }
    if length > MAX_ALLOC {
        return Err(RequestError::TooLarge);
    }
    Ok(MmzInfo {
        length,
        ..MmzInfo::default()
    })
}

/// An allocation the kernel answered. Made only from an answer to [`alloc`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocated {
    info: MmzInfo,
}

impl Allocated {
    /// Takes the kernel's `answer` to the request `asked`.
    pub fn new(asked: &MmzInfo, answer: &MmzInfo) -> Result<Self, RequestError> {
        let sound = answer.length == asked.length
            && asked.length > 0
            && answer.kernel_virt_addr != 0
            && answer.mmz_phys != 0
            && answer.mmz_phys.is_multiple_of(PAGE)
            && answer.mmz_phys.checked_add(answer.length).is_some();
        if !sound {
            return Err(RequestError::Answer);
        }
        Ok(Self {
            info: MmzInfo {
                user_virt_addr: 0,
                ..*answer
            },
        })
    }

    /// The physical address.
    pub fn physical(&self) -> u64 {
        self.info.mmz_phys
    }

    /// The length, in bytes.
    pub fn length(&self) -> u64 {
        self.info.length
    }
}

/// The free of `allocated`, exactly as the kernel answered it.
pub fn free(allocated: &Allocated) -> MmzInfo {
    allocated.info
}

/// The `mmap` offset and length for `len` bytes from `offset` into `allocated`. Whole pages only.
pub fn map(allocated: &Allocated, offset: u64, len: u64) -> Result<(u64, u64), RequestError> {
    if len == 0 {
        return Err(RequestError::Empty);
    }
    if !offset.is_multiple_of(PAGE) || !len.is_multiple_of(PAGE) {
        return Err(RequestError::Unaligned);
    }
    let end = offset.checked_add(len).ok_or(RequestError::TooLarge)?;
    if end > allocated.length() {
        return Err(RequestError::TooLarge);
    }
    Ok((allocated.physical() + offset, len))
}
