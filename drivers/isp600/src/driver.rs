//! The `camera.isp/1` and `isp.isp600/1` driver: the ISP600 run through the vendor kernel's `vin`
//! driver.
//!
//! The vendor kernel driver owns the block, its buffers and its pipeline. This driver selects the
//! capture node's input (which sets the pipeline up, the ISP included), sets the image format,
//! turns the kernel's statistics on, keeps the capture queue turning, and for each job carries
//! the caller's load image in and the statistics and image out. It does no 3A and holds no
//! tuning: a load image is bytes in the block's own layout ([`crate::layout`]), of which this
//! driver reads only the address words it checks.
//!
//! # Streams
//!
//! - `config`: jobs ([`IspJob`]) are posted on its ring, and its buffers hold load images. Every
//!   buffer of this stream stays with the driver: the caller writes an image into one, then posts
//!   a job naming it. The driver copies the image out **once**, into its own memory, when it
//!   takes the job; only that copy is checked and handed to the kernel.
//! - `statistics`: one buffer per finished job, filled with [`STATISTICS_BYTES`] bytes in the
//!   layout `isp.isp600/1` describes.
//! - `output0`: the image; a job names a buffer from those the stream has handed this driver.
//!
//! Everything published carries the job's identity, the kernel's frame count (widened to 64
//! bits) and, for images, the frame's timestamp.
//!
//! # A job, step by step
//!
//! The kernel keeps one load image waiting, and a second handed in before the block took the
//! first replaces it, so one job runs at a time; the others wait on the ring.
//!
//! 1. [`job::check`]: the layout is [`LAYOUT_LOAD`], the config buffer is on the config stream,
//!    the output buffer is on the output's stream and no other output is named. Then: the config
//!    buffer and the output buffer are ones this driver holds.
//! 2. The image is read from the config buffer into the driver's copy, exactly [`LOAD_BYTES`];
//!    [`layout::check_load`] passes it or the job is dropped; the kernel copies it.
//! 3. The kernel gives an image to the block at the block's next parameter load, which takes
//!    effect from the frame after, and statistics come ready a frame behind their frame. So the
//!    first `settle` statistics and the first `settle` images finished after the load are of
//!    earlier parameters and go by; the next of each are the job's, and are written into the
//!    job's stream buffers and published. Waiting a frame more than needed only adds a frame of
//!    latency: no other image can go in until the job is done.
//! 4. Statistics and images with no job waiting go straight back to the kernel. A part the kernel
//!    flagged, or that doesn't fit its stream buffer, is not delivered and the job counts as an
//!    error.
//!
//! A job that fails step 1 or 2 is counted as dropped. Bytes on the config stream's job queue
//! that are no job are counted as dropped too and, as the contract says of a broken consumer,
//! fault that consumer's config stream; the config buffers held under it are given up. Nothing
//! on the way allocates.
//!
//! # Settings
//!
//! From the device's `config` in the board file:
//!
//! - `capture`: the capture node's media entity name (the vendor kernel's `vin_video<n>`).
//!   Required.
//! - `isp`: the ISP subdevice's entity name (`sunxi_isp.<n>`). Required.
//! - `statistics`: the statistics subdevice's entity name (`sunxi_h3a.<n>`). Required. All three
//!   are looked for in one media graph.
//! - `input`: the capture node's input index, default 0.
//! - `buffers`: image buffers on the capture node, 2 to 32, default 4.
//! - `settle`: finished frames that go by after a load before a job's, 0 to 8, default
//!   [`DEFAULT_SETTLE`].
//!
//! # Failures
//!
//! A kernel node or a stream that errors starts recovery: the next ticks stop what's left and
//! find, open and start the nodes again, with the delay between attempts doubling from 250 ms and
//! capped at 8 s, and clearing when one succeeds. A job waiting when it failed counts as an
//! error; the stream buffers it named stay with the driver for later jobs. The driver never gives
//! up and never turns itself off; `stop` is what ends it. While recovering, health is degraded.

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, ConfigLayout, Counters, IspInfo, IspInput, IspJob, IspOutput,
    MAX_STREAM_NAME, OutputLayout,
};
use lemnos_contract_isp_isp600::{
    Isp600Info, IspIsp600, LAYOUT_LOAD, LAYOUT_STATISTICS, Region as WireRegion,
};
use lemnos_core::{
    Config, Decode, Description, Health, Item, Lifecycle, MethodId, Reader, Result, Status,
    StreamHandle, driver,
};
use lemnos_hal::blocks::StreamFrame;
use lemnos_v4l2::FourCc;

use crate::hw::{Frame, Hardware, LinuxHardware, Settings, Wanted};
use crate::job::{self, Limits};
use crate::layout::{
    self, LAYOUT_VERSION, LOAD_BYTES, LOAD_REGIONS, Region, STATISTICS_BYTES, STATISTICS_REGIONS,
};

const START: MethodId = MethodId {
    contract: 0,
    method: 10,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 11,
};

/// The stream jobs and load images come on: the need's name.
pub const CONFIG_STREAM: &str = "config";
/// The stream statistics go out on.
pub const STATISTICS_STREAM: &str = "statistics";
/// The stream images go out on.
pub const OUTPUT_STREAM: &str = "output0";

/// Most buffers a stream this driver uses may have: one bit each in a `u64`.
pub const MAX_STREAM_BUFFERS: u32 = 64;
/// Smallest image in each direction (`MIN_WIDTH`, `MIN_HEIGHT` of the vendor kernel).
pub const MIN_DIMENSION: u32 = 32;
/// Largest image in each direction (`MAX_WIDTH`, `MAX_HEIGHT` of the vendor kernel).
pub const MAX_DIMENSION: u32 = 4800;
/// Default `settle`: two frames finish on the earlier parameters, and statistics come one frame
/// behind.
pub const DEFAULT_SETTLE: u8 = 3;
/// Largest `settle`.
pub const MAX_SETTLE: u8 = 8;
/// Bytes of an encoded job at most: every field at its widest, with room.
const MAX_JOB_BYTES: usize = 128;
/// Statistics taken from the kernel in one tick at most: its most buffers (`STAT_MAX_BUFS`).
const MAX_STATISTICS_PER_TICK: usize = 6;
/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often the nodes and the streams are looked at while running.
const POLL_NS: u64 = 1_000_000;

/// The delay before recovery attempt `attempt` (from 0), doubling to the 8 s cap.
pub(crate) const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Running,
    Recovering {
        attempt: u32,
        /// When the next attempt is due; `None` until a tick has had the host's clock.
        due: Option<u64>,
    },
}

/// The image formats the capture node takes behind the ISP, one plane each: NV12 and NV21 at
/// 12 bits per pixel, NV16 at 16 (`vin-video/vin_core.c`).
const FORMATS: [(u32, u32); 3] = [
    (u32::from_le_bytes(*b"NV12"), 3),
    (u32::from_le_bytes(*b"NV21"), 3),
    (u32::from_le_bytes(*b"NV16"), 4),
];

/// Lines of `width` bytes, and `half_bytes_per_pixel / 2` bytes per pixel.
fn image_layout(width: u32, height: u32, fourcc: u32) -> Option<(u32, u32)> {
    let (_, halves) = FORMATS.iter().find(|(f, _)| *f == fourcc)?;
    let size = width.checked_mul(height)?.checked_mul(*halves)? / 2;
    Some((width, size))
}

fn dimension(v: u32) -> Result<u32> {
    if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&v) || !v.is_multiple_of(2) {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(v)
}

/// The driver's own settings, beside where the nodes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub input: u32,
    pub buffers: u32,
    pub settle: u8,
}

impl Options {
    /// `INVALID_ARGUMENT` for a buffer count or a settle out of range.
    pub fn new(input: u32, buffers: u32, settle: u8) -> Result<Self> {
        if !(2..=32).contains(&buffers) || settle > MAX_SETTLE {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            input,
            buffers,
            settle,
        })
    }
}

fn options(cfg: &Config<'_>) -> Result<Options> {
    let int = |key, max: i64, default: u32| match cfg.int(key) {
        Some(v) if (0..=max).contains(&v) => u32::try_from(v).map_err(|_| Status::INVALID_ARGUMENT),
        Some(_) => Err(Status::INVALID_ARGUMENT),
        None => Ok(default),
    };
    let settle = int("settle", i64::from(MAX_SETTLE), u32::from(DEFAULT_SETTLE))?;
    Options::new(
        int("input", i64::from(i32::MAX), 0)?,
        int("buffers", 32, 4)?,
        u8::try_from(settle).map_err(|_| Status::INVALID_ARGUMENT)?,
    )
}

/// A kernel count, widened to 64 bits across its wraps.
#[derive(Debug, Clone, Copy, Default)]
struct Sequence {
    last: Option<u32>,
    wraps: u64,
}

impl Sequence {
    fn widen(&mut self, sequence: u32) -> u64 {
        if self.last.is_some_and(|last| sequence < last) {
            self.wraps = self.wraps.wrapping_add(1);
        }
        self.last = Some(sequence);
        (self.wraps << 32) | u64::from(sequence)
    }
}

/// Whether bit `index` of `mask` is set; `false` past the mask.
const fn has(mask: u64, index: u32) -> bool {
    index < 64 && mask & (1 << index) != 0
}

/// `mask` with bit `index` set or cleared; unchanged past the mask.
const fn with(mask: u64, index: u32, set: bool) -> u64 {
    if index >= 64 {
        mask
    } else if set {
        mask | (1 << index)
    } else {
        mask & !(1 << index)
    }
}

/// The lowest set bit of `mask`.
const fn lowest(mask: u64) -> Option<u32> {
    if mask == 0 {
        None
    } else {
        Some(mask.trailing_zeros())
    }
}

/// The job whose image is with the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    identity: u64,
    /// Statistics still to let go by before the job's, and whether they are still wanted.
    statistics_settle: u8,
    statistics: bool,
    /// Images still to let go by before the job's, and the stream buffer still to fill.
    image_settle: u8,
    output: Option<u32>,
    /// A part was not delivered.
    missed: bool,
}

impl Pending {
    fn finished(&self) -> bool {
        !self.statistics && self.output.is_none()
    }
}

/// The output, as agreed in `set_output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Agreed {
    fourcc: u32,
    width: u32,
    height: u32,
    stride: u32,
    size: u32,
    /// Bytes a stream buffer holds for it: the image and the caller's padding.
    stream_bytes: u32,
}

/// The stream buffers this driver holds, one bit per buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Held {
    config: u64,
    statistics: u64,
    output: u64,
}

/// Takes the buffers `stream` has handed back into `mask`, at most one round of the stream.
fn take_returned(stream: &mut StreamHandle, mask: &mut u64) -> Result<()> {
    for _ in 0..MAX_STREAM_BUFFERS {
        let Some(buffer) = stream.next_buffer().map_err(|_| Status::IO)? else {
            break;
        };
        *mask = with(*mask, buffer, true);
    }
    Ok(())
}

/// Publishes `buffer` of `stream` holding `bytes`. `true` when it was published: the buffer is no
/// longer held.
fn publish(stream: &mut StreamHandle, mask: &mut u64, frame: StreamFrame) -> bool {
    if stream.publish(frame).is_err() {
        return false;
    }
    *mask = with(*mask, frame.buffer, false);
    true
}

/// The ISP600 as `camera.isp/1` and `isp.isp600/1`.
#[driver("isp600", provides = [CameraIsp, IspIsp600])]
pub struct Isp600 {
    #[needs]
    config: StreamHandle,
    #[needs]
    statistics: StreamHandle,
    #[needs]
    output0: StreamHandle,
    hw: Box<dyn Hardware>,
    options: Options,
    input: Option<IspInput>,
    output: Option<Agreed>,
    state: State,
    counters: Counters,
    /// What every job is checked against, set at `start`.
    limits: Option<Limits>,
    held: Held,
    pending: Option<Pending>,
    statistics_sequence: Sequence,
    image_sequence: Sequence,
    /// The driver's copy of a job's load image.
    image: Box<[u8; LOAD_BYTES]>,
    /// Statistics as the kernel copied them out.
    stats: Box<[u8; STATISTICS_BYTES]>,
}

impl Lifecycle for Isp600 {
    type Needs = Isp600Needs;
    const TICKS: bool = true;

    fn start(needs: Isp600Needs, cfg: &Config<'_>) -> Result<Self> {
        let name = |key| cfg.str(key).ok_or(Status::INVALID_ARGUMENT);
        let settings = Settings::new(name("capture")?, name("isp")?, name("statistics")?)?;
        let options = options(cfg)?;
        Ok(Self::with_hardware(
            Box::new(LinuxHardware::new(settings)),
            needs,
            options,
        ))
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } => {
                Health::Degraded("the ISP's nodes failed while running: opening them again")
            }
            _ => Health::Ok,
        }
    }

    /// Carries jobs and finished buffers when running, and runs a due recovery attempt.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Running => {
                if self.pump().is_err() {
                    self.fail();
                    return self.tick(now_ns);
                }
                Some(now_ns.saturating_add(POLL_NS))
            }
            State::Recovering { attempt, due: None } => {
                let at = now_ns.saturating_add(backoff_ns(attempt));
                self.state = State::Recovering {
                    attempt,
                    due: Some(at),
                };
                Some(at)
            }
            State::Recovering { due: Some(at), .. } if now_ns < at => Some(at),
            State::Recovering { attempt, .. } => match self.begin() {
                Ok(()) => {
                    self.state = State::Running;
                    Some(now_ns.saturating_add(POLL_NS))
                }
                Err(_) => {
                    let attempt = attempt.saturating_add(1);
                    let at = now_ns.saturating_add(backoff_ns(attempt));
                    self.state = State::Recovering {
                        attempt,
                        due: Some(at),
                    };
                    Some(at)
                }
            },
        }
    }
}

impl Isp600 {
    /// A driver over `hw` with the streams in `needs`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, needs: Isp600Needs, options: Options) -> Self {
        Self {
            config: needs.config,
            statistics: needs.statistics,
            output0: needs.output0,
            hw,
            options,
            input: None,
            output: None,
            state: State::Stopped,
            counters: Counters::default(),
            limits: None,
            held: Held::default(),
            pending: None,
            statistics_sequence: Sequence::default(),
            image_sequence: Sequence::default(),
            image: Box::new([0; LOAD_BYTES]),
            stats: Box::new([0; STATISTICS_BYTES]),
        }
    }

    fn stopped(&self) -> Result<()> {
        if self.state == State::Stopped {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
    }

    /// Ends the waiting job as an error: its frame will not come.
    fn abandon(&mut self) {
        if self.pending.take().is_some() {
            self.counters.errors = self.counters.errors.saturating_add(1);
        }
    }

    fn fail(&mut self) {
        self.hw.stop();
        self.abandon();
        self.state = State::Recovering {
            attempt: 0,
            due: None,
        };
    }

    /// Opens and starts the nodes from nothing.
    fn begin(&mut self) -> Result<()> {
        self.hw.stop();
        self.abandon();
        let output = self.output.ok_or(Status::INVALID_ARGUMENT)?;
        let fourcc = FourCc::new(&output.fourcc.to_le_bytes());
        let granted = self.hw.open(&Wanted {
            input: self.options.input,
            fourcc,
            width: output.width,
            height: output.height,
            stride: output.stride,
            size: output.size,
        })?;
        let as_agreed = granted.fourcc == fourcc
            && granted.width == output.width
            && granted.height == output.height
            && granted.stride == output.stride
            && granted.size >= output.size;
        if !as_agreed {
            self.hw.stop();
            return Err(Status::NOT_SUPPORTED);
        }
        if let Err(status) = self.hw.stream_on(self.options.buffers) {
            self.hw.stop();
            return Err(status);
        }
        self.statistics_sequence = Sequence::default();
        self.image_sequence = Sequence::default();
        Ok(())
    }

    /// Stream buffers back, finished kernel buffers out, a new job in. An error means a kernel
    /// node or a stream failed.
    fn pump(&mut self) -> Result<()> {
        take_returned(&mut self.config, &mut self.held.config)?;
        take_returned(&mut self.statistics, &mut self.held.statistics)?;
        take_returned(&mut self.output0, &mut self.held.output)?;
        self.drain_statistics()?;
        self.drain_images()?;
        self.retire();
        if self.pending.is_none() {
            self.take_job()?;
        }
        Ok(())
    }

    /// Takes every statistics buffer the kernel finished, and gives the waiting job its own.
    fn drain_statistics(&mut self) -> Result<()> {
        for _ in 0..MAX_STATISTICS_PER_TICK {
            let Some(got) = self.hw.statistics(&mut self.stats)? else {
                break;
            };
            let sequence = self.statistics_sequence.widen(got.frame_number);
            let Some(p) = self.pending.as_mut().filter(|p| p.statistics) else {
                continue;
            };
            if p.statistics_settle > 0 {
                p.statistics_settle -= 1;
                continue;
            }
            let delivered = match (
                lowest(self.held.statistics),
                job::filled(&self.stats[..], got.bytes, STATISTICS_BYTES),
            ) {
                (Some(buffer), Some(data)) => {
                    let frame = StreamFrame {
                        buffer,
                        bytes: 0,
                        sequence,
                        identity: p.identity,
                        timestamp_ns: 0,
                    };
                    write_and_publish(
                        &mut self.statistics,
                        &mut self.held.statistics,
                        frame,
                        &[data],
                    )
                }
                _ => false,
            };
            p.statistics = false;
            p.missed |= !delivered;
        }
        Ok(())
    }

    /// Takes every image the capture node filled, gives the waiting job its own, and gives each
    /// back to the node.
    fn drain_images(&mut self) -> Result<()> {
        for _ in 0..self.options.buffers {
            let Some(frame) = self.hw.dequeue()? else {
                break;
            };
            self.image_done(&frame);
            self.hw.requeue(frame.index)?;
        }
        Ok(())
    }

    /// One filled image.
    fn image_done(&mut self, frame: &Frame) {
        let sequence = self.image_sequence.widen(frame.sequence);
        let Some(p) = self.pending.as_mut() else {
            return;
        };
        let Some(buffer) = p.output else {
            return;
        };
        if p.image_settle > 0 {
            p.image_settle -= 1;
            return;
        }
        p.output = None;
        let limit = self.output.map_or(0, |o| o.stream_bytes as usize);
        if frame.bad || frame.planes > MAX_PLANES {
            p.missed = true;
            return;
        }
        let mut planes: [&[u8]; MAX_PLANES] = [&[]; MAX_PLANES];
        let mut total = 0usize;
        for (plane, slot) in planes.iter_mut().enumerate().take(frame.planes) {
            let Ok(bytes) = self.hw.payload(frame, plane) else {
                p.missed = true;
                return;
            };
            total = total.saturating_add(bytes.len());
            *slot = bytes;
        }
        if total > limit {
            p.missed = true;
            return;
        }
        let published = StreamFrame {
            buffer,
            bytes: 0,
            sequence,
            identity: p.identity,
            timestamp_ns: frame.timestamp_ns,
        };
        let delivered = write_and_publish(
            &mut self.output0,
            &mut self.held.output,
            published,
            &planes[..frame.planes],
        );
        p.missed |= !delivered;
    }

    /// Counts and forgets the job when it has nothing left to deliver.
    fn retire(&mut self) {
        let Some(p) = self.pending.filter(Pending::finished) else {
            return;
        };
        let counter = if p.missed {
            &mut self.counters.errors
        } else {
            &mut self.counters.jobs
        };
        *counter = counter.saturating_add(1);
        self.pending = None;
    }

    /// Takes posted jobs until one is sent, counting each it can't send as dropped.
    fn take_job(&mut self) -> Result<()> {
        let mut encoded = [0u8; MAX_JOB_BYTES];
        // At most a round of the ring per tick; the rest wait for the next.
        for _ in 0..MAX_STREAM_BUFFERS {
            let Some(len) = self.config.next_job(&mut encoded).map_err(|_| Status::IO)? else {
                break;
            };
            let job = encoded
                .get(..len)
                .and_then(|bytes| IspJob::decode(&mut Reader::new(bytes)).ok());
            let Some(job) = job else {
                // Bytes that are no job: a broken consumer, which loses the config stream.
                self.counters.dropped = self.counters.dropped.saturating_add(1);
                return self.fault_config();
            };
            let sent = self.submit(&job)?;
            if sent {
                break;
            }
            self.counters.dropped = self.counters.dropped.saturating_add(1);
        }
        Ok(())
    }

    /// Faults the config stream's consumer. The config buffers held were taken under it and
    /// have nothing behind them now: each is given up, to be taken again under the next one.
    fn fault_config(&mut self) -> Result<()> {
        self.config.fault_consumer().map_err(|_| Status::IO)?;
        for buffer in 0..MAX_STREAM_BUFFERS {
            if !has(self.held.config, buffer) {
                continue;
            }
            // Nothing is behind it, so this reaches no one.
            self.config
                .publish(StreamFrame {
                    buffer,
                    bytes: 0,
                    sequence: 0,
                    identity: 0,
                    timestamp_ns: 0,
                })
                .map_err(|_| Status::IO)?;
            self.held.config = with(self.held.config, buffer, false);
        }
        Ok(())
    }

    /// Sends one job's load image to the kernel. `false` for a job that can't be sent, which is
    /// the caller's mistake; an error means the kernel's node failed.
    fn submit(&mut self, job: &IspJob) -> Result<bool> {
        let Some(limits) = self.limits.as_ref() else {
            return Ok(false);
        };
        let Some(checked) = job::check(job, limits) else {
            return Ok(false);
        };
        if !has(self.held.config, checked.config_buffer) {
            return Ok(false);
        }
        if let Some(buffer) = checked.output
            && (self.output.is_none() || !has(self.held.output, buffer))
        {
            return Ok(false);
        }
        // The one read of the caller's buffer. Everything after uses the copy.
        match self
            .config
            .read(checked.config_buffer, 0, &mut self.image[..])
        {
            Ok(true) => {}
            Ok(false) | Err(_) => return Ok(false),
        }
        let Ok(image) = layout::check_load(&self.image[..]) else {
            return Ok(false);
        };
        self.hw.load(image)?;
        let settle = self.options.settle;
        self.pending = Some(Pending {
            identity: checked.identity,
            statistics_settle: settle,
            statistics: true,
            image_settle: settle,
            output: checked.output,
            missed: false,
        });
        Ok(true)
    }
}

/// Most planes an image buffer has: the formats here have one; room for a two-plane node.
const MAX_PLANES: usize = 2;

/// Writes `parts` one after another into `frame.buffer` of `stream` and publishes it with their
/// length. `true` when it reached a consumer; the buffer is no longer held when it was published,
/// delivered or not.
fn write_and_publish(
    stream: &mut StreamHandle,
    mask: &mut u64,
    frame: StreamFrame,
    parts: &[&[u8]],
) -> bool {
    let mut offset = 0usize;
    let mut reached = true;
    for part in parts {
        match stream.write(frame.buffer, offset, part) {
            Ok(true) => offset = offset.saturating_add(part.len()),
            // Nobody to land on: hand the buffer back empty.
            Ok(false) => {
                reached = false;
                offset = 0;
                break;
            }
            Err(_) => return false,
        }
    }
    let Ok(bytes) = u32::try_from(offset) else {
        return false;
    };
    publish(stream, mask, StreamFrame { bytes, ..frame }) && reached
}

/// Writes a stream name into `into`.
fn name(name: &str, into: &mut [u8]) -> Result<usize> {
    let bytes = name.as_bytes();
    into.get_mut(..bytes.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(bytes);
    Ok(bytes.len())
}

const _: () = {
    assert!(CONFIG_STREAM.len() <= MAX_STREAM_NAME);
    assert!(STATISTICS_STREAM.len() <= MAX_STREAM_NAME);
    assert!(OUTPUT_STREAM.len() <= MAX_STREAM_NAME);
    assert!(LOAD_BYTES <= u32::MAX as usize);
    assert!(STATISTICS_BYTES <= u32::MAX as usize);
};

impl CameraIsp for Isp600 {
    fn info(&mut self) -> Result<IspInfo> {
        Ok(IspInfo {
            max_outputs: 1,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
            config_layouts: 1,
            statistics_layout: LAYOUT_STATISTICS,
            statistics_bytes: STATISTICS_BYTES as u32,
        })
    }

    fn config_layout(&mut self, index: u8) -> Result<ConfigLayout> {
        if index != 0 {
            return Err(Status::NOT_FOUND);
        }
        Ok(ConfigLayout {
            layout: LAYOUT_LOAD,
            bytes: LOAD_BYTES as u32,
        })
    }

    fn set_input(&mut self, input: IspInput) -> Result<()> {
        self.stopped()?;
        if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&input.width)
            || !(MIN_DIMENSION..=MAX_DIMENSION).contains(&input.height)
            || !matches!(input.bit_depth, 8 | 10 | 12 | 14 | 16)
            || input.bayer == BayerOrder::Mono
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.input = Some(input);
        Ok(())
    }

    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout> {
        if output != 0 {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        let width = dimension(image.width)?;
        let height = dimension(image.height)?;
        let (stride, size) =
            image_layout(width, height, image.fourcc).ok_or(Status::INVALID_ARGUMENT)?;
        let stream_bytes = size
            .checked_add(image.pad_bytes)
            .ok_or(Status::INVALID_ARGUMENT)?;
        self.output = Some(Agreed {
            fourcc: image.fourcc,
            width,
            height,
            stride,
            size,
            stream_bytes,
        });
        Ok(OutputLayout {
            stride,
            size: stream_bytes,
        })
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        if output != 0 {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        self.output = None;
        Ok(())
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(CONFIG_STREAM, into)
    }

    fn statistics_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(STATISTICS_STREAM, into)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // Frames come straight from the receiver, inside the kernel.
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, output: u8, into: &mut [u8]) -> Result<usize> {
        if output != 0 {
            return Err(Status::NOT_FOUND);
        }
        name(OUTPUT_STREAM, into)
    }

    fn start(&mut self) -> Result<()> {
        // The capture node always runs behind the ISP, so it needs its image set.
        if self.input.is_none() || self.output.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.stopped()?;
        let count = |n: u32| {
            if n > MAX_STREAM_BUFFERS {
                Err(Status::INVALID_ARGUMENT)
            } else {
                Ok(n)
            }
        };
        let config_buffers = count(self.config.buffer_count())?;
        count(self.statistics.buffer_count())?;
        let output_buffers = count(self.output0.buffer_count())?;
        self.limits = Some(Limits {
            config_buffers,
            output_buffers,
        });
        self.counters = Counters::default();
        self.begin()?;
        self.state = State::Running;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.state = State::Stopped;
        self.hw.stop();
        self.abandon();
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }
}

fn wire(region: Option<&Region>) -> Result<WireRegion> {
    let region = region.ok_or(Status::NOT_FOUND)?;
    Ok(WireRegion {
        offset: u32::try_from(region.offset).map_err(|_| Status::IO)?,
        bytes: u32::try_from(region.bytes).map_err(|_| Status::IO)?,
    })
}

impl IspIsp600 for Isp600 {
    fn info(&mut self) -> Result<Isp600Info> {
        Ok(Isp600Info {
            layout_version: LAYOUT_VERSION,
            load_bytes: LOAD_BYTES as u32,
            statistics_bytes: STATISTICS_BYTES as u32,
            load_regions: LOAD_REGIONS.len() as u8,
            statistics_regions: STATISTICS_REGIONS.len() as u8,
            settle_frames: self.options.settle,
        })
    }

    fn load_region(&mut self, region: u8) -> Result<WireRegion> {
        wire(LOAD_REGIONS.get(usize::from(region)))
    }

    fn statistics_region(&mut self, region: u8) -> Result<WireRegion> {
        wire(STATISTICS_REGIONS.get(usize::from(region)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_ignore_bits_past_their_width() {
        assert!(!has(u64::MAX, 64));
        assert!(!has(u64::MAX, u32::MAX));
        assert_eq!(with(0, 64, true), 0);
        assert_eq!(with(0, 63, true), 1 << 63);
        assert_eq!(lowest(0b1000), Some(3));
        assert_eq!(lowest(0), None);
    }

    #[test]
    fn image_layouts_are_the_formats() {
        let nv12 = u32::from_le_bytes(*b"NV12");
        let nv16 = u32::from_le_bytes(*b"NV16");
        assert_eq!(image_layout(64, 32, nv12), Some((64, 3072)));
        assert_eq!(image_layout(64, 32, nv16), Some((64, 4096)));
        assert_eq!(image_layout(64, 32, u32::from_le_bytes(*b"RG10")), None);
        assert_eq!(image_layout(u32::MAX, 2, nv12), None, "overflow");
    }

    #[test]
    fn counts_widen_across_the_wrap() {
        let mut s = Sequence::default();
        assert_eq!(s.widen(u32::MAX), u64::from(u32::MAX));
        assert_eq!(s.widen(0), 1 << 32);
        assert_eq!(s.widen(1), (1 << 32) | 1);
    }
}
