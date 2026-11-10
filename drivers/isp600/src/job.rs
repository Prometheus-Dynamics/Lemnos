//! Where a caller's numbers meet the ISP, checked first.
//!
//! A [`IspJob`] names buffers by index; the driver turns them into buffers it holds. The load
//! image comes in as the caller's bytes, is copied once, and only the copy is checked
//! ([`crate::layout::check_load`]) and handed to the kernel; statistics and images come out of the
//! kernel's buffers into the caller's streams. Each step is bounded here: an index against the
//! count of buffers really made, a length against the buffer it is read from and the one it is
//! written to. No arithmetic on a caller's number can overflow, and nothing here allocates.

use lemnos_contract_camera_isp::{IspJob, MAX_OUTPUTS, NO_BUFFER};
use lemnos_contract_isp_isp600::LAYOUT_LOAD;

/// What the driver set up, which every job is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub config_buffers: u32,
    /// Buffers on the output's stream.
    pub output_buffers: u32,
}

/// A job that passed [`check`]: every index is within the buffers made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    /// The caller's name for the frame, carried to the statistics and image.
    pub identity: u64,
    /// A buffer on the config stream.
    pub config_buffer: u32,
    /// A buffer on the output's stream, or `None` to skip the image this job.
    pub output: Option<u32>,
}

/// The job, if the driver can run it; `None` for a caller's mistake, which the driver counts as
/// dropped. Refused: another layout than [`LAYOUT_LOAD`], a config buffer past the config stream,
/// an input buffer (frames come straight from the receiver), a buffer for any output but the
/// first (the ISP has one), and a buffer past the output's stream.
#[must_use]
pub fn check(job: &IspJob, limits: &Limits) -> Option<Checked> {
    if job.layout != LAYOUT_LOAD
        || job.config_buffer >= limits.config_buffers
        || job.input_buffer != NO_BUFFER
    {
        return None;
    }
    let [first, rest @ ..] = &job.outputs;
    if rest.iter().take(MAX_OUTPUTS - 1).any(|&b| b != NO_BUFFER) {
        return None;
    }
    let output = match *first {
        NO_BUFFER => None,
        buffer if buffer < limits.output_buffers => Some(buffer),
        _ => return None,
    };
    Some(Checked {
        identity: job.identity,
        config_buffer: job.config_buffer,
        output,
    })
}

/// The filled part of a kernel buffer (statistics or an image plane), for writing into a stream
/// buffer of `stream_buffer_bytes`: the first `bytes_used` bytes of `mapped`. `None` when the
/// kernel reports more than it mapped, or more than the stream buffer holds.
#[must_use]
pub fn filled(mapped: &[u8], bytes_used: usize, stream_buffer_bytes: usize) -> Option<&[u8]> {
    if bytes_used > stream_buffer_bytes {
        return None;
    }
    mapped.get(..bytes_used)
}
