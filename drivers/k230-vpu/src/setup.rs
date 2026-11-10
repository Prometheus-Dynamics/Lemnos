//! What a claim opens a codec session with: which way the block runs, the format of each side,
//! how many buffers each side has, and the caller's controls.
//!
//! The formats and controls are the caller's codec settings, carried to the kernel without being
//! read. The driver checks only what it must to hold the session's buffers: multi-planar formats
//! of one to [`crate::uapi::MAX_PLANES`] memory planes, and buffer counts the session can track.

use lemnos_driver_m2m_decode::{MAX_BUFFERS as QUEUE_BUFFERS, PixFormatMplane};

use crate::uapi::{MAX_PLANES, control};

/// Which way the block runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Bitstream in, frames out.
    Decode,
    /// Frames in, bitstream out.
    Encode,
}

/// A control (`VIDIOC_S_CTRL`) set on the session before it streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Control {
    /// The control's id: a standard codec control or one of [`crate::uapi::control`].
    pub id: u32,
    /// Its value.
    pub value: i32,
}

/// Controls a claim refuses:
///
/// - [`control::SECURE_VIDEO`] makes the kernel load the session's firmware through its secure
///   path (`mvx_fw_cache_get` with `securevideo`), which changes where the session's memory comes
///   from; this driver holds only ordinary dma-bufs.
/// - [`control::FW_PROFILING`] turns on the firmware's profiling, a debugging aid outside a
///   caller's codec settings.
pub const REFUSED_CONTROLS: [u32; 2] = [control::SECURE_VIDEO, control::FW_PROFILING];

/// How a claim opens its session.
#[derive(Clone, Copy, Debug)]
pub struct Setup<'a> {
    /// Which way the block runs.
    pub direction: Direction,
    /// The source side's format (the output queue): a coded format to decode, or the frames to
    /// encode. Multi-planar.
    pub source: PixFormatMplane,
    /// The destination side's format (the capture queue): the frames a decoder writes, or the
    /// coded format to encode to. Multi-planar.
    pub destination: PixFormatMplane,
    /// Source buffers to ask for, 1 to 64.
    pub sources: u32,
    /// Destination buffers to ask for, up to 64. A decoder may ask for 0, which waits for the
    /// stream's format (see [`crate::Claim::adopt_destinations`]); an encoder may not.
    pub destinations: u32,
    /// Controls set once the session is open, before it streams.
    pub controls: &'a [Control],
}

/// Why a setup was refused. The device was not touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    /// A format has no memory planes, or more than the driver's buffers have.
    Planes {
        /// Whether it is the source side's.
        source: bool,
        /// Its plane count.
        planes: u32,
    },
    /// No source buffers, more than 64 on a side, or an encoder without destination buffers.
    Buffers,
    /// A control in [`REFUSED_CONTROLS`].
    Control {
        /// The control.
        id: u32,
    },
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for SetupError {}

impl Setup<'_> {
    /// Checks the setup before anything is opened.
    pub fn check(&self) -> Result<(), SetupError> {
        for (source, format) in [(true, &self.source), (false, &self.destination)] {
            let planes = format.num_planes;
            if planes == 0 || planes as usize > MAX_PLANES {
                return Err(SetupError::Planes { source, planes });
            }
        }
        if self.sources == 0
            || self.sources > QUEUE_BUFFERS
            || self.destinations > QUEUE_BUFFERS
            || (self.direction == Direction::Encode && self.destinations == 0)
        {
            return Err(SetupError::Buffers);
        }
        if let Some(c) = self
            .controls
            .iter()
            .find(|c| REFUSED_CONTROLS.contains(&c.id))
        {
            return Err(SetupError::Control { id: c.id });
        }
        Ok(())
    }
}
