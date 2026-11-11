//! The deinterlacer as `codec.block/1` has it. The block's name is `sunxi-di`.
//!
//! A job is one pass of the block: its settings, and its frames, each a part of a buffer of
//! the job stream. The kernel's request is a structure of descriptors and plane addresses it
//! checks nothing of, so this driver builds it, from the caller's numbers, and only out of
//! frames checked against the real size of the dma-buf each sits in. The work is:
//!
//! - **Item [`TAG_CONFIG`]**, exactly one, [`CONFIG_BYTES`] long: the frames' width and
//!   height as `u32`s, then four bytes: the mode ([`mode`]), the flags ([`flag`]), the noise
//!   reduction (0 none, 1 fixed, 2 adaptive) and its level (0 high, 1 middle, 2 low).
//! - **Items [`TAG_FRAME`]**, one for each frame, in the frames' order, [`FRAME_BYTES`] long,
//!   six `u32`s: the pixel format (a DRM fourcc of [`Format`]), the luma stride, the chroma
//!   stride, and the byte offsets in the buffer of the luma plane, the chroma plane and the
//!   second chroma plane (0 for a two-plane format).
//! - **A buffer reference for each frame**, in the same order: the inputs, oldest first, in
//!   slots 0 and up, then the outputs (the deinterlaced frames, then the noise-reduced one).
//!   How many of each a mode takes is [`Mode::frames`]. `offset` and `length` are 0: where
//!   the planes are is the frame item's.
//!
//! The read-back is one byte: 1 when the block's motion detector took the input to be
//! progressive, 0 otherwise. The outputs are written either way.
//!
//! What is checked, in `lemnos-driver-a733-di-sys`'s [`Frame`] and [`Job`]: the size (a
//! multiple of 4 up to 2048 each way), each stride (at least a row, at most the block's
//! pitch register), every plane's last byte inside the dma-buf's real size (asked of the
//! kernel, overflow-free), a chroma offset that is not 0, the frame count the mode takes, one
//! size for all frames, planar outputs with noise reduction. A stream buffer without a handle
//! refuses the job.
//!
//! A job runs to its end inside [`Block::start`] (the kernel's own 3 s bound stands). After a
//! failure the block is closed and a later job opens it again, behind a backoff doubling from
//! 250 ms to 8 s; it never gives up and never turns itself off.

use std::io;
use std::path::PathBuf;

use lemnos_contract_codec_block::work::Work;
use lemnos_contract_codec_block::{BlockInfo, CodecBlock, Counters};
use lemnos_core::{Config as BoardConfig, Health, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_driver_a733_di_sys::{
    Config, Field, Format, Frame, Job, Layout, Level, Mode, Tnr, uapi,
};
use lemnos_driver_codec_jobs::{Block, Buffers, Server, Start, name_into};
use lemnos_hal::blocks::BufferKind;

use crate::engine::{Deinterlacer, Device, RunError, State, backoff_ns};
use crate::linux::LinuxDevice;

/// The block's name.
pub const NAME: &str = "sunxi-di";
/// The item that carries the job's settings.
pub const TAG_CONFIG: u32 = 1;
/// The item that says where one frame's planes are.
pub const TAG_FRAME: u32 = 2;
/// Bytes of a [`TAG_CONFIG`] item.
pub const CONFIG_BYTES: usize = 12;
/// Bytes of a [`TAG_FRAME`] item.
pub const FRAME_BYTES: usize = 24;
/// The most frames a job has: three inputs and three outputs.
pub const MAX_FRAMES: usize = 6;
/// What health says while the block is closed after a failure.
pub const REOPENING: &str = "the deinterlacer failed: it is opened again by a later job";

/// The mode byte of a [`TAG_CONFIG`] item.
pub mod mode {
    /// Both fields woven into one frame.
    pub const WEAVE: u8 = 0;
    /// One field interpolated to a frame.
    pub const BOB: u8 = 1;
    /// Motion-adaptive, one frame out.
    pub const MOTION: u8 = 2;
    /// Motion-adaptive, a frame for each field.
    pub const MOTION_FIELD_RATE: u8 = 3;
    /// Temporal noise reduction alone.
    pub const NOISE_ONLY: u8 = 4;
}

/// The flag bits of a [`TAG_CONFIG`] item.
pub mod flag {
    /// Film-mode detection, with the motion-adaptive modes.
    pub const FILM_DETECT: u8 = 1;
    /// The top field is the earlier one.
    pub const TOP_FIELD_FIRST: u8 = 2;
    /// The base field is the bottom one.
    pub const BASE_BOTTOM: u8 = 4;
}

/// A little-endian `u32` at `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

/// The format a DRM fourcc names, if the block takes it.
fn format(fourcc: u32) -> Option<Format> {
    [
        Format::Nv12,
        Format::Nv21,
        Format::Nv16,
        Format::Nv61,
        Format::Yuv420,
        Format::Yvu420,
        Format::Yuv422,
        Format::Yvu422,
    ]
    .into_iter()
    .find(|format| format.fourcc() == fourcc)
}

/// What a [`TAG_CONFIG`] item says: the configuration, and the two field flags.
fn config(bytes: &[u8]) -> Option<(Config, bool, Field)> {
    if bytes.len() != CONFIG_BYTES {
        return None;
    }
    let (width, height) = (word(bytes, 0)?, word(bytes, 4)?);
    let [which, flags, reduction, level] = [bytes[8], bytes[9], bytes[10], bytes[11]];
    if flags & !(flag::FILM_DETECT | flag::TOP_FIELD_FIRST | flag::BASE_BOTTOM) != 0 {
        return None;
    }
    let tnr = match (reduction, level) {
        (0, 0) => None,
        (1 | 2, 0..=2) => Some(Tnr {
            adaptive: reduction == 2,
            level: [Level::High, Level::Middle, Level::Low][usize::from(level)],
        }),
        _ => return None,
    };
    let film_detect = flags & flag::FILM_DETECT != 0;
    let mode = match (which, tnr, film_detect) {
        (mode::WEAVE, None, false) => Mode::Weave,
        (mode::BOB, None, false) => Mode::Bob,
        (mode::MOTION, tnr, film_detect) => Mode::Motion { film_detect, tnr },
        (mode::MOTION_FIELD_RATE, tnr, film_detect) => Mode::MotionFieldRate { film_detect, tnr },
        (mode::NOISE_ONLY, Some(tnr), false) => Mode::NoiseOnly(tnr),
        _ => return None,
    };
    let base = if flags & flag::BASE_BOTTOM != 0 {
        Field::Bottom
    } else {
        Field::Top
    };
    let config = Config::new(width, height, mode).ok()?;
    Some((config, flags & flag::TOP_FIELD_FIRST != 0, base))
}

/// The deinterlacer as `codec.block/1` has it, on the device `open` opens.
pub struct DiBlock<D: Device, F> {
    engine: Deinterlacer<D, F>,
}

impl<D: Device, F: FnMut() -> io::Result<D>> DiBlock<D, F> {
    /// The block `open` opens. Nothing is opened until a job comes.
    pub fn new(open: F) -> Self {
        Self {
            engine: Deinterlacer::new(open),
        }
    }

    /// The version the block answered when it was last opened.
    pub fn version(&self) -> Option<uapi::Version> {
        self.engine.version()
    }
}

impl<D: Device, F: FnMut() -> io::Result<D>> Block for DiBlock<D, F> {
    fn name(&self) -> &str {
        NAME
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: 512,
            max_refs: MAX_FRAMES as u16,
            max_items: 1 + MAX_FRAMES as u16,
            max_read_back: 1,
            max_buffers: lemnos_driver_codec_jobs::MAX_BUFFERS,
        }
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        let mut items = work.items();
        let Some((TAG_CONFIG, bytes)) = items.next() else {
            return Start::Refused;
        };
        let Some((config, top_field_first, base)) = config(bytes) else {
            return Start::Refused;
        };
        let (inputs, deinterlaced, reduced) = config.mode().frames();
        let count = inputs + deinterlaced + reduced;
        if work.ref_count() != count || work.item_count() != 1 + count {
            return Start::Refused;
        }
        // Each frame: its reference's buffer by handle, its item's layout, checked against
        // the dma-buf's real size.
        let mut frames = [None; MAX_FRAMES];
        for (at, (reference, (tag, bytes))) in work.refs().zip(items).enumerate() {
            if tag != TAG_FRAME
                || bytes.len() != FRAME_BYTES
                || reference.slot as usize != at
                || reference.offset != 0
                || reference.length != 0
            {
                return Start::Refused;
            }
            let (Some(handle), Some(format)) = (
                buffers.handle(reference.buffer),
                word(bytes, 0).and_then(format),
            ) else {
                return Start::Refused;
            };
            let field = |at: usize| word(bytes, at).unwrap_or(0);
            let layout = Layout {
                luma_stride: field(4),
                chroma_stride: field(8),
                luma_offset: u64::from(field(12)),
                chroma_offset: u64::from(field(16)),
                chroma2_offset: u64::from(field(20)),
            };
            let (width, height) = (config.settings().size.width, config.settings().size.height);
            let Ok(frame) = Frame::new(handle.fd(), format, width, height, layout) else {
                return Start::Refused;
            };
            let Some(slot) = frames.get_mut(at) else {
                return Start::Refused;
            };
            *slot = Some(frame);
        }
        if frames[..count].iter().any(Option::is_none) {
            return Start::Refused;
        }
        // The frames are all there, in order: split into the job's two lists.
        let Some(first) = frames[0] else {
            return Start::Refused;
        };
        let list = frames.map(|frame| frame.unwrap_or(first));
        let Ok(job) = Job::new(
            config,
            top_field_first,
            base,
            &list[..inputs],
            &list[inputs..count],
        ) else {
            return Start::Refused;
        };
        match self.engine.run(now_ns, &job) {
            Ok(processed) => {
                let Some(byte) = read_back.first_mut() else {
                    return Start::Failed;
                };
                *byte = u8::from(processed.progressive);
                Start::Done(1)
            }
            // The kernel refused the job or its settings as invalid: the job's, not the
            // block's.
            Err(RunError::Refused(_)) => Start::Refused,
            Err(_) => Start::Failed,
        }
    }

    fn release(&mut self) {
        // The kernel holds a job's buffers only for the length of the request.
    }

    fn health(&self) -> Health {
        match self.engine.state() {
            State::BackingOff { .. } => Health::Degraded(REOPENING),
            _ => Health::Ok,
        }
    }
}

/// The stream jobs come on: the need's name, and the name `job_stream` answers.
pub const JOB_STREAM: &str = "jobs";
/// What health says while the job stream fails.
pub const STREAM_FAILED: &str = "the job stream failed: trying it again";

/// The block as the driver holds it.
pub type BoxedBlock = Box<dyn Block + Send>;

/// The deinterlacer as `codec.block/1`.
///
/// Settings, from the device's `config` in the board file:
///
/// - `device` (string, default `/dev/deinterlace`): the block's node.
///
/// The stream `jobs` is required, with `memory = "dma-buf"`: the block takes frames only as
/// dma-bufs, so a stream of plain memory fails the bind with `NOT_SUPPORTED`. Nothing is
/// opened at bind.
#[driver("a733-di", provides = [CodecBlock])]
pub struct A733Di {
    #[needs]
    jobs: StreamHandle,
    block: BoxedBlock,
    server: Server,
    /// Stream errors in a row.
    stream_failures: u32,
    /// When the stream is tried again after an error.
    retry_at_ns: u64,
}

impl A733Di {
    /// The driver over `block`, for a test or a host that builds it directly.
    #[must_use]
    pub fn on(needs: A733DiNeeds, block: BoxedBlock) -> Self {
        Self {
            jobs: needs.jobs,
            block,
            server: Server::new(),
            stream_failures: 0,
            retry_at_ns: 0,
        }
    }
}

impl Lifecycle for A733Di {
    type Needs = A733DiNeeds;
    const TICKS: bool = true;

    fn start(needs: A733DiNeeds, cfg: &BoardConfig<'_>) -> Result<Self> {
        let path = PathBuf::from(cfg.str("device").unwrap_or(uapi::DEVICE));
        if path.as_os_str().is_empty() {
            return Err(Status::INVALID_ARGUMENT);
        }
        if needs.jobs.buffer_kind() != BufferKind::Handles {
            return Err(Status::NOT_SUPPORTED);
        }
        let block = DiBlock::new(move || LinuxDevice::open(&path));
        Ok(Self::on(needs, Box::new(block)))
    }

    fn health(&self) -> Health {
        if self.stream_failures > 0 {
            return Health::Degraded(STREAM_FAILED);
        }
        self.block.health()
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        if self.stream_failures > 0 && now_ns < self.retry_at_ns {
            return Some(self.retry_at_ns);
        }
        match self.server.serve(&mut *self.jobs, &mut *self.block, now_ns) {
            Ok(more) => {
                self.stream_failures = 0;
                Some(Server::next_tick_ns(more, now_ns))
            }
            Err(_) => {
                self.retry_at_ns = now_ns.saturating_add(backoff_ns(self.stream_failures));
                self.stream_failures = self.stream_failures.saturating_add(1);
                Some(self.retry_at_ns)
            }
        }
    }
}

impl CodecBlock for A733Di {
    fn info(&mut self) -> Result<BlockInfo> {
        Ok(Server::info(&*self.block))
    }

    fn block(&mut self, into: &mut [u8]) -> Result<usize> {
        name_into(self.block.name(), into)
    }

    fn job_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name_into(JOB_STREAM, into)
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.server.counters())
    }
}

#[cfg(test)]
mod tests;
