//! A job: the register values the caller wants on the block, the registers that hold its buffers,
//! and the registers it wants back. The driver carries them and never reads what they mean; it
//! checks only what the kernel would turn into a descriptor, an address, an index or a length.
//!
//! # Where a caller's numbers become memory
//!
//! The kernel takes every one of these from the job without a bound of its own:
//!
//! - **Translated registers.** For each register in the session's translation table, the kernel
//!   takes the low 10 bits as a descriptor of the calling process, maps that dma-buf, and writes
//!   its device address plus the upper 22 bits (a byte offset) into the register. Without a table
//!   of the session's own it uses a built-in one per block and format. So a caller's raw value in
//!   such a register would name any dma-buf the process holds, at any offset. Here the session's
//!   table is sent with every job and lists exactly the job's [`Address`] registers; each of those
//!   holds the descriptor of a buffer the claim holds, placed by the driver, with offset bits 0.
//!   A job with no table address is refused, because an empty table leaves the built-in one in
//!   force. The table's indices are below the block's register count (the kernel indexes its
//!   register array with them unchecked).
//! - **Register offsets.** `MPP_CMD_SET_REG_ADDR_OFFSET` adds a byte offset to a register by
//!   index, both unchecked. Here an entry is made only for a job's [`Address`], with its register
//!   (inside the block, inside the write) and its offset, which is below the size of the buffer the
//!   claim measured.
//! - **Direct registers.** The decoders translate their motion vector base in H.264 mode whatever
//!   the table says ([`crate::Direct`]). When the job's mode register selects that, the register
//!   must be one of the job's addresses, and it is then left out of the table (the kernel would
//!   otherwise translate it twice).
//! - **Register reads and writes.** The kernel bounds them by its register array, which can be
//!   larger than the block (writes past the block's registers reach other memory-mapped
//!   registers), and when one overruns it shortens it wrongly. Here a write or read is a run of
//!   whole registers inside the block's register count and reaching its first register.
//! - **Counts.** At most [`MAX_ADDRESSES`] addresses, below the kernel's 80-entry arrays (whose
//!   region array check is off by one), and four messages per job, below its 16.
//!
//! # Parameter jobs
//!
//! A parameter block ([`crate::ParamFacts`]) takes a [`ParamJob`]: the whole parameter
//! structure, and the buffer for each word the kernel translates. The kernel copies a write of
//! any size into its fixed structure and a read of any size out of its results, translates its
//! fixed list of words with no table, takes the session's `MPP_FLAGS_REG_NO_OFFSET` to mean a
//! word is a whole descriptor whose offset is looked up by register in the job's offset list,
//! and then programs the block from the structure, using some words as indices, sizes and
//! counts. So:
//!
//! - **Descriptor words** are written by the driver, never taken from the caller: every one
//!   of them is first zeroed, and then holds the descriptor of a buffer the claim holds, named
//!   by the job, and a job must name one for every descriptor word (a word left 0 is skipped by
//!   the kernel and gives the block address 0). Each is named once.
//! - **Offsets** go in the offset list, keyed by the descriptor word's register, made only for
//!   the job's addresses, each below the size the claim measured. The messages carry
//!   `MPP_FLAGS_REG_NO_OFFSET`, so no offset rides in a descriptor word.
//! - **Sizes**: the write is exactly the structure's size, the read exactly the results'.
//! - **The caller's words** the kernel or the block takes as an index, a size, a count or a
//!   mode that changes what is reached are checked against the ranges the facts give; every
//!   other word is carried as it is.
//!
//! What the block then reads or writes from each address, how far, is set by the caller's other
//! registers, which the driver does not interpret. That is confined by the block's IOMMU: a claim
//! is taken only on a block behind one, and the claim is the only session on the service, so the
//! only buffers mapped for the block are the claim's own (see [`crate::Claim`]).

#[cfg(feature = "device")]
use crate::block::{Facts, MAX_REGISTERS, ParamFacts};
#[cfg(feature = "device")]
use crate::message::READ_BACK_BYTES;
#[cfg(feature = "device")]
use crate::uapi::{FD_MASK, RegOffsetElem};

/// The most addresses one job may name.
pub const MAX_ADDRESSES: usize = 64;

/// A buffer the claim holds, as [`crate::Claim::add_buffer`] named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferId {
    pub(crate) slot: u8,
    pub(crate) generation: u16,
}

/// A register that holds a buffer's device address: `offset` bytes into `buffer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    /// The register, by index; inside the job's write.
    pub register: u16,
    /// A buffer the claim holds.
    pub buffer: BufferId,
    /// Bytes into the buffer; below its size.
    pub offset: u32,
}

/// Registers the job wants back when it completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadBack {
    /// The first register.
    pub first: u16,
    /// How many.
    pub count: u16,
}

/// One task for the block.
#[derive(Clone, Copy, Debug)]
pub struct Job<'a> {
    /// The caller's name for the job. Carried to its completion, never read.
    pub identity: u64,
    /// The first register written.
    pub first: u16,
    /// The values written, from `first` on. An [`Address`] register's value is replaced by the
    /// driver.
    pub registers: &'a [u32],
    /// The registers that hold buffers.
    pub addresses: &'a [Address],
    /// The registers read back at completion, if any.
    pub read: Option<ReadBack>,
}

/// A word of a parameter job that holds a buffer's device address: `offset` bytes into
/// `buffer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamAddress {
    /// The word, by its byte offset in the parameter structure; one the kernel translates.
    pub word: u16,
    /// A buffer the claim holds.
    pub buffer: BufferId,
    /// Bytes into the buffer; below its size.
    pub offset: u32,
}

/// One task for a parameter block: its whole parameter structure, and the buffer each of its
/// descriptor words holds. The results structure is always read back.
#[derive(Clone, Copy, Debug)]
pub struct ParamJob<'a> {
    /// The caller's name for the job. Carried to its completion, never read.
    pub identity: u64,
    /// The structure, in the kernel's layout and the machine's byte order. Its descriptor words
    /// are replaced by the driver.
    pub params: &'a [u8],
    /// A buffer for every descriptor word.
    pub addresses: &'a [ParamAddress],
}

/// Why a job was refused. The device was not touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// The write is empty, or runs past the block's registers.
    WriteOutside,
    /// The write ends before the block's first register; the kernel would skip it.
    WriteBeforeFirst,
    /// The read-back is empty, or runs past the block's registers.
    ReadOutside,
    /// The read-back ends before the block's first register; the kernel would skip it.
    ReadBeforeFirst,
    /// No address goes through the translation table, which would leave the kernel's own in force.
    NoTableAddress,
    /// More than [`MAX_ADDRESSES`] addresses.
    TooManyAddresses,
    /// An address register is outside the job's write.
    AddressOutsideWrite {
        /// The register.
        register: u16,
    },
    /// Two addresses name the same register.
    DuplicateAddress {
        /// The register.
        register: u16,
    },
    /// An address names a buffer the claim does not hold.
    UnknownBuffer {
        /// The register.
        register: u16,
    },
    /// An address's offset is not inside its buffer.
    OffsetOutside {
        /// The register.
        register: u16,
        /// The offset asked for.
        offset: u32,
        /// The buffer's size.
        size: u64,
    },
    /// The job's mode makes the kernel translate this register, and it is not one of the job's
    /// addresses.
    DirectNotAddress {
        /// The register.
        register: u16,
    },
    /// A parameter job on a register-file block.
    NotParamBlock,
    /// The parameter structure is not the size the kernel's is.
    ParamsSize {
        /// Bytes given.
        len: usize,
    },
    /// An address names a word the kernel doesn't translate.
    NotDescriptor {
        /// The word's byte offset.
        word: u16,
    },
    /// A word the kernel translates has no buffer named for it.
    MissingDescriptor {
        /// The word's byte offset.
        word: u16,
    },
    /// A word the kernel or the block uses as an index, a size, a count or a mode is out of its
    /// range.
    Word {
        /// The word's byte offset.
        at: u16,
        /// Its value.
        value: u32,
    },
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for JobError {}

/// The claim's memory for building a job's messages, made once with the claim.
#[cfg(feature = "device")]
pub(crate) struct Staging {
    pub(crate) registers: [u8; 4 * MAX_REGISTERS],
    pub(crate) table: [u8; 2 * MAX_ADDRESSES],
    pub(crate) offsets: [u8; 8 * MAX_ADDRESSES],
    pub(crate) read_back: [u8; READ_BACK_BYTES],
}

#[cfg(feature = "device")]
impl Staging {
    pub(crate) fn new() -> Box<Self> {
        Box::new(Self {
            registers: [0; 4 * MAX_REGISTERS],
            table: [0; 2 * MAX_ADDRESSES],
            offsets: [0; 8 * MAX_ADDRESSES],
            read_back: [0; READ_BACK_BYTES],
        })
    }
}

/// A buffer as the plan sees it.
#[derive(Clone, Copy, Debug)]
#[cfg(feature = "device")]
pub(crate) struct Held {
    pub(crate) slot: u8,
    /// Between 1 and [`FD_MASK`].
    pub(crate) fd: u32,
    pub(crate) size: u64,
}

/// A checked job, staged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "device")]
pub(crate) struct Plan {
    /// Byte offset of the write in the register file, and its bytes in `Staging::registers`.
    pub(crate) write: (u32, usize, usize),
    /// Bytes of `Staging::table`.
    pub(crate) table: usize,
    /// Bytes of `Staging::offsets`.
    pub(crate) offsets: usize,
    /// Byte offset and bytes of the read-back.
    pub(crate) read: Option<(u32, usize)>,
    /// The claim's buffer slots the job uses.
    pub(crate) buffers: u64,
}

/// The run `first..first + count` inside `facts`' registers and reaching its first, as bytes.
#[cfg(feature = "device")]
fn run(facts: &Facts, first: u16, count: usize) -> Option<Result<(usize, usize), ()>> {
    let first = usize::from(first);
    let end = first.checked_add(count)?;
    if count == 0 || end > usize::from(facts.registers) {
        return None;
    }
    if end < usize::from(facts.first) {
        return Some(Err(()));
    }
    Some(Ok((first * 4, count * 4)))
}

/// Checks `job` against the block and the claim's buffers, and stages its messages' payloads.
#[cfg(feature = "device")]
pub(crate) fn plan(
    facts: &Facts,
    job: &Job<'_>,
    held: impl Fn(BufferId) -> Option<Held>,
    staging: &mut Staging,
) -> Result<Plan, JobError> {
    let (write_at, write_len) = match run(facts, job.first, job.registers.len()) {
        None => return Err(JobError::WriteOutside),
        Some(Err(())) => return Err(JobError::WriteBeforeFirst),
        Some(Ok(at)) => at,
    };
    let read = match job.read {
        None => None,
        Some(read) => match run(facts, read.first, usize::from(read.count)) {
            None => return Err(JobError::ReadOutside),
            Some(Err(())) => return Err(JobError::ReadBeforeFirst),
            Some(Ok((at, len))) => Some((at, len)),
        },
    };
    if job.addresses.len() > MAX_ADDRESSES {
        return Err(JobError::TooManyAddresses);
    }

    // The register file as the kernel will hold it: zero, then the write.
    let file = &mut staging.registers[..4 * usize::from(facts.registers)];
    file.fill(0);
    let (words, _) = file[write_at..write_at + write_len].as_chunks_mut::<4>();
    for (word, value) in words.iter_mut().zip(job.registers) {
        *word = value.to_ne_bytes();
    }

    // Each address: a register inside the write, named once, holding a buffer the claim holds at
    // an offset inside it. The register takes the descriptor with offset bits 0.
    let written = usize::from(job.first)..usize::from(job.first) + job.registers.len();
    let mut buffers = 0u64;
    for (i, address) in job.addresses.iter().enumerate() {
        let register = address.register;
        if !written.contains(&usize::from(register)) {
            return Err(JobError::AddressOutsideWrite { register });
        }
        if job.addresses[..i].iter().any(|a| a.register == register) {
            return Err(JobError::DuplicateAddress { register });
        }
        let buffer = held(address.buffer).ok_or(JobError::UnknownBuffer { register })?;
        if u64::from(address.offset) >= buffer.size {
            return Err(JobError::OffsetOutside {
                register,
                offset: address.offset,
                size: buffer.size,
            });
        }
        debug_assert!((1..=FD_MASK).contains(&buffer.fd));
        let at = 4 * usize::from(register);
        file[at..at + 4].copy_from_slice(&buffer.fd.to_ne_bytes());
        buffers |= 1 << buffer.slot;
    }

    // A register the kernel translates outside the table in this job's mode must be an address,
    // and stays out of the table.
    let mut direct = None;
    if let Some(d) = facts.direct {
        let at = 4 * usize::from(d.mode_register);
        let mode = u32::from_ne_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]);
        if d.applies(mode) {
            if !job.addresses.iter().any(|a| a.register == d.register) {
                return Err(JobError::DirectNotAddress {
                    register: d.register,
                });
            }
            direct = Some(d.register);
        }
    }

    let mut table = 0;
    let mut offsets = 0;
    for address in job.addresses {
        if Some(address.register) != direct {
            staging.table[table..table + 2].copy_from_slice(&address.register.to_ne_bytes());
            table += 2;
        }
        if address.offset != 0 {
            let elem = RegOffsetElem {
                index: u32::from(address.register),
                offset: address.offset,
            };
            staging.offsets[offsets..offsets + 8].copy_from_slice(&elem.to_ne_bytes());
            offsets += 8;
        }
    }
    if table == 0 {
        return Err(JobError::NoTableAddress);
    }

    Ok(Plan {
        write: (
            u32::try_from(write_at).unwrap_or(u32::MAX),
            write_at,
            write_len,
        ),
        table,
        offsets,
        read: read.map(|(at, len)| (u32::try_from(at).unwrap_or(u32::MAX), len)),
        buffers,
    })
}

/// The native-endian word at byte `at` of `bytes`.
#[cfg(feature = "device")]
fn word_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Checks a parameter job against the block and the claim's buffers, and stages its messages'
/// payloads: the structure with every descriptor word the driver's, and the offset list.
#[cfg(feature = "device")]
pub(crate) fn plan_params(
    facts: &ParamFacts,
    job: &ParamJob<'_>,
    held: impl Fn(BufferId) -> Option<Held>,
    staging: &mut Staging,
) -> Result<Plan, JobError> {
    if job.params.len() != facts.bytes || facts.bytes > staging.registers.len() {
        return Err(JobError::ParamsSize {
            len: job.params.len(),
        });
    }
    if job.addresses.len() > facts.descriptors.len() {
        return Err(JobError::TooManyAddresses);
    }
    let file = &mut staging.registers[..facts.bytes];
    file.copy_from_slice(job.params);

    // The caller's words the kernel or the block takes as an index, a size, a count or a mode.
    for word in facts.words {
        let value = word_at(file, usize::from(word.at));
        if !word.check.allows(value) {
            return Err(JobError::Word { at: word.at, value });
        }
    }

    // No caller's number stays in a word the kernel translates.
    for descriptor in facts.descriptors {
        let at = usize::from(descriptor.at);
        file[at..at + 4].fill(0);
    }

    // Each address: a descriptor word, named once, holding a buffer the claim holds at an
    // offset inside it.
    let mut buffers = 0u64;
    let mut offsets = 0;
    for (i, address) in job.addresses.iter().enumerate() {
        let descriptor = facts
            .descriptors
            .iter()
            .find(|d| d.at == address.word)
            .ok_or(JobError::NotDescriptor { word: address.word })?;
        let register = descriptor.register;
        if job.addresses[..i].iter().any(|a| a.word == address.word) {
            return Err(JobError::DuplicateAddress { register });
        }
        let buffer = held(address.buffer).ok_or(JobError::UnknownBuffer { register })?;
        if u64::from(address.offset) >= buffer.size {
            return Err(JobError::OffsetOutside {
                register,
                offset: address.offset,
                size: buffer.size,
            });
        }
        debug_assert!((1..=FD_MASK).contains(&buffer.fd));
        let at = usize::from(address.word);
        file[at..at + 4].copy_from_slice(&buffer.fd.to_ne_bytes());
        buffers |= 1 << buffer.slot;
        if address.offset != 0 {
            let elem = RegOffsetElem {
                index: u32::from(register),
                offset: address.offset,
            };
            staging.offsets[offsets..offsets + 8].copy_from_slice(&elem.to_ne_bytes());
            offsets += 8;
        }
    }

    // Every word the kernel translates holds a buffer: one left 0 would give the block
    // address 0.
    for descriptor in facts.descriptors {
        if !job.addresses.iter().any(|a| a.word == descriptor.at) {
            return Err(JobError::MissingDescriptor {
                word: descriptor.at,
            });
        }
    }

    Ok(Plan {
        write: (0, 0, facts.bytes),
        table: 0,
        offsets,
        read: Some((0, facts.results)),
        buffers,
    })
}
