//! The `npu/1` contract: a neural accelerator that runs programs on buffers, given as bytes.
//!
//! Lemnos never reads or compiles a program. A caller writes the program's bytes into a buffer on
//! the job stream and calls [`Npu::load`]. The block takes those bytes and hands them to the chip,
//! and only the block's own tooling reads them. A program the block refuses is `INVALID_ARGUMENT`.
//!
//! The buffers a program runs on are attached to it by slot, with [`Npu::attach`]. Slots number
//! from 0, and the program's own tensors say which slot is which. A run is one [`RunJob`], encoded
//! with this crate's codec and posted on the stream's job queue. No socket is used per run.
//!
//! A run touches only the buffers attached to its program, and its result buffer. A driver enforces
//! that with the chip's memory protection, so a program can name no address outside them. The result
//! buffer must not be one of the program's attached buffers.
//!
//! Completion is a frame on the same stream. Its buffer is the job's result buffer, its `identity`
//! is the job's, and its length is one byte: the block writes a [`RunResult`] there first. Every
//! job the block reads gets exactly one completion. A job that cannot be decoded, or whose result
//! buffer is not on the stream, is a broken consumer: the block faults the stream, as it does for
//! any malformed ring.
//!
//! While a run is in flight the block owns its attached buffers and its result buffer. After the
//! run's completion is popped the consumer may write the inputs again and read the outputs, and the
//! result buffer goes back with the usual frame return.
//!
//! Units: `bytes` in bytes, `program` and `slot` are numbers the block gives out and the caller
//! names. `fourcc` and tensor shapes are the program's business, not this contract's.

#![no_std]

use lemnos_core::{Decode, Encode, Result, contract};

/// Longest stream name [`Npu::job_stream`] answers, in bytes.
pub const MAX_STREAM_NAME: usize = 32;

/// The largest result a run completes with, in bytes.
pub const RESULT_BYTES: usize = 1;

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

/// What the block takes.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NpuInfo {
    /// The largest program [`Npu::load`] takes, in bytes.
    pub max_program_bytes: u32,
    /// Programs loaded at once.
    pub max_programs: u16,
    /// Attachment slots per program, numbered from 0.
    pub max_attached: u16,
}

/// The outcome of one run, written to the result buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum RunStatus {
    /// The program ran to the end and its outputs are written.
    #[default]
    Done = 0,
    /// The run was not started: an unknown program, a slot that is not attached, or a result
    /// buffer that is attached. Nothing ran.
    Refused = 1,
    /// The chip started the run and did not finish it.
    Failed = 2,
}

wire_enum!(RunStatus { Done = 0, Refused = 1, Failed = 2 });

/// The completion's record, in the result buffer.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunResult {
    pub status: RunStatus,
}

/// One run, posted on the job stream.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunJob {
    /// The caller's name for the run. Carried to the completion, never read.
    pub identity: u64,
    /// A program from [`Npu::load`].
    pub program: u32,
    /// A buffer on the job stream, not attached to the program, for the completion's record.
    pub result: u32,
}

/// Encoded size of a [`RunJob`], in bytes. The job queue must hold jobs this large.
pub const JOB_BYTES: usize = 8 + 4 + 4;

/// Counted since the job stream was opened.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Runs completed as [`RunStatus::Done`].
    pub jobs: u64,
    /// Runs completed as [`RunStatus::Refused`] or [`RunStatus::Failed`].
    pub errors: u64,
}

#[contract("npu", version = 1)]
pub trait Npu {
    #[method(1)]
    fn info(&mut self) -> Result<NpuInfo>;
    /// The name to open the job stream by, as UTF-8 into `into`, returning its length. `into`
    /// holds at least [`MAX_STREAM_NAME`] bytes.
    #[method(2)]
    fn job_stream(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Loads the program in the first `bytes` of `buffer` on the job stream, and returns its
    /// number. The buffer is the caller's again when this returns. `INVALID_ARGUMENT` for a
    /// program too large, or one the chip refuses. `BUSY` when no program slot is free.
    #[method(3)]
    fn load(&mut self, buffer: u32, bytes: u32) -> Result<u32>;
    /// Unloads a program, and detaches all its buffers. `NOT_FOUND` for an unknown program.
    #[method(4)]
    fn unload(&mut self, program: u32) -> Result<()>;
    /// Attaches the first `bytes` of `buffer` on the job stream as slot `slot` of `program`.
    /// `NOT_FOUND` for an unknown program. `INVALID_ARGUMENT` for a slot past the last, a buffer
    /// that is not on the stream, or bytes past the buffer.
    #[method(5)]
    fn attach(&mut self, program: u32, slot: u16, buffer: u32, bytes: u32) -> Result<()>;
    /// Detaches one slot. `NOT_FOUND` for an unknown program or a slot not attached.
    #[method(6)]
    fn detach(&mut self, program: u32, slot: u16) -> Result<()>;
    #[method(7)]
    fn counters(&mut self) -> Result<Counters>;
}
