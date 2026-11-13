//! The write side of a VEPU580 H.265 job: the caller's values in the register classes they belong
//! to, the address registers that hold its buffers, and the status class read back, in the form
//! `lemnos-driver-rk-mpp`'s [`Job`] and `codec.block/1` take.
//!
//! The block, the classes, the read-back window, the refusals and the job's shape are the same as
//! the H.264 encoder's, so they are re-exported from [`crate::vepu580_h264::job`] rather than
//! copied: [`SetError`], [`StreamRef`], [`CodecJob`], [`READ_BACK`], [`WRITE_CLASSES`],
//! [`WRITE_RUNS`], [`TRANSLATED_CLASS`] and [`BLOCK_WORDS`]. What is H.265's own is the register
//! set, which holds the H.265 values.

use lemnos_driver_rk_mpp::{Address, BufferId, Job, MAX_ADDRESSES, Run};

use super::VEPU580_CLASSES;
use crate::layout::{AddressReg, Class, Field, Region};
pub use crate::vepu580_h264::job::{
    BLOCK_WORDS, CodecJob, READ_BACK, SetError, StreamRef, TRANSLATED_CLASS, WRITE_CLASSES,
    WRITE_RUNS,
};

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
fn class_of(word: usize) -> Option<&'static Class> {
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

    /// Sets a field of a writable class, with the same checks as the H.264 set: the width, the
    /// sign, the header's encodings, and the field's access.
    ///
    /// # Errors
    ///
    /// [`SetError::OutsideWrite`] for a field of a class a job does not write (the status, debug or
    /// OSD class, or word 23, which is in no class), [`SetError::ReadOnly`] for a field that is not
    /// writable, [`SetError::Range`] and [`SetError::Reserved`] for a value the field does not take.
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
