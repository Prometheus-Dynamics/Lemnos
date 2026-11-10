//! The framing a program is loaded in: a small header and a table of places to put buffer
//! addresses, in front of the bytes the block runs. The driver reads the header and the table;
//! it never reads, parses or changes the body other than at the places the table names.
//!
//! # Why a framing
//!
//! The kernel does not take a program. It takes a task array in a kernel-mapped object, the
//! number of tasks, and the address of a register command stream, and the block then follows the
//! addresses written in those commands. Those addresses are on the NPU side of the IOMMU and are
//! chosen by the kernel when an object is made, so they are not known when a program is
//! compiled. Something must write them in once the objects exist; the vendor's runtime does it by
//! reading the model. Here the caller says where, and the driver writes the address of the named
//! object (plus an offset) at each place. That is the whole of what the driver does to the body.
//! None of it is a safety check: whatever address a program ends up holding, the IOMMU domain the
//! job runs in maps only the program's own objects (see [`crate::claim`]).
//!
//! # Layout
//!
//! Little-endian throughout.
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 4 | [`MAGIC`] |
//! | 4 | 2 | version, [`VERSION`] |
//! | 6 | 2 | [`flags`] |
//! | 8 | 4 | tasks: entries of the kernel's task array at the start of the body, at least 1 |
//! | 12 | 2 | slots: buffers the program runs on, numbered from 0; a run needs every one attached |
//! | 14 | 2 | relocations: entries of the table that follows the header |
//! | 16 | 4 | body bytes, at least one task array long |
//! | 20 | 4 | scratch bytes: a zeroed object the driver makes for the program's own use, or 0 |
//! | 24 | 4 | task base: the body offset whose address the kernel writes to `PC_DMA_BASE_ADDR` |
//! | 28 | 4 | reserved, 0 |
//!
//! Then `relocations` entries of [`RELOCATION_BYTES`]:
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 4 | at: the body offset to write, a multiple of 4 |
//! | 4 | 2 | target: a slot, [`SCRATCH`] or [`BODY`] |
//! | 6 | 1 | width: 4 or 8 bytes |
//! | 7 | 1 | reserved, 0 |
//! | 8 | 4 | addend: bytes into the target, inside it |
//!
//! Then the body. A program is exactly header, table and body long.

use crate::uapi::TASK_BYTES;

/// The framing's first four bytes.
pub const MAGIC: [u8; 4] = *b"LNPU";

/// The only version.
pub const VERSION: u16 = 1;

/// Header bytes.
pub const HEADER_BYTES: usize = 32;

/// Bytes of one relocation.
pub const RELOCATION_BYTES: usize = 12;

/// Slots a program may use: the contract's `max_attached`.
pub const MAX_SLOTS: u16 = 32;

/// The largest body.
pub const MAX_BODY_BYTES: u32 = 256 << 20;

/// The largest scratch object.
pub const MAX_SCRATCH_BYTES: u32 = 256 << 20;

/// The largest program as loaded: a full table and the largest body.
pub const MAX_PROGRAM_BYTES: u32 =
    HEADER_BYTES as u32 + u16::MAX as u32 * RELOCATION_BYTES as u32 + MAX_BODY_BYTES;

/// Relocation target: the body itself.
pub const BODY: u16 = 0xffff;

/// Relocation target: the scratch object.
pub const SCRATCH: u16 = 0xfffe;

/// Header flags.
pub mod flags {
    /// Run with the kernel's `RKNPU_JOB_PINGPONG`.
    pub const PINGPONG: u16 = 1 << 0;
    /// Every flag known.
    pub const ALL: u16 = PINGPONG;
}

/// Why a program's framing was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramingError {
    /// Not [`MAGIC`], or not [`VERSION`].
    Magic,
    /// A flag or reserved field this version does not know.
    Reserved,
    /// No task, or more tasks than the body holds.
    Tasks,
    /// An empty body, or one past [`MAX_BODY_BYTES`].
    Body,
    /// Scratch past [`MAX_SCRATCH_BYTES`].
    Scratch,
    /// More than [`MAX_SLOTS`] slots.
    Slots,
    /// The task base is not inside the body.
    TaskBase,
    /// The program is not exactly header, table and body long.
    Length,
    /// A relocation's place, width, target or addend is not inside what it names.
    Relocation,
}

/// A checked header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// [`flags`].
    pub flags: u16,
    /// Task array entries, at least 1, all inside the body.
    pub tasks: u32,
    /// Slots, at most [`MAX_SLOTS`].
    pub slots: u16,
    /// Relocation entries.
    pub relocations: u16,
    /// Body bytes, at least 1 and at most [`MAX_BODY_BYTES`].
    pub body: u32,
    /// Scratch bytes, at most [`MAX_SCRATCH_BYTES`].
    pub scratch: u32,
    /// A body offset below `body`.
    pub task_base: u32,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    let mut b = [0; 2];
    if let Some(from) = bytes.get(at..at + 2) {
        b.copy_from_slice(from);
    }
    u16::from_le_bytes(b)
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut b = [0; 4];
    if let Some(from) = bytes.get(at..at + 4) {
        b.copy_from_slice(from);
    }
    u32::from_le_bytes(b)
}

impl Header {
    /// Checks the header of a program `total` bytes long.
    pub fn parse(bytes: &[u8; HEADER_BYTES], total: u32) -> Result<Self, FramingError> {
        if bytes[..4] != MAGIC || u16_at(bytes, 4) != VERSION {
            return Err(FramingError::Magic);
        }
        let header = Self {
            flags: u16_at(bytes, 6),
            tasks: u32_at(bytes, 8),
            slots: u16_at(bytes, 12),
            relocations: u16_at(bytes, 14),
            body: u32_at(bytes, 16),
            scratch: u32_at(bytes, 20),
            task_base: u32_at(bytes, 24),
        };
        if header.flags & !flags::ALL != 0 || u32_at(bytes, 28) != 0 {
            return Err(FramingError::Reserved);
        }
        if header.body == 0 || header.body > MAX_BODY_BYTES {
            return Err(FramingError::Body);
        }
        // The kernel reads `tasks` entries from the body's kernel mapping without checking them
        // against the object (`rknpu_job_subcore_commit_pc`), and writes the last one's status.
        if header.tasks == 0
            || u64::from(header.tasks) * u64::from(TASK_BYTES) > u64::from(header.body)
        {
            return Err(FramingError::Tasks);
        }
        if header.scratch > MAX_SCRATCH_BYTES {
            return Err(FramingError::Scratch);
        }
        if header.slots > MAX_SLOTS {
            return Err(FramingError::Slots);
        }
        if header.task_base >= header.body {
            return Err(FramingError::TaskBase);
        }
        let expected = HEADER_BYTES as u64
            + u64::from(header.relocations) * RELOCATION_BYTES as u64
            + u64::from(header.body);
        if u64::from(total) != expected {
            return Err(FramingError::Length);
        }
        Ok(header)
    }

    /// Where the relocation table starts in the program.
    #[must_use]
    pub const fn table_at(&self) -> usize {
        HEADER_BYTES
    }

    /// Bytes of the relocation table.
    #[must_use]
    pub const fn table_bytes(&self) -> usize {
        self.relocations as usize * RELOCATION_BYTES
    }

    /// Where the body starts in the program.
    #[must_use]
    pub const fn body_at(&self) -> usize {
        HEADER_BYTES + self.table_bytes()
    }

    /// Whether the program runs with the kernel's ping-pong bit.
    #[must_use]
    pub const fn pingpong(&self) -> bool {
        self.flags & flags::PINGPONG != 0
    }
}

/// What a relocation writes the address of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The body.
    Body,
    /// The scratch object.
    Scratch,
    /// The buffer attached at a slot.
    Slot(u16),
}

/// A checked relocation: `at` and `width` inside the body, the target one the program has, the
/// addend inside the body or scratch (a slot's addend is checked when a buffer is attached).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// Body offset written.
    pub at: u32,
    /// Bytes written: 4 or 8.
    pub width: u8,
    /// The object whose address is written.
    pub target: Target,
    /// Bytes into the target.
    pub addend: u32,
}

impl Relocation {
    /// Checks one table entry of a program with `header`.
    pub fn parse(bytes: &[u8; RELOCATION_BYTES], header: &Header) -> Result<Self, FramingError> {
        let at = u32_at(bytes, 0);
        let target = u16_at(bytes, 4);
        let width = bytes[6];
        let addend = u32_at(bytes, 8);
        if bytes[7] != 0 || !matches!(width, 4 | 8) || !at.is_multiple_of(4) {
            return Err(FramingError::Relocation);
        }
        if u64::from(at) + u64::from(width) > u64::from(header.body) {
            return Err(FramingError::Relocation);
        }
        let target = match target {
            BODY if addend < header.body => Target::Body,
            SCRATCH if addend < header.scratch => Target::Scratch,
            slot if slot < header.slots => Target::Slot(slot),
            _ => return Err(FramingError::Relocation),
        };
        Ok(Self {
            at,
            width,
            target,
            addend,
        })
    }

    /// Writes `base + addend` at the relocation's place in `body`. `false`, with nothing
    /// written, when the place is not in `body` or a 4-byte place cannot hold the address.
    pub fn apply(&self, body: &mut [u8], base: u64) -> bool {
        let Some(value) = base.checked_add(u64::from(self.addend)) else {
            return false;
        };
        let at = self.at as usize;
        match self.width {
            4 => match (u32::try_from(value), body.get_mut(at..at + 4)) {
                (Ok(value), Some(place)) => place.copy_from_slice(&value.to_le_bytes()),
                _ => return false,
            },
            _ => match body.get_mut(at..at + 8) {
                Some(place) => place.copy_from_slice(&value.to_le_bytes()),
                None => return false,
            },
        }
        true
    }
}
