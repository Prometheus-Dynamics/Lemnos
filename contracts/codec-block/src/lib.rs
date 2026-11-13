//! The `codec.block/1` contract: a hardware codec block (a video decoder or encoder, a still
//! image coder) given to a caller, with its work arriving as jobs on a ring.
//!
//! Lemnos gives the caller the block; the codec is the caller's. A job's work is bytes the
//! caller built for the block (register values, a set of controls, the settings of a
//! bitstream) and the buffers it runs on. The block's driver carries the work to the block
//! and never reads what it means: it checks only what would otherwise let the work name
//! memory the caller doesn't hold. No bitstream is parsed here and no codec is named.
//!
//! # The job stream
//!
//! A consumer opens the stream named by [`CodecBlock::job_stream`]. Its buffers are the
//! memory the block works on (the bitstream, the pictures, the work itself), and its ring
//! carries the jobs. A job is one [`Job`], encoded with this crate's codec and posted on the
//! stream's job queue. No socket is used per job.
//!
//! The block holds every buffer the consumer has not been handed, so a job can name any of
//! them: the consumer fills a buffer before posting the job that names it, and leaves the
//! buffers of a posted job alone until its completion. Hardware reads and writes the buffers
//! themselves: nothing is copied but the work.
//!
//! # The work
//!
//! A job names a buffer that holds its work, [`Job::work_bytes`] long, in the layout of
//! [`work`]: a [`WorkHeader`], then the buffer references, then the items.
//!
//! - A [`BufferRef`] puts `length` bytes at `offset` of a stream buffer in one of the block's
//!   slots. What a slot is, is the block's: a register that takes a buffer's address, the
//!   place of the bitstream or of the picture in a decode. The driver checks that the buffer
//!   is one it holds and that `offset` and `length` lie inside the memory really behind it,
//!   and it is the driver, never the work, that turns the reference into what the block is
//!   given.
//! - An item is a tag and bytes. What a tag is, is the block's too: a run of register values,
//!   the registers wanted back, one control of a stateless decoder. The bytes are the
//!   caller's and are passed on as they are.
//!
//! What a slot is, is the block's, and the block's driver documents its slots and tags under
//! the name [`CodecBlock::block`] gives (`rk-mpp:vdpu2`, `v4l2-stateless`). A block that names
//! a reference picture by a value in its items, rather than by a slot, says so there too: a
//! stateless decoder's reference is a timestamp in its controls, and is not a slot of the
//! contract. A caller that doesn't know the name has no work to give it.
//!
//! The work is read once, into the driver's own memory, when the job is taken: changing the
//! buffer afterwards changes nothing.
//!
//! # Completion
//!
//! Completion is a frame on the same stream. Its buffer is the job's [`Job::result`] buffer
//! and its `identity` is the job's. The block writes a [`Completion`] to the start of the
//! buffer first, and after [`DATA_OFFSET`] what the job read back from the block
//! ([`Completion::count`] bytes: register values, how many bytes of a picture were written),
//! in the form the block's driver documents. The frame's length is [`DATA_OFFSET`] plus that
//! count. The result buffer may be the work's buffer, and must not be one the job references.
//! It is released with the usual frame return; every other buffer of the job stays with the
//! block, and its bytes are the consumer's to read once the completion is popped.
//!
//! Every job the block reads gets exactly one completion. Jobs run one at a time, in the
//! order they were posted. A job that cannot be decoded, or whose work or result buffer is
//! not one the block holds, is a broken consumer: the block faults the stream, as it does for
//! any malformed ring. A job whose work is malformed or names what it may not completes as
//! [`JobStatus::Refused`], and nothing reaches the block.
//!
//! Units: lengths and offsets in bytes, buffers by their number on the job stream.

#![no_std]

pub mod work;

use lemnos_core::{Decode, Encode, Result, contract};

/// Longest stream name [`CodecBlock::job_stream`] answers, in bytes.
pub const MAX_STREAM_NAME: usize = 32;

/// Longest block name [`CodecBlock::block`] answers, in bytes.
pub const MAX_BLOCK_NAME: usize = 48;

/// Where a job's read-back starts in its result buffer. The bytes before are the completion's
/// record.
pub const DATA_OFFSET: usize = 8;

/// One job, posted on the job stream.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Job {
    /// The caller's name for the job. Carried to the completion, never read.
    pub identity: u64,
    /// The buffer that holds the work, from its first byte.
    pub work: u32,
    /// How many bytes of it are the work.
    pub work_bytes: u32,
    /// The buffer the completion is written to and published on.
    pub result: u32,
}

/// Encoded size of a [`Job`], in bytes. The job queue must hold jobs this large.
pub const JOB_BYTES: usize = 8 + 4 + 4 + 4;

/// The start of a job's work.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkHeader {
    /// How many [`BufferRef`]s follow.
    pub refs: u16,
    /// How many items follow them.
    pub items: u16,
}

/// Encoded size of a [`WorkHeader`], in bytes.
pub const WORK_HEADER_BYTES: usize = 2 + 2;

/// Part of a stream buffer, in one of the block's slots.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BufferRef {
    /// The slot, as the block numbers them.
    pub slot: u32,
    /// A buffer on the job stream.
    pub buffer: u32,
    /// Where in the buffer the block starts.
    pub offset: u32,
    /// How many bytes from there are the block's to use. 0 where the slot has no length of its
    /// own (the block uses the buffer from `offset` as far as the work makes it).
    pub length: u32,
}

/// Encoded size of a [`BufferRef`], in bytes.
pub const BUFFER_REF_BYTES: usize = 4 * 4;

/// What comes before an item's bytes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ItemHeader {
    /// What the item is, as the block numbers them.
    pub tag: u32,
    /// How many bytes follow.
    pub bytes: u32,
}

/// Encoded size of an [`ItemHeader`], in bytes.
pub const ITEM_HEADER_BYTES: usize = 4 + 4;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $value:literal),* $(,)? }) => {
        impl Encode for $name {
            const SHAPE: lemnos_core::Shape = lemnos_core::Shape::U8;
            fn encode(&self, out: &mut lemnos_core::Writer<'_>) -> Result<()> {
                (*self as u8).encode(out)
            }
        }

        impl Decode<'_> for $name {
            fn decode(from: &mut lemnos_core::Reader<'_>) -> Result<Self> {
                match u8::decode(from)? {
                    $($value => Ok(Self::$variant),)*
                    _ => Err(lemnos_core::Status::INVALID_ARGUMENT),
                }
            }
        }
    };
}

/// How a job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum JobStatus {
    /// The block ran the job, and what it read back is after the record.
    #[default]
    Done = 0,
    /// The job was refused and nothing reached the block: work that is malformed, over the
    /// block's limits, or names a buffer, an offset, a slot or an item it may not.
    Refused = 1,
    /// The block was given the job and did not finish it, or is not there to be given it.
    Failed = 2,
}

wire_enum!(JobStatus { Done = 0, Refused = 1, Failed = 2 });

/// The completion's record, at the start of the result buffer.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Completion {
    pub status: JobStatus,
    /// Bytes of read-back after [`DATA_OFFSET`]. 0 unless the job was done.
    pub count: u32,
}

/// Encoded size of a [`Completion`], in bytes. It fits in [`DATA_OFFSET`].
pub const COMPLETION_BYTES: usize = 1 + 4;

/// What the block takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockInfo {
    /// The most bytes of work one job carries.
    pub max_work_bytes: u32,
    /// The most buffer references in one job.
    pub max_refs: u16,
    /// The most items in one job.
    pub max_items: u16,
    /// The most bytes one job reads back. A result buffer holds [`DATA_OFFSET`] more.
    pub max_read_back: u32,
    /// Buffers of the job stream the block can use, counted from 0. Higher ones can't be
    /// named.
    pub max_buffers: u32,
}

/// Counted since the driver started.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Jobs completed as [`JobStatus::Done`].
    pub jobs: u64,
    /// Jobs completed as [`JobStatus::Refused`], and consumers faulted for what was no job.
    pub refused: u64,
    /// Jobs completed as [`JobStatus::Failed`].
    pub failed: u64,
}

#[contract("codec.block", version = 1)]
pub trait CodecBlock {
    #[method(1)]
    fn info(&mut self) -> Result<BlockInfo>;
    /// The block's name, as UTF-8 into `into`, returning its length: what says which slots
    /// and item tags its work is made of. `into` holds at least [`MAX_BLOCK_NAME`] bytes.
    #[method(2)]
    fn block(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// The name to open the job stream by, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_STREAM_NAME`] bytes.
    #[method(3)]
    fn job_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    #[method(4)]
    fn counters(&mut self) -> Result<Counters>;
}
