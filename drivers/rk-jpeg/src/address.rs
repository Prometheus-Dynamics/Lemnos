//! The address side of a JPEG job: which dma-buf, at which offset, each address register names,
//! checked against what the caller holds.
//!
//! # Where a caller's numbers become addresses
//!
//! The vendor kernel turns each register of the block's address table into the bus address of
//! the dma-buf the register names, then adds every offset of the job's `CMD_SET_REG_ADDR_OFFSET`
//! list to the register that list entry names. It checks neither the offset against the dma-buf
//! nor the entry's register against its register file. So here:
//!
//! - **Descriptors.** Only an [`Addresses`] writes one, only from a [`Buffer`], and only into a
//!   register of the variant's address table ([`Variant::address_registers`]). Descriptor 0 is
//!   refused (the kernel reads it as no buffer and leaves the raw value). The descriptors stay
//!   borrowed until the [`Addresses`] is dropped. [`Addresses::fill`] also zeroes every address
//!   register not placed, so no number the caller put there survives.
//! - **Offsets.** Each goes in the offset list keyed by its own address register, at most once per
//!   register (the kernel adds every entry, so a repeated one would add twice), and is at most
//!   `u32::MAX`. The call is to carry `FLAG_REG_NO_OFFSET`, so the descriptor fills its register
//!   whole and no offset rides in its high bits.
//! - **Extents.** `offset + extent` is checked against the dma-buf's size, in `u64` with overflow
//!   refused. The extent is the bytes the block may reach from that address. It is the caller's
//!   figure here: the registers that fix it (sizes, strides, formats, stream lengths) are not
//!   named in the vendor kernel's source. The decoder builds no job on that account. The encoder's
//!   job (`lemnos-driver-rkjpege`, see [`crate::encoder`]) checks its planes against the encoded
//!   picture, which its header fields give.
//! - **The encoder's link table** (register 9) is refused: in link mode the block fetches register
//!   lists from memory, which would let a buffer's contents program addresses.
//! - **Register reads** ([`read_window`]) stay inside the variant's register file: the vendor
//!   driver's own bound check miscomputes the size of a read that runs past the end.

use std::fmt;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::mpp::{CMD_SET_REG_READ, FLAG_MULTI_MSG, FLAG_REG_NO_OFFSET, Msg, OffsetElem};
use crate::regs::{Variant, enc};

/// The most address registers a variant has.
pub const MAX_ADDRESSES: usize = enc::ADDRESS_REGISTERS.len();

/// A dma-buf the caller holds, with the size the kernel gives it.
#[derive(Clone, Copy, Debug)]
pub struct Buffer<'a> {
    fd: BorrowedFd<'a>,
    len: u64,
}

impl<'a> Buffer<'a> {
    /// The dma-buf `fd`, measured. Descriptor 0 is refused. A descriptor that is not a dma-buf is
    /// refused by the kernel when a job names it.
    pub fn new(fd: BorrowedFd<'a>) -> std::io::Result<Self> {
        let len = lemnos_linux_sys::dma_buf::size(fd)?;
        Self::with_len(fd, len)
    }

    /// `fd` taken to be `len` bytes. Only tests and [`Self::new`] make one.
    pub(crate) fn with_len(fd: BorrowedFd<'a>, len: u64) -> std::io::Result<Self> {
        if fd.as_raw_fd() <= 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "descriptor 0 reads as no buffer to the kernel",
            ));
        }
        Ok(Self { fd, len })
    }

    /// The size in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the dma-buf is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn word(&self) -> u32 {
        // Positive, checked when made.
        self.fd.as_raw_fd().unsigned_abs()
    }
}

/// Where something starts: a dma-buf and a byte offset into it.
#[derive(Clone, Copy, Debug)]
pub struct At<'a> {
    /// The dma-buf.
    pub buffer: Buffer<'a>,
    /// Bytes from its start.
    pub offset: u64,
}

impl<'a> At<'a> {
    /// `offset` bytes into `buffer`.
    #[must_use]
    pub fn new(buffer: Buffer<'a>, offset: u64) -> Self {
        Self { buffer, offset }
    }
}

/// Why a placement was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// The register is not in the variant's address table.
    NotAnAddress(u32),
    /// The encoder's link table, which is never placed.
    LinkTable,
    /// The register was placed already.
    Twice(u32),
    /// An extent of zero.
    Empty(u32),
    /// The extent runs past the dma-buf, or the offset is past `u32::MAX`.
    OutOfBuffer {
        /// The register.
        register: u32,
        /// The byte it would end at, or `u64::MAX` if that overflows.
        end: u64,
        /// The dma-buf's size.
        len: u64,
    },
}

impl fmt::Display for PlaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnAddress(r) => write!(f, "register {r} is not an address"),
            Self::LinkTable => f.write_str("the link table is never placed"),
            Self::Twice(r) => write!(f, "register {r} placed twice"),
            Self::Empty(r) => write!(f, "register {r} given no extent"),
            Self::OutOfBuffer { register, end, len } => {
                write!(
                    f,
                    "register {register} reaches {end}, past its {len}-byte dma-buf"
                )
            }
        }
    }
}

impl std::error::Error for PlaceError {}

/// A job's address registers, each a checked place in a dma-buf the caller holds.
#[derive(Clone, Debug)]
pub struct Addresses<'a> {
    variant: Variant,
    words: [(u32, u32); MAX_ADDRESSES],
    offsets: [OffsetElem; MAX_ADDRESSES],
    count: usize,
    fds: PhantomData<BorrowedFd<'a>>,
}

impl<'a> Addresses<'a> {
    /// No address placed, for `variant`.
    #[must_use]
    pub fn new(variant: Variant) -> Self {
        Self {
            variant,
            words: [(0, 0); MAX_ADDRESSES],
            offsets: [OffsetElem::default(); MAX_ADDRESSES],
            count: 0,
            fds: PhantomData,
        }
    }

    /// Places `register` at `at`, with the block reaching `extent` bytes from there.
    pub fn place(&mut self, register: u32, at: At<'a>, extent: u64) -> Result<(), PlaceError> {
        if !self.variant.address_registers().contains(&register) {
            return Err(PlaceError::NotAnAddress(register));
        }
        if self.variant.device_type() == crate::mpp::DEVICE_RKJPEGE && register == enc::LINK_TABLE {
            return Err(PlaceError::LinkTable);
        }
        if self.words[..self.count].iter().any(|&(r, _)| r == register) {
            return Err(PlaceError::Twice(register));
        }
        if extent == 0 {
            return Err(PlaceError::Empty(register));
        }
        let len = at.buffer.len();
        let end = at.offset.checked_add(extent);
        let offset = u32::try_from(at.offset);
        let (Some(end), Ok(offset)) = (end, offset) else {
            return Err(PlaceError::OutOfBuffer {
                register,
                end: end.unwrap_or(u64::MAX),
                len,
            });
        };
        if end > len {
            return Err(PlaceError::OutOfBuffer { register, end, len });
        }
        // At most one entry per address register, and the table has at most MAX_ADDRESSES.
        self.words[self.count] = (register, at.buffer.word());
        self.offsets[self.count] = OffsetElem {
            index: register,
            offset,
        };
        self.count += 1;
        Ok(())
    }

    /// The variant.
    #[must_use]
    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// The placed registers and their descriptors.
    #[must_use]
    pub fn words(&self) -> &[(u32, u32)] {
        &self.words[..self.count]
    }

    /// The offset list, one entry per placed register.
    #[must_use]
    pub fn offsets(&self) -> &[OffsetElem] {
        &self.offsets[..self.count]
    }

    /// Writes the descriptors into a register file of the variant's size, and zero into every
    /// address register not placed. `false`, writing nothing, if `file` is not that size.
    pub fn fill(&self, file: &mut [u32]) -> bool {
        if file.len() != self.variant.registers() as usize {
            return false;
        }
        for &register in self.variant.address_registers() {
            file[register as usize] = 0;
        }
        for &(register, word) in self.words() {
            file[register as usize] = word;
        }
        true
    }
}

/// The message reading back `count` registers from `first`, or `None` if they are not all inside
/// the variant's register file.
#[must_use]
pub fn read_window(variant: Variant, first: u32, count: u32) -> Option<Msg> {
    let end = first.checked_add(count)?;
    if count == 0 || end > variant.registers() {
        return None;
    }
    Some(Msg {
        cmd: CMD_SET_REG_READ,
        flags: FLAG_MULTI_MSG | FLAG_REG_NO_OFFSET,
        size: count * 4,
        offset: first * 4,
        data_ptr: 0,
    })
}
