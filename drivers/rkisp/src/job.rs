//! Where a caller's numbers meet the ISP's queues, checked first.
//!
//! A [`IspJob`] names buffers by index; the driver turns them into buffers it holds. Parameters
//! come in as the caller's bytes and go to the kernel's parameter buffer, statistics and images
//! come out of the kernel's buffers into the caller's stream. Each step is bounded here: an index
//! against the count of buffers really made, a length against the buffer it is read from and the
//! one it is written to. No arithmetic on a caller's number can overflow, and nothing here
//! allocates, so it runs on every frame.
//!
//! The vendor driver takes buffers it allocated itself (`MMAP`), so no caller's number ever
//! becomes an address, an offset into device memory or a file descriptor: the indices and lengths
//! below are the whole surface.

use lemnos_contract_camera_isp::{IspJob, MAX_OUTPUTS, NO_BUFFER};
use lemnos_core::{Result, Status};

use crate::generation::{Generation, OUTPUTS};

/// What the driver set up, which every job is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    generation: Generation,
    config_buffers: u32,
    /// Buffers on each output's stream; `None` for an output that is off.
    outputs: [Option<u32>; OUTPUTS as usize],
}

impl Limits {
    /// `config_buffers` buffers of `config_buffer_bytes` on the config stream, and `outputs`
    /// buffers on each output's stream. `INVALID_ARGUMENT` when a config buffer can't hold the
    /// generation's parameters.
    pub fn new(
        generation: Generation,
        config_buffers: u32,
        config_buffer_bytes: u32,
        outputs: [Option<u32>; OUTPUTS as usize],
    ) -> Result<Self> {
        if config_buffer_bytes < generation.config_layout().bytes {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            generation,
            config_buffers,
            outputs,
        })
    }
}

/// A job that passed [`check`]: every index is one the driver holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    /// The caller's name for the frame, carried to the statistics and images.
    pub identity: u64,
    /// A buffer on the config stream.
    pub config_buffer: u32,
    /// For each output, a buffer on its stream, or `None` to skip it this job.
    pub outputs: [Option<u32>; OUTPUTS as usize],
}

/// The job, if the driver can run it; `None` for a caller's mistake, which the driver counts as
/// dropped. Refused: another layout than the generation's parameters, a config buffer past the
/// config stream, an input buffer (frames come straight from the receiver), a buffer for an
/// output that is off or that the ISP doesn't have, and a buffer past its output's stream.
#[must_use]
pub fn check(job: &IspJob, limits: &Limits) -> Option<Checked> {
    if job.layout != limits.generation.config_layout().layout
        || job.config_buffer >= limits.config_buffers
        || job.input_buffer != NO_BUFFER
    {
        return None;
    }
    let mut outputs = [None; OUTPUTS as usize];
    for (index, &buffer) in job.outputs.iter().enumerate().take(MAX_OUTPUTS) {
        if buffer == NO_BUFFER {
            continue;
        }
        let count = limits.outputs.get(index).copied().flatten()?;
        if buffer >= count {
            return None;
        }
        *outputs.get_mut(index)? = Some(buffer);
    }
    Some(Checked {
        identity: job.identity,
        config_buffer: job.config_buffer,
        outputs,
    })
}

/// Copies one parameter buffer's worth of the caller's bytes `from` (a config stream buffer) into
/// the kernel's parameter buffer `into`, and returns its length: the generation's parameter size.
/// `INVALID_ARGUMENT` when `from` is shorter, `BUFFER_TOO_SMALL` when `into` is.
pub fn copy_params(generation: Generation, from: &[u8], into: &mut [u8]) -> Result<usize> {
    let n = generation.config_layout().bytes as usize;
    let from = from.get(..n).ok_or(Status::INVALID_ARGUMENT)?;
    into.get_mut(..n)
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(from);
    Ok(n)
}

/// The filled part of a dequeued kernel buffer (statistics or an image), for writing into a
/// stream buffer of `stream_buffer_bytes`: the first `bytes_used` bytes of `mapped`. `None` when
/// the kernel reports more than it mapped, or more than the stream buffer holds.
#[must_use]
pub fn filled(mapped: &[u8], bytes_used: usize, stream_buffer_bytes: u32) -> Option<&[u8]> {
    if bytes_used > stream_buffer_bytes as usize {
        return None;
    }
    mapped.get(..bytes_used)
}
