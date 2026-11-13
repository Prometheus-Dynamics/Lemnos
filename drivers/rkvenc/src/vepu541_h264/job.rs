//! The write side of a job: the values the caller chooses, and the address registers that hold
//! its buffers, in the form `lemnos-driver-rk-mpp`'s [`Job`] takes.
//!
//! Every value is checked against its field's width and sign, and a read-only register is refused.
//! An address register takes one buffer, at an offset its alignment allows. Nothing else is decided
//! here: registers the caller does not set go out as 0, and the driver replaces each address
//! register's value with the buffer's address.

use std::fmt;

use lemnos_driver_rk_mpp::{Address, BufferId, Job, MAX_ADDRESSES, ReadBack, Run};

use crate::layout::{AddressReg, Field, Region};

/// The words of the write side: a job writes words `0..WRITE_WORDS`, up to the end of the last
/// register the header makes writable (`OSD_ADDR`, words 124 to 131). Words 132 to 208 are read-only
/// status and are read back, never written. The kernel's own write window ends at the same word
/// (`reg_end` 131 in `mpp_rkvenc.c`).
pub const WRITE_WORDS: usize = 132;

/// The words of the second window (the L2 registers), as the kernel bounds them: 320 words from
/// `RKVENC_L2_OFFSET`. The header's L2 struct uses words 0 to 195 of them.
pub const L2_WORDS: usize = 320;

/// The first word of the second window in the block's register numbering: 0x10000 bytes
/// (`RKVENC_L2_OFFSET` of `mpp_rkvenc.c`), word 0x4000.
pub const L2_FIRST: u16 = 0x4000;

/// The read-back window: from `hw_status` (word 7) to the end of the read-back registers (word
/// 209). A job asks for all of it, so every status register is copied back.
pub const READ_BACK: ReadBack = ReadBack {
    first: 7,
    count: 202,
};

/// Why a value or an address was refused. The job is not built from a refused value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetError {
    /// The field is in a window the write side does not reach.
    OutsideWrite,
    /// The register is read only, or the header gives no access type for it.
    ReadOnly,
    /// The value is one the header marks reserved for the field.
    Reserved {
        /// The value given.
        value: i64,
    },
    /// The value does not fit the field.
    Range {
        /// The value given.
        value: i64,
        /// The field's width in bits.
        width: u8,
        /// Whether the field is signed (then the value is `-2^(width-1)` to `2^(width-1) - 1`).
        signed: bool,
    },
    /// The offset is not a multiple of the alignment the header gives.
    Misaligned {
        /// The alignment in bytes.
        align: u32,
    },
    /// The register already holds a buffer in this job.
    DuplicateAddress,
    /// The job already names [`MAX_ADDRESSES`] address registers.
    TooManyAddresses,
    /// The job names a stream buffer, which only a codec.block job can carry: use [`RegisterSet::codec_job`].
    StreamBuffers,
    /// The job names a claim's buffer, which only a claim job can carry: use [`RegisterSet::job`].
    ClaimBuffers,
    /// The field is in the second window, which [`RegisterSet::set_l2`] sets.
    L2Window,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideWrite => write!(f, "the field is outside the write side"),
            Self::ReadOnly => write!(f, "the register is not writable"),
            Self::Range {
                value,
                width,
                signed,
            } => write!(
                f,
                "{value} does not fit a {} {width}-bit field",
                if *signed { "signed" } else { "unsigned" }
            ),
            Self::Reserved { value } => write!(f, "{value} is a reserved encoding of the field"),
            Self::Misaligned { align } => write!(f, "the offset is not {align}-byte aligned"),
            Self::DuplicateAddress => write!(f, "the register already holds a buffer"),
            Self::TooManyAddresses => write!(f, "more than {MAX_ADDRESSES} address registers"),
            Self::StreamBuffers => write!(f, "the job names a stream buffer: use codec_job"),
            Self::ClaimBuffers => write!(f, "the job names a claim's buffer: use job"),
            Self::L2Window => write!(f, "the field is in the second window: use set_l2"),
        }
    }
}

impl std::error::Error for SetError {}

/// A reference to a stream buffer the caller holds, as `codec.block/1` names one: the caller's
/// buffer index, `offset` bytes into it, and the register the driver writes its device address
/// into. The driver takes the buffer's address there; the caller never sees a register number it
/// has to place by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamRef {
    /// The address register, by its word in the write side.
    pub register: u16,
    /// The caller's stream buffer index.
    pub buffer: u32,
    /// Bytes into the buffer; the header's alignment is already checked.
    pub offset: u32,
}

/// An encoder job in `codec.block/1`'s form: the write side's words from word 0, the second
/// window's words when the job sets any, the stream references of its address registers, and the
/// read-back window. Each run is one `TAG_WRITE` item.
#[derive(Clone, Copy, Debug)]
pub struct CodecJob<'a> {
    /// The register values of the write side, from word 0.
    pub write: &'a [u32],
    /// The second window's values, from [`L2_FIRST`], when the job sets any.
    pub l2: Option<Run<'a>>,
    /// The address registers, each a stream buffer and an offset.
    pub refs: &'a [StreamRef],
    /// The read-back window.
    pub read: ReadBack,
}

/// A job's register values and address registers. Built once and reused: [`Self::clear`] empties
/// it, and nothing allocates after [`Self::new`].
///
/// A job names its buffers in one of two ways. A caller that holds a claim names the claim's
/// buffers with [`Self::set_address`] and takes [`Self::job`]. A caller of `codec.block/1` names
/// its stream buffers with [`Self::set_stream_address`] and takes [`Self::codec_job`]. A set holds
/// one kind.
#[derive(Debug)]
pub struct RegisterSet {
    words: [u32; WRITE_WORDS],
    l2: [u32; L2_WORDS],
    /// The words of `l2` the job writes: up to the last one a field set.
    l2_len: usize,
    addresses: Vec<Address>,
    streams: Vec<StreamRef>,
}

impl Default for RegisterSet {
    fn default() -> Self {
        Self::new()
    }
}

impl RegisterSet {
    /// An empty set. Its address lists are allocated here, once, at their full size.
    #[must_use]
    pub fn new() -> Self {
        Self {
            words: [0; WRITE_WORDS],
            l2: [0; L2_WORDS],
            l2_len: 0,
            addresses: Vec::with_capacity(MAX_ADDRESSES),
            streams: Vec::with_capacity(MAX_ADDRESSES),
        }
    }

    /// Empties the set for the next job.
    pub fn clear(&mut self) {
        self.words = [0; WRITE_WORDS];
        self.l2 = [0; L2_WORDS];
        self.l2_len = 0;
        self.addresses.clear();
        self.streams.clear();
    }

    /// Sets a field of the write side. `value` is unsigned for an unsigned field and signed for a
    /// signed one; it must fit the field's width and be one of the field's encodings, when it has
    /// them.
    ///
    /// # Errors
    ///
    /// [`SetError::OutsideWrite`] for a field of another window, [`SetError::ReadOnly`] for a
    /// register that is not writable, [`SetError::Range`] for a value that does not fit, and
    /// [`SetError::Reserved`] for an encoding the header marks reserved.
    pub fn set(&mut self, field: Field, value: i64) -> Result<(), SetError> {
        if field.region != Region::Set || usize::from(field.word) >= WRITE_WORDS {
            return Err(SetError::OutsideWrite);
        }
        if !field.writable {
            return Err(SetError::ReadOnly);
        }
        let bits = encode(field, value)?;
        let slot = &mut self.words[usize::from(field.word)];
        *slot = merge(*slot, field, bits);
        Ok(())
    }

    /// Sets a field of the second window, which the job writes from [`L2_FIRST`] on. The same
    /// checks as [`Self::set`]; the words up to the last one set are written, the rest are not.
    ///
    /// # Errors
    ///
    /// The same as [`Self::set`], with [`SetError::OutsideWrite`] for a field of another window.
    pub fn set_l2(&mut self, field: Field, value: i64) -> Result<(), SetError> {
        if field.region != Region::L2 || usize::from(field.word) >= L2_WORDS {
            return Err(SetError::OutsideWrite);
        }
        if !field.writable {
            return Err(SetError::ReadOnly);
        }
        let bits = encode(field, value)?;
        let at = usize::from(field.word);
        self.l2[at] = merge(self.l2[at], field, bits);
        self.l2_len = self.l2_len.max(at + 1);
        Ok(())
    }

    /// Checks an address register and that the job has room for one more.
    fn check_address(&self, reg: AddressReg, offset: u32) -> Result<(), SetError> {
        if reg.region != Region::Set || usize::from(reg.word) >= WRITE_WORDS {
            return Err(SetError::OutsideWrite);
        }
        if !offset.is_multiple_of(reg.align) {
            return Err(SetError::Misaligned { align: reg.align });
        }
        let held = self.addresses.iter().any(|a| a.register == reg.word)
            || self.streams.iter().any(|a| a.register == reg.word);
        if held {
            return Err(SetError::DuplicateAddress);
        }
        if self.addresses.len() + self.streams.len() == MAX_ADDRESSES {
            return Err(SetError::TooManyAddresses);
        }
        Ok(())
    }

    /// Gives an address register a buffer the claim holds, `offset` bytes into it, for
    /// [`Self::job`].
    ///
    /// # Errors
    ///
    /// [`SetError::OutsideWrite`] for a register of another window, [`SetError::Misaligned`] when
    /// the offset breaks the header's alignment, [`SetError::DuplicateAddress`] when the register
    /// already holds a buffer here, and [`SetError::TooManyAddresses`] past [`MAX_ADDRESSES`].
    pub fn set_address(
        &mut self,
        reg: AddressReg,
        buffer: BufferId,
        offset: u32,
    ) -> Result<(), SetError> {
        self.check_address(reg, offset)?;
        self.addresses.push(Address {
            register: reg.word,
            buffer,
            offset,
        });
        Ok(())
    }

    /// Gives an address register one of the caller's stream buffers, `offset` bytes into it, for
    /// [`Self::codec_job`]. The buffer's index is the one the caller's `codec.block/1` work names.
    ///
    /// # Errors
    ///
    /// The same as [`Self::set_address`].
    pub fn set_stream_address(
        &mut self,
        reg: AddressReg,
        buffer: u32,
        offset: u32,
    ) -> Result<(), SetError> {
        self.check_address(reg, offset)?;
        self.streams.push(StreamRef {
            register: reg.word,
            buffer,
            offset,
        });
        Ok(())
    }

    /// The register values of the write side.
    #[must_use]
    pub fn words(&self) -> &[u32; WRITE_WORDS] {
        &self.words
    }

    /// The stream references named so far.
    #[must_use]
    pub fn stream_refs(&self) -> &[StreamRef] {
        &self.streams
    }

    /// The job for a claim holder: all of the write side from word 0, the second window's words
    /// when the set has any, its claim addresses, and the read-back window. The identity is carried
    /// to the completion, never read.
    ///
    /// `spare` holds the second window's run: a job borrows it, so the caller keeps it for as long
    /// as the job is used. It is written only when the set has second-window words.
    ///
    /// # Errors
    ///
    /// [`SetError::StreamBuffers`] when the set names stream buffers.
    pub fn job<'a>(
        &'a self,
        identity: u64,
        spare: &'a mut [Run<'a>; 1],
    ) -> Result<Job<'a>, SetError> {
        if !self.streams.is_empty() {
            return Err(SetError::StreamBuffers);
        }
        let more: &'a [Run<'a>] = if self.l2_len > 0 {
            spare[0] = Run {
                first: L2_FIRST,
                registers: &self.l2[..self.l2_len],
            };
            &spare[..]
        } else {
            &[]
        };
        Ok(Job {
            identity,
            first: 0,
            registers: &self.words,
            more,
            addresses: &self.addresses,
            read: Some(READ_BACK),
        })
    }

    /// The job in `codec.block/1`'s form: the write side's words from word 0, the stream references
    /// of its address registers, and the read-back window. A caller sends it as its work, with the
    /// stream buffers it names; the driver places each buffer's address.
    ///
    /// # Errors
    ///
    /// [`SetError::ClaimBuffers`] when the set names a claim's buffers.
    pub fn codec_job(&self) -> Result<CodecJob<'_>, SetError> {
        if !self.addresses.is_empty() {
            return Err(SetError::ClaimBuffers);
        }
        Ok(CodecJob {
            write: &self.words,
            l2: (self.l2_len > 0).then(|| Run {
                first: L2_FIRST,
                registers: &self.l2[..self.l2_len],
            }),
            refs: &self.streams,
            read: READ_BACK,
        })
    }
}

/// The field's bits in place for `value`, checked against its width, sign and encodings.
fn encode(field: Field, value: i64) -> Result<u32, SetError> {
    let width = u32::from(field.width);
    let (low, high) = if field.signed {
        (-(1i64 << (width - 1)), (1i64 << (width - 1)) - 1)
    } else {
        (0, (1i64 << width) - 1)
    };
    if value < low || value > high {
        return Err(SetError::Range {
            value,
            width: field.width,
            signed: field.signed,
        });
    }
    if !field.values.is_empty() && !field.values.iter().any(|&v| i64::from(v) == value) {
        return Err(SetError::Reserved { value });
    }
    Ok(((value as u32) & field.mask()) << field.lsb)
}

/// `word` with the field's bits replaced by `bits` (from [`encode`]).
fn merge(word: u32, field: Field, bits: u32) -> u32 {
    let shifted = field.mask() << field.lsb;
    (word & !shifted) | bits
}
