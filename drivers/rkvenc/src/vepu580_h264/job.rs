//! The write side of a VEPU580 H.264 job: the caller's values in the register classes they belong
//! to, the address registers that hold its buffers, and the status class read back, in the form
//! `lemnos-driver-rk-mpp`'s [`Job`] and `codec.block/1` take.
//!
//! The job is one write run per writable class ([`WRITE_RUNS`]), each the class's whole range, so
//! the kernel's class buffers are filled as the caller set them and zero elsewhere. The OSD class
//! is not written: a field of it is refused ([`SetError::OutsideWrite`]), since rk-mpp has no
//! window for it. The read-back is the status class ([`READ_BACK`]).

use std::fmt;

use lemnos_driver_rk_mpp::{Address, BufferId, Job, MAX_ADDRESSES, ReadBack, Run};

use super::VEPU580_CLASSES;
use crate::layout::{AddressReg, Field, Region};

/// The words of the whole block, from word 0 to the end of the debug class (word 5333).
pub const BLOCK_WORDS: usize = 5334;

/// The classes a job writes, in the kernel's order: BASE, PIC, RC, PAR, SQI and SCL. The class
/// index is the kernel's `RKVENC_CLASS_TYPE` value.
pub const WRITE_CLASSES: [u8; 6] = [0, 1, 2, 3, 4, 5];

/// The picture class: the only one the kernel translates (its fd class).
pub const TRANSLATED_CLASS: u8 = 1;

/// The status class, read back by a job: word 4096, 180 words.
pub const READ_BACK: ReadBack = ReadBack {
    first: 4096,
    count: 180,
};

/// Why a value or an address was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetError {
    /// The field is in a class a job does not write (the status, debug or OSD class), or the
    /// address is in a class the kernel does not translate.
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
        /// Whether the field is signed.
        signed: bool,
    },
    /// The offset is not a multiple of the alignment the header gives.
    Misaligned {
        /// The alignment in bytes.
        align: u32,
    },
    /// The offset does not fit in the descriptor word above the fd (22 bits).
    OffsetTooLarge,
    /// The register already holds a buffer in this job.
    DuplicateAddress,
    /// The job already names [`MAX_ADDRESSES`] address registers.
    TooManyAddresses,
    /// The job names a stream buffer: use [`RegisterSet::codec_job`].
    StreamBuffers,
    /// The job names a claim's buffer: use [`RegisterSet::job`].
    ClaimBuffers,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideWrite => write!(f, "the register is in a class a job does not write"),
            Self::ReadOnly => write!(f, "the register is not writable"),
            Self::Reserved { value } => write!(f, "{value} is a reserved encoding of the field"),
            Self::Range {
                value,
                width,
                signed,
            } => write!(
                f,
                "{value} does not fit a {} {width}-bit field",
                if *signed { "signed" } else { "unsigned" }
            ),
            Self::Misaligned { align } => write!(f, "the offset is not {align}-byte aligned"),
            Self::OffsetTooLarge => write!(f, "the offset does not fit the descriptor word"),
            Self::DuplicateAddress => write!(f, "the register already holds a buffer"),
            Self::TooManyAddresses => write!(f, "more than {MAX_ADDRESSES} address registers"),
            Self::StreamBuffers => write!(f, "the job names a stream buffer: use codec_job"),
            Self::ClaimBuffers => write!(f, "the job names a claim's buffer: use job"),
        }
    }
}

impl std::error::Error for SetError {}

/// A reference to a stream buffer the caller holds, as `codec.block/1` names one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamRef {
    /// The address register, by its absolute word.
    pub register: u16,
    /// The caller's stream buffer index.
    pub buffer: u32,
    /// Bytes into the buffer.
    pub offset: u32,
}

/// A job in `codec.block/1`'s form: one write item per run, the stream references of its address
/// registers, and the read-back.
#[derive(Clone, Copy, Debug)]
pub struct CodecJob<'a> {
    /// The write runs, in class order. Each is one `TAG_WRITE` item: the first word, then values.
    pub runs: [Run<'a>; WRITE_RUNS],
    /// The address registers, each a stream buffer and an offset.
    pub refs: &'a [StreamRef],
    /// The read-back: the status class.
    pub read: ReadBack,
}

/// The most write runs a job carries: one per writable class.
pub const WRITE_RUNS: usize = 6;

/// A job's register values and address registers, built once and reused.
#[derive(Debug)]
pub struct RegisterSet {
    words: Box<[u32; BLOCK_WORDS]>,
    addresses: Vec<Address>,
    streams: Vec<StreamRef>,
}

impl Default for RegisterSet {
    fn default() -> Self {
        Self::new()
    }
}

/// The class a word sits in, if any.
fn class_of(word: usize) -> Option<&'static super::Class> {
    VEPU580_CLASSES.iter().find(|c| {
        let first = usize::from(c.first);
        word >= first && word < first + usize::from(c.words)
    })
}

impl RegisterSet {
    /// An empty set; its lists are allocated here, once.
    #[must_use]
    pub fn new() -> Self {
        Self {
            words: Box::new([0; BLOCK_WORDS]),
            addresses: Vec::with_capacity(MAX_ADDRESSES),
            streams: Vec::with_capacity(MAX_ADDRESSES),
        }
    }

    /// Empties the set for the next job.
    pub fn clear(&mut self) {
        self.words.fill(0);
        self.addresses.clear();
        self.streams.clear();
    }

    /// Sets a field of a writable class. The same checks as the VEPU541 set: the width, the sign,
    /// the header's encodings, and the field's access.
    ///
    /// # Errors
    ///
    /// [`SetError::OutsideWrite`] for a field of a class a job does not write, [`SetError::ReadOnly`]
    /// for a field that is not writable, [`SetError::Range`] and [`SetError::Reserved`] for a value
    /// the field does not take.
    pub fn set(&mut self, field: Field, value: i64) -> Result<(), SetError> {
        let word = usize::from(field.word);
        let Region::Class(class) = field.region else {
            return Err(SetError::OutsideWrite);
        };
        if !WRITE_CLASSES.contains(&class) || class_of(word).is_none() {
            return Err(SetError::OutsideWrite);
        }
        if !field.writable {
            return Err(SetError::ReadOnly);
        }
        let bits = encode(field, value)?;
        self.words[word] = merge(self.words[word], field, bits);
        Ok(())
    }

    /// Checks an address register and that the job has room for one more.
    fn check_address(&self, reg: AddressReg, offset: u32) -> Result<(), SetError> {
        if reg.region != Region::Class(TRANSLATED_CLASS) {
            return Err(SetError::OutsideWrite);
        }
        if !offset.is_multiple_of(reg.align) {
            return Err(SetError::Misaligned { align: reg.align });
        }
        if offset >= 1 << 22 {
            return Err(SetError::OffsetTooLarge);
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

    /// Gives an address register a buffer the claim holds, for [`Self::job`].
    ///
    /// # Errors
    ///
    /// [`SetError::OutsideWrite`] for a register outside the picture class (the only one the kernel
    /// translates), and the checks of [`Self::check_address`].
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

    /// Gives an address register one of the caller's stream buffers, for [`Self::codec_job`].
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

    /// The write run of class `class`: its first word and its words.
    fn run(&self, class: u8) -> Run<'_> {
        let c = VEPU580_CLASSES[usize::from(class)];
        let first = usize::from(c.first);
        Run {
            first: c.first,
            registers: &self.words[first..first + usize::from(c.words)],
        }
    }

    /// The job for a claim holder: the write runs, the claim addresses and the read-back. The
    /// first run is the BASE class; `spare` holds the other five, so the caller keeps it for as
    /// long as the job is used.
    ///
    /// # Errors
    ///
    /// [`SetError::StreamBuffers`] when the set names stream buffers.
    pub fn job<'a>(
        &'a self,
        identity: u64,
        spare: &'a mut [Run<'a>; WRITE_RUNS - 1],
    ) -> Result<Job<'a>, SetError> {
        if !self.streams.is_empty() {
            return Err(SetError::StreamBuffers);
        }
        for (slot, &class) in spare.iter_mut().zip(&WRITE_CLASSES[1..]) {
            *slot = self.run(class);
        }
        let first = self.run(WRITE_CLASSES[0]);
        Ok(Job {
            identity,
            first: first.first,
            registers: first.registers,
            more: &spare[..],
            addresses: &self.addresses,
            read: Some(READ_BACK),
        })
    }

    /// The job in `codec.block/1`'s form: one write run per writable class, the stream references,
    /// and the read-back.
    ///
    /// # Errors
    ///
    /// [`SetError::ClaimBuffers`] when the set names a claim's buffers.
    pub fn codec_job(&self) -> Result<CodecJob<'_>, SetError> {
        if !self.addresses.is_empty() {
            return Err(SetError::ClaimBuffers);
        }
        let runs = std::array::from_fn(|i| self.run(WRITE_CLASSES[i]));
        Ok(CodecJob {
            runs,
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
