//! The memory the main path writes frames into, and the planes laid out in it.
//!
//! The memory interface takes 32-bit bus addresses and the block has no MMU in front of it. An
//! address it is given is never a caller's number: every base address [`Capture`](crate::Capture)
//! writes is the address of a [`FrameMemory`] plus a plane's offset, and a [`FrameMemory`] is made
//! only by [`FrameMemory::allocate`] from the host's DMA memory ([`DmaBuffers`], whose
//! [`DmaBuffer::device_address`](lemnos_hal::blocks::DmaBuffer) is "the address as the block sees
//! it"). That allocation is the one thing trusted: the host made it and it lives until
//! [`Capture`](crate::Capture) gives it back.
//!
//! What is checked, overflow-safe, before any of it reaches a register:
//!
//! - the memory: its address is 16-byte aligned (the base fields drop the low four bits) and the
//!   whole of it lies below 4 GiB (the fields are 32 bits), and the host gave at least what was
//!   asked;
//! - each plane ([`Planes::new`]): offset and size are 16-byte multiples (the fields' granularity),
//!   the size is not zero and fits the size field, and no two planes overlap;
//! - the planes against the memory ([`Planes::end`] against [`FrameMemory::len`]), for every
//!   memory in the ring, when the path is started.

use lemnos_hal::blocks::DmaBuffers;

use crate::regs::{BASE_MASK, SIZE_MASK};

/// The alignment of every base and size the memory interface takes.
pub const ALIGN: u32 = 16;

/// Why memory or planes were refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryError {
    /// The host gave less than was asked.
    Short,
    /// The host's address is not 16-byte aligned.
    Unaligned,
    /// Some of the memory lies at or past 4 GiB, which the 32-bit fields cannot name.
    Unreachable,
    /// A plane's offset or size is not a 16-byte multiple, is zero, or is past the size field.
    Plane,
    /// Two planes overlap.
    Overlap,
    /// The planes reach past the memory.
    Outside,
}

/// DMA memory the main path may write: one allocation from the host, checked. Made only by
/// [`FrameMemory::allocate`]; its fields are private, so no other address can be put in one.
#[derive(Debug, PartialEq, Eq)]
pub struct FrameMemory {
    id: u32,
    len: u64,
    address: u32,
}

impl FrameMemory {
    /// Allocates `len` bytes from `dma` and checks what came back. Memory that fails the check is
    /// given back before the error returns. `Err(Ok(e))` is the host's error; `Err(Err(e))` the
    /// check's.
    pub fn allocate<D: DmaBuffers + ?Sized>(
        dma: &mut D,
        len: u64,
    ) -> Result<Self, Result<D::Error, MemoryError>> {
        let buffer = dma.allocate(len).map_err(Ok)?;
        let checked = if buffer.len < len {
            Err(MemoryError::Short)
        } else if buffer.device_address % u64::from(ALIGN) != 0 {
            Err(MemoryError::Unaligned)
        } else {
            match buffer.device_address.checked_add(buffer.len) {
                Some(end) if end <= 1 << 32 => {
                    u32::try_from(buffer.device_address).map_err(|_| MemoryError::Unreachable)
                }
                _ => Err(MemoryError::Unreachable),
            }
        };
        match checked {
            Ok(address) => Ok(Self {
                id: buffer.id,
                len: buffer.len,
                address,
            }),
            Err(e) => {
                // Given back on a best-effort basis: the error is the check's either way.
                let _ = dma.release(buffer.id);
                Err(Err(e))
            }
        }
    }

    /// The host's id for the memory, for reading it back.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// Bytes the memory holds.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Whether the memory holds nothing (never, for memory [`FrameMemory::allocate`] made with a
    /// length above zero).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bus address of byte `offset`, for a plane already checked against the memory.
    pub(crate) const fn address(&self, offset: u32) -> u32 {
        // In range: the plane lies inside the memory, and the memory below 4 GiB.
        self.address + offset
    }
}

/// One plane: its offset in a [`FrameMemory`] and its size, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plane {
    /// Byte offset of the plane's first byte.
    pub offset: u32,
    /// Bytes in the plane: where the path's write wraps.
    pub size: u32,
}

impl Plane {
    /// One past the plane's last byte.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.offset as u64 + self.size as u64
    }

    const fn valid(&self) -> bool {
        self.size != 0
            && self.offset.is_multiple_of(ALIGN)
            && self.size & !SIZE_MASK == 0
            && self.offset & !BASE_MASK == 0
    }

    const fn overlaps(&self, other: &Self) -> bool {
        self.end() > other.offset as u64 && other.end() > self.offset as u64
    }
}

/// The planes of one frame, checked against each other. The first is the Y plane (or the only
/// plane, or the raw plane); then the Cb (or interleaved CbCr) plane and the Cr plane, if the
/// format has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planes {
    y: Plane,
    cb: Option<Plane>,
    cr: Option<Plane>,
}

impl Planes {
    /// The planes, if each is valid and no two overlap.
    pub fn new(y: Plane, cb: Option<Plane>, cr: Option<Plane>) -> Result<Self, MemoryError> {
        for p in [Some(y), cb, cr].into_iter().flatten() {
            if !p.valid() {
                return Err(MemoryError::Plane);
            }
        }
        let overlap = cb.is_some_and(|cb| cb.overlaps(&y))
            || cr.is_some_and(|cr| cr.overlaps(&y))
            || matches!((cb, cr), (Some(cb), Some(cr)) if cb.overlaps(&cr));
        if overlap {
            return Err(MemoryError::Overlap);
        }
        Ok(Self { y, cb, cr })
    }

    /// The Y (or only) plane.
    #[must_use]
    pub const fn y(&self) -> Plane {
        self.y
    }

    /// The Cb (or CbCr) plane, or the Y plane when there is none: what the path's Cb registers
    /// are pointed at, so that a register left in use by the block can only reach this memory.
    #[must_use]
    pub fn cb_or_y(&self) -> Plane {
        self.cb.unwrap_or(self.y)
    }

    /// The Cr plane, or the Y plane when there is none.
    #[must_use]
    pub fn cr_or_y(&self) -> Plane {
        self.cr.unwrap_or(self.y)
    }

    /// One past the last byte of any plane.
    #[must_use]
    pub fn end(&self) -> u64 {
        [Some(self.y), self.cb, self.cr]
            .into_iter()
            .flatten()
            .map(|p| p.end())
            .max()
            .unwrap_or(0)
    }

    /// Whether every plane lies inside `memory`.
    #[must_use]
    pub fn fits(&self, memory: &FrameMemory) -> bool {
        self.end() <= memory.len()
    }
}
