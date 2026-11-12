//! The `camera.isp/1` driver: the vendor kernel's ISP run through its stock nodes.
//!
//! The vendor kernel driver owns the block and its buffers. This driver links the ISP's path in
//! its media graph ([`graph::plan`]), sets the formats, keeps the kernel's statistics and image
//! queues turning, and for each job carries the caller's parameters in and the statistics and
//! images out. It does no 3A and holds no tuning: a parameter buffer is bytes in the layout
//! [`Generation::config_layout`] names, which this driver never reads past its size.
//!
//! # Streams
//!
//! - `config`: jobs ([`IspJob`]) are posted on its ring, and its buffers hold parameters. Every
//!   buffer of this stream stays with the driver: the caller writes parameters into one, then
//!   posts a job naming it. The driver copies the parameters out **once**, into its own memory,
//!   when it takes the job, and only that copy is checked and handed to the kernel. The caller
//!   may change the buffer at any time afterwards; the job's statistics coming back with its
//!   identity say the copy was taken.
//! - `statistics`: one buffer per finished job, filled with the generation's statistics layout.
//! - `output0` (the main path) and `output1` (the self path, optional): a job names a buffer on
//!   each output it wants, from those the stream has handed this driver and no other job waiting
//!   has named.
//!
//! Everything published carries the job's identity, the kernel's frame sequence (widened to
//! 64 bits) and the frame's timestamp.
//!
//! # A job, step by step
//!
//! 1. [`job::check`]: the layout is the generation's, the config buffer is on the config
//!    stream, each output buffer is on a stream of an output that is on. Then: the config
//!    buffer is one this driver holds, each output buffer is one it holds and no waiting job
//!    named, a kernel parameter buffer is free, and fewer than [`MAX_PENDING`] jobs wait.
//! 2. The parameters are read from the config buffer into the driver's copy, exactly the
//!    generation's size; [`job::copy_params`] puts that copy into the kernel's parameter
//!    buffer, which is queued.
//! 3. When the kernel gives the parameter buffer back, its sequence names the frame the
//!    parameters were applied to. The statistics and images of that frame are the job's:
//!    [`job::filled`] bounds each by the kernel's mapping and by the stream buffer, and each is
//!    written into its stream buffer and published.
//! 4. A frame with no job waiting for it goes straight back to the kernel. A part of a job
//!    whose frame has passed, or that the kernel flagged, is not delivered and the job counts
//!    as an error.
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
//! - `generation`: `isp21`, `isp30` or `isp35`. Required.
//! - `device`: the ISP's media device driver name (`rkisp-vir0`). With only one ISP on the
//!   machine, it may be left out.
//! - `source`: the entity that should feed the ISP (a receiver's subdevice). Default: whatever
//!   feeds it now.
//! - `buffers`: statistics and image buffers per kernel queue, 2 to 32, default 4.
//!
//! # Failures
//!
//! A kernel queue or a stream that errors starts recovery: the next ticks stop what's left and
//! link, set up and start the path again, with the delay between attempts doubling from 250 ms
//! and capped at 8 s, and clearing when one succeeds. Jobs waiting when it failed count as
//! errors; the stream buffers they named stay with the driver for later jobs. The driver never
//! gives up and never turns itself off; `stop` is what ends it. While recovering, health is
//! degraded.

use lemnos_contract_camera_isp::{
    BayerOrder, CameraIsp, ConfigLayout, Counters, IspInfo, IspInput, IspJob, IspOutput,
    MAX_STREAM_NAME, OutputLayout,
};
use lemnos_core::{
    Config, Decode, Description, Health, Item, Lifecycle, MethodId, Reader, Result, Status,
    StreamHandle, driver,
};
use lemnos_hal::blocks::StreamFrame;
use lemnos_v4l2::subdev::{MbusCode, MbusFormat};

use crate::generation::{Generation, OUTPUTS};
use crate::graph;
use crate::hw::{Done, Hardware, Image, LinuxHardware, QUEUES, Queue, Settings, Setup};
use crate::job::{self, Limits};

const START: MethodId = MethodId {
    contract: 0,
    method: 10,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 11,
};

/// The stream jobs and parameters come on: the need's name.
pub const CONFIG_STREAM: &str = "config";
/// The stream statistics go out on.
pub const STATISTICS_STREAM: &str = "statistics";
/// The streams each output's images go out on.
pub const OUTPUT_STREAMS: [&str; OUTPUTS as usize] = ["output0", "output1"];

/// Jobs that may wait for their frame at once, and the kernel parameter buffers asked for.
pub const MAX_PENDING: usize = 8;
/// Most buffers a stream this driver uses may have: one bit each in a `u64`.
pub const MAX_STREAM_BUFFERS: u32 = 64;
/// The largest raw frame or image in each direction this driver passes on; the kernel refuses
/// what its ISP can't take when the path starts.
pub const MAX_DIMENSION: u32 = 8192;
/// Bytes of an encoded job at most: every field at its widest, with room.
const MAX_JOB_BYTES: usize = 128;
/// Jobs taken from the ring in one tick at most.
const MAX_JOBS_PER_TICK: usize = 64;
/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often the kernel queues and the streams are looked at while running.
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

/// The image formats an output takes: NV12 and NV16, chroma after luma in one plane.
const FORMATS: [u32; 2] = [u32::from_le_bytes(*b"NV12"), u32::from_le_bytes(*b"NV16")];

/// The media bus code of raw frames of `depth` bits in `order` (`media-bus-format.h`).
fn raw_code(order: BayerOrder, depth: u8) -> Option<MbusCode> {
    let name = match (order, depth) {
        (BayerOrder::Mono, 8) => "Y8_1X8",
        (BayerOrder::Mono, 10) => "Y10_1X10",
        (BayerOrder::Mono, 12) => "Y12_1X12",
        (BayerOrder::Rggb, 8) => "SRGGB8_1X8",
        (BayerOrder::Rggb, 10) => "SRGGB10_1X10",
        (BayerOrder::Rggb, 12) => "SRGGB12_1X12",
        (BayerOrder::Grbg, 8) => "SGRBG8_1X8",
        (BayerOrder::Grbg, 10) => "SGRBG10_1X10",
        (BayerOrder::Grbg, 12) => "SGRBG12_1X12",
        (BayerOrder::Gbrg, 8) => "SGBRG8_1X8",
        (BayerOrder::Gbrg, 10) => "SGBRG10_1X10",
        (BayerOrder::Gbrg, 12) => "SGBRG12_1X12",
        (BayerOrder::Bggr, 8) => "SBGGR8_1X8",
        (BayerOrder::Bggr, 10) => "SBGGR10_1X10",
        (BayerOrder::Bggr, 12) => "SBGGR12_1X12",
        _ => return None,
    };
    MbusCode::from_name(name)
}

fn dimension(v: u32) -> Result<u32> {
    if v == 0 || v > MAX_DIMENSION {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(v)
}

/// The driver's own settings, beside where the ISP is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub generation: Generation,
    /// The entity that should feed the ISP, or `None` for whatever feeds it.
    pub source: Option<String>,
    /// Statistics and image buffers per kernel queue.
    pub buffers: u32,
}

impl Options {
    /// `INVALID_ARGUMENT` for a buffer count out of range or an empty source name.
    pub fn new(generation: Generation, source: Option<&str>, buffers: u32) -> Result<Self> {
        if !(2..=32).contains(&buffers) || source.is_some_and(str::is_empty) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            generation,
            source: source.map(str::to_owned),
            buffers,
        })
    }
}

fn options(cfg: &Config<'_>) -> Result<Options> {
    let generation = cfg
        .str("generation")
        .and_then(Generation::from_name)
        .ok_or(Status::INVALID_ARGUMENT)?;
    let buffers = match cfg.int("buffers") {
        Some(v) => u32::try_from(v).map_err(|_| Status::INVALID_ARGUMENT)?,
        None => 4,
    };
    Options::new(generation, cfg.str("source"), buffers)
}

/// A kernel sequence number, widened to 64 bits across its wraps.
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

/// Whether frame `a` came before frame `b`, across the sequence's wrap.
const fn before(a: u32, b: u32) -> bool {
    let ahead = b.wrapping_sub(a);
    ahead != 0 && ahead <= u32::MAX / 2
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

/// A job whose parameters are with the kernel, waiting for its frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    identity: u64,
    /// When it was taken, to find the oldest.
    order: u64,
    /// The kernel parameter buffer carrying its parameters.
    slot: u32,
    /// The frame the kernel applied them to, once the parameter buffer is back.
    sequence: Option<u32>,
    /// Statistics still to deliver.
    statistics: bool,
    /// For each output, the stream buffer still to fill.
    outputs: [Option<u32>; OUTPUTS as usize],
    /// A part was not delivered.
    missed: bool,
}

impl Pending {
    fn wants(&self, part: Part) -> bool {
        match part {
            Part::Statistics => self.statistics,
            Part::Output(o) => self.outputs.get(o).copied().flatten().is_some(),
        }
    }

    fn settle(&mut self, part: Part, delivered: bool) {
        match part {
            Part::Statistics => self.statistics = false,
            Part::Output(o) => {
                if let Some(slot) = self.outputs.get_mut(o) {
                    *slot = None;
                }
            }
        }
        self.missed |= !delivered;
    }

    fn finished(&self) -> bool {
        !self.statistics && self.outputs.iter().all(Option::is_none)
    }
}

/// What a finished kernel buffer is to a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Statistics,
    Output(usize),
}

/// One output, as agreed in `set_output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Agreed {
    /// What the kernel answered.
    image: Image,
    /// Bytes a stream buffer holds for it: the image and the caller's padding.
    stream_bytes: u32,
}

/// The stream buffers this driver holds, one bit per buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Held {
    config: u64,
    statistics: u64,
    outputs: [u64; OUTPUTS as usize],
}

/// The output stream `output`, if there is one.
fn output_stream<'a>(
    output0: &'a mut StreamHandle,
    output1: &'a mut Option<StreamHandle>,
    output: usize,
) -> Option<&'a mut StreamHandle> {
    match output {
        0 => Some(output0),
        1 => output1.as_mut(),
        _ => None,
    }
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

/// Writes `data` into `buffer` of `stream` and publishes it. `true` when it reached a consumer;
/// the buffer is no longer held when it was published, delivered or not.
fn deliver(
    stream: &mut StreamHandle,
    mask: &mut u64,
    buffer: u32,
    data: &[u8],
    frame: StreamFrame,
) -> bool {
    let Ok(bytes) = u32::try_from(data.len()) else {
        return false;
    };
    let (bytes, reached) = match stream.write(buffer, 0, data) {
        Ok(true) => (bytes, true),
        // Nobody to land on: hand the buffer back empty.
        Ok(false) => (0, false),
        Err(_) => return false,
    };
    if stream.publish(StreamFrame { bytes, ..frame }).is_err() {
        return false;
    }
    *mask = with(*mask, buffer, false);
    reached
}

/// The vendor kernel's ISP as `camera.isp/1`.
#[driver("rkisp", provides = [CameraIsp])]
pub struct Rkisp {
    #[needs]
    config: StreamHandle,
    #[needs]
    statistics: StreamHandle,
    #[needs]
    output0: StreamHandle,
    #[needs(optional)]
    output1: Option<StreamHandle>,
    hw: Box<dyn Hardware>,
    options: Options,
    input: Option<IspInput>,
    outputs: [Option<Agreed>; OUTPUTS as usize],
    state: State,
    counters: Counters,
    /// What every job is checked against, set at `start`.
    limits: Option<Limits>,
    held: Held,
    pending: [Option<Pending>; MAX_PENDING],
    /// Jobs taken, for [`Pending::order`].
    taken: u64,
    /// Kernel parameter buffers the kernel doesn't hold.
    params_free: u64,
    /// Buffers on each kernel queue.
    kernel: [u32; QUEUES],
    sequence: [Sequence; QUEUES],
    /// The driver's copy of a job's parameters: exactly the generation's size.
    params: Box<[u8]>,
}

impl Lifecycle for Rkisp {
    type Needs = RkispNeeds;
    const TICKS: bool = true;

    fn start(needs: RkispNeeds, cfg: &Config<'_>) -> Result<Self> {
        let settings = Settings::new(cfg.str("device"))?;
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
                Health::Degraded("the ISP's path failed while running: setting it up again")
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

impl Rkisp {
    /// A driver over `hw` with the streams in `needs`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, needs: RkispNeeds, options: Options) -> Self {
        let params_bytes = options.generation.config_layout().bytes as usize;
        Self {
            config: needs.config,
            statistics: needs.statistics,
            output0: needs.output0,
            output1: needs.output1,
            hw,
            options,
            input: None,
            outputs: [None; OUTPUTS as usize],
            state: State::Stopped,
            counters: Counters::default(),
            limits: None,
            held: Held::default(),
            pending: [None; MAX_PENDING],
            taken: 0,
            params_free: 0,
            kernel: [0; QUEUES],
            sequence: [Sequence::default(); QUEUES],
            params: vec![0; params_bytes].into_boxed_slice(),
        }
    }

    /// Outputs this ISP has: the self path needs its stream.
    fn output_count(&self) -> usize {
        if self.output1.is_some() { 2 } else { 1 }
    }

    fn stopped(&self) -> Result<()> {
        if self.state == State::Stopped {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
    }

    /// Ends every waiting job as an error: its frame will not come.
    fn abandon(&mut self) {
        for slot in &mut self.pending {
            if slot.take().is_some() {
                self.counters.errors = self.counters.errors.saturating_add(1);
            }
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

    /// Links, sets up and starts the path from nothing.
    fn begin(&mut self) -> Result<()> {
        self.hw.stop();
        self.abandon();
        let input = self.input.ok_or(Status::INVALID_ARGUMENT)?;
        let code = raw_code(input.bayer, input.bit_depth).ok_or(Status::INVALID_ARGUMENT)?;
        let t = self.hw.topology()?;
        let plan = graph::plan(
            &t,
            self.options.source.as_deref(),
            self.outputs[1].is_some(),
        )?;
        for (source, sink) in &plan.disable {
            self.hw.setup_link(*source, *sink, false)?;
        }
        for (source, sink) in &plan.enable {
            self.hw.setup_link(*source, *sink, true)?;
        }
        let raw = MbusFormat {
            width: input.width,
            height: input.height,
            code,
            field: 1,
            ..Default::default()
        };
        self.hw.set_formats(&t, plan.source, plan.nodes.isp, raw)?;
        let generation = self.options.generation;
        let setup = Setup {
            nodes: plan.nodes,
            params_bytes: generation.config_layout().bytes,
            statistics_bytes: generation.statistics_layout().bytes,
            params_buffers: MAX_PENDING as u32,
            buffers: self.options.buffers,
            outputs: self.outputs.map(|o| o.map(|a| a.image)),
        };
        let opened = self.hw.open(&t, &setup)?;
        // The kernel must lay each image out as it said it would in `set_output`.
        let as_agreed = self
            .outputs
            .iter()
            .zip(opened.outputs)
            .all(|(agreed, got)| match (agreed, got) {
                (None, _) => true,
                (Some(a), Some(got)) => {
                    (got.width, got.height, got.fourcc, got.stride)
                        == (
                            a.image.width,
                            a.image.height,
                            a.image.fourcc,
                            a.image.stride,
                        )
                }
                (Some(_), None) => false,
            });
        let params = opened.buffers[Queue::Params as usize];
        if !as_agreed || params == 0 || opened.buffers[Queue::Statistics as usize] == 0 {
            self.hw.stop();
            return Err(Status::NOT_SUPPORTED);
        }
        // Only as many as there are waiting jobs to use them.
        let usable = params.min(MAX_PENDING as u32);
        self.params_free = (1u64 << usable) - 1;
        self.kernel = opened.buffers;
        self.sequence = [Sequence::default(); QUEUES];
        Ok(())
    }

    /// Stream buffers back, finished kernel buffers out, new jobs in. An error means a kernel
    /// queue or a stream failed.
    fn pump(&mut self) -> Result<()> {
        self.take_returns()?;
        self.drain_params()?;
        for queue in [Queue::Statistics, Queue::Main, Queue::SelfPath] {
            self.drain(queue)?;
        }
        self.retire();
        self.take_jobs()
    }

    /// Takes back every parameter buffer the kernel finished, and learns from each which frame
    /// its job's results are.
    fn drain_params(&mut self) -> Result<()> {
        for _ in 0..self.kernel[Queue::Params as usize] {
            let Some(done) = self.hw.dequeue(Queue::Params)? else {
                break;
            };
            if done.index >= self.kernel[Queue::Params as usize] {
                return Err(Status::IO);
            }
            self.params_free = with(self.params_free, done.index, true);
            let job = self
                .pending
                .iter_mut()
                .flatten()
                .find(|p| p.slot == done.index && p.sequence.is_none());
            if let Some(p) = job {
                if done.bad {
                    // The kernel didn't apply them: nothing of this job will come.
                    p.statistics = false;
                    p.outputs = [None; OUTPUTS as usize];
                    p.missed = true;
                } else {
                    p.sequence = Some(done.sequence);
                }
            }
        }
        Ok(())
    }

    /// Takes every buffer `queue` finished, gives each to the job waiting for its frame, and
    /// gives it back to the kernel.
    fn drain(&mut self, queue: Queue) -> Result<()> {
        for _ in 0..self.kernel[queue as usize] {
            let Some(done) = self.hw.dequeue(queue)? else {
                break;
            };
            self.finished(queue, &done);
            self.hw.requeue(queue, done.index)?;
        }
        Ok(())
    }

    /// One finished statistics or image buffer.
    fn finished(&mut self, queue: Queue, done: &Done) {
        let part = match queue {
            Queue::Statistics => Part::Statistics,
            Queue::Main => Part::Output(0),
            Queue::SelfPath => Part::Output(1),
            Queue::Params => return,
        };
        let sequence = self.sequence[queue as usize].widen(done.sequence);
        // Jobs whose frame has passed without this part miss it.
        for p in self.pending.iter_mut().flatten() {
            if p.sequence.is_some_and(|s| before(s, done.sequence)) && p.wants(part) {
                p.settle(part, false);
            }
        }
        let Some(p) = self
            .pending
            .iter_mut()
            .flatten()
            .find(|p| p.sequence == Some(done.sequence) && p.wants(part))
        else {
            return;
        };
        if done.bad {
            p.settle(part, false);
            return;
        }
        let Ok(payload) = self.hw.payload(queue, done) else {
            p.settle(part, false);
            return;
        };
        let frame = StreamFrame {
            buffer: 0,
            bytes: 0,
            sequence,
            identity: p.identity,
            timestamp_ns: done.timestamp_ns,
        };
        let delivered = match part {
            Part::Statistics => {
                let limit = self.options.generation.statistics_layout().bytes;
                match (
                    lowest(self.held.statistics),
                    job::filled(payload, payload.len(), limit),
                ) {
                    (Some(buffer), Some(data)) => deliver(
                        &mut self.statistics,
                        &mut self.held.statistics,
                        buffer,
                        data,
                        StreamFrame { buffer, ..frame },
                    ),
                    _ => false,
                }
            }
            Part::Output(o) => {
                let buffer = p.outputs.get(o).copied().flatten();
                let limit = self
                    .outputs
                    .get(o)
                    .copied()
                    .flatten()
                    .map(|a| a.stream_bytes);
                let stream = output_stream(&mut self.output0, &mut self.output1, o);
                let mask = self.held.outputs.get_mut(o);
                match (buffer, limit, stream, mask) {
                    (Some(buffer), Some(limit), Some(stream), Some(mask)) => {
                        match job::filled(payload, payload.len(), limit) {
                            Some(data) => {
                                deliver(stream, mask, buffer, data, StreamFrame { buffer, ..frame })
                            }
                            None => false,
                        }
                    }
                    _ => false,
                }
            }
        };
        p.settle(part, delivered);
    }

    /// Counts and forgets every job with nothing left to deliver.
    fn retire(&mut self) {
        for slot in &mut self.pending {
            let Some(p) = slot else {
                continue;
            };
            if !p.finished() {
                continue;
            }
            let counter = if p.missed {
                &mut self.counters.errors
            } else {
                &mut self.counters.jobs
            };
            *counter = counter.saturating_add(1);
            *slot = None;
        }
    }

    /// Takes the buffers every stream has handed back.
    fn take_returns(&mut self) -> Result<()> {
        take_returned(&mut self.config, &mut self.held.config)?;
        take_returned(&mut self.statistics, &mut self.held.statistics)?;
        take_returned(&mut self.output0, &mut self.held.outputs[0])?;
        if let Some(stream) = self.output1.as_mut() {
            take_returned(stream, &mut self.held.outputs[1])?;
        }
        Ok(())
    }

    /// Takes every job posted, and sends each or counts it as dropped.
    fn take_jobs(&mut self) -> Result<()> {
        let mut encoded = [0u8; MAX_JOB_BYTES];
        // At most a round of the ring per tick; the rest wait for the next.
        for _ in 0..MAX_JOBS_PER_TICK {
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
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look.
            self.take_returns()?;
            let sent = self.submit(&job)?;
            if !sent {
                self.counters.dropped = self.counters.dropped.saturating_add(1);
            }
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

    /// Sends one job's parameters to the kernel. `false` for a job that can't be sent, which is
    /// the caller's mistake or a full queue; an error means the kernel's queue failed.
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
        for (o, buffer) in checked.outputs.iter().enumerate() {
            let Some(buffer) = *buffer else {
                continue;
            };
            let held = self.held.outputs.get(o).is_some_and(|m| has(*m, buffer));
            let named = self
                .pending
                .iter()
                .flatten()
                .any(|p| p.outputs.get(o) == Some(&Some(buffer)));
            if !held || named {
                return Ok(false);
            }
        }
        let Some(free) = self.pending.iter().position(Option::is_none) else {
            return Ok(false);
        };
        let Some(slot) = lowest(self.params_free) else {
            return Ok(false);
        };
        // The one read of the caller's buffer. Everything after uses the copy.
        match self.config.read(checked.config_buffer, 0, &mut self.params) {
            Ok(true) => {}
            Ok(false) | Err(_) => return Ok(false),
        }
        let generation = self.options.generation;
        let into = self.hw.params_buffer(slot)?;
        let n = job::copy_params(generation, &self.params, into)?;
        let n = u32::try_from(n).map_err(|_| Status::IO)?;
        self.hw.queue_params(slot, n)?;
        self.params_free = with(self.params_free, slot, false);
        self.taken = self.taken.wrapping_add(1);
        if let Some(entry) = self.pending.get_mut(free) {
            *entry = Some(Pending {
                identity: checked.identity,
                order: self.taken,
                slot,
                sequence: None,
                statistics: true,
                outputs: checked.outputs,
                missed: false,
            });
        }
        Ok(true)
    }

    /// The oldest job still waiting, for tests and diagnostics: its identity.
    #[must_use]
    pub fn oldest_waiting(&self) -> Option<u64> {
        self.pending
            .iter()
            .flatten()
            .min_by_key(|p| p.order)
            .map(|p| p.identity)
    }
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
    assert!(OUTPUT_STREAMS[0].len() <= MAX_STREAM_NAME);
    assert!(OUTPUT_STREAMS[1].len() <= MAX_STREAM_NAME);
    assert!(MAX_PENDING <= 64);
};

impl CameraIsp for Rkisp {
    fn info(&mut self) -> Result<IspInfo> {
        let statistics = self.options.generation.statistics_layout();
        Ok(IspInfo {
            max_outputs: self.output_count() as u8,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
            config_layouts: 1,
            statistics_layout: statistics.layout,
            statistics_bytes: statistics.bytes,
        })
    }

    fn config_layout(&mut self, index: u8) -> Result<ConfigLayout> {
        if index != 0 {
            return Err(Status::NOT_FOUND);
        }
        Ok(self.options.generation.config_layout().into())
    }

    fn set_input(&mut self, input: IspInput) -> Result<()> {
        self.stopped()?;
        dimension(input.width)?;
        dimension(input.height)?;
        raw_code(input.bayer, input.bit_depth).ok_or(Status::INVALID_ARGUMENT)?;
        self.input = Some(input);
        Ok(())
    }

    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout> {
        let o = usize::from(output);
        if o >= self.output_count() {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        if !FORMATS.contains(&image.fourcc) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let asked = Image {
            width: dimension(image.width)?,
            height: dimension(image.height)?,
            fourcc: image.fourcc,
            stride: 0,
            size: 0,
        };
        let got = self.hw.try_output(o, asked)?;
        if (got.width, got.height, got.fourcc) != (asked.width, asked.height, asked.fourcc)
            || got.size == 0
            || got.stride < got.width
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        let stream_bytes = got
            .size
            .checked_add(image.pad_bytes)
            .ok_or(Status::INVALID_ARGUMENT)?;
        *self.outputs.get_mut(o).ok_or(Status::NOT_FOUND)? = Some(Agreed {
            image: got,
            stream_bytes,
        });
        Ok(OutputLayout {
            stride: got.stride,
            size: stream_bytes,
        })
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        let o = usize::from(output);
        if o >= self.output_count() {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        *self.outputs.get_mut(o).ok_or(Status::NOT_FOUND)? = None;
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
        let o = usize::from(output);
        if o >= self.output_count() {
            return Err(Status::NOT_FOUND);
        }
        name(OUTPUT_STREAMS.get(o).ok_or(Status::NOT_FOUND)?, into)
    }

    fn start(&mut self) -> Result<()> {
        if self.input.is_none() || self.outputs[0].is_none() {
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
        let config = count(self.config.buffer_count())?;
        count(self.statistics.buffer_count())?;
        let output0 = count(self.output0.buffer_count())?;
        let output1 = match &self.output1 {
            Some(stream) => Some(count(stream.buffer_count())?),
            None => None,
        };
        let generation = self.options.generation;
        self.limits = Some(Limits::new(
            generation,
            config,
            // Only the generation's size is ever read; the stream refuses a read past a buffer.
            generation.config_layout().bytes,
            [
                self.outputs[0].map(|_| output0),
                self.outputs[1].and(output1),
            ],
        )?);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_order_across_the_wrap() {
        assert!(before(1, 2));
        assert!(!before(2, 2));
        assert!(!before(3, 2));
        assert!(before(u32::MAX, 0));
        assert!(!before(0, u32::MAX));
    }

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
    fn raw_codes_are_the_headers() {
        assert_eq!(raw_code(BayerOrder::Rggb, 10), Some(MbusCode(0x300f)));
        assert_eq!(raw_code(BayerOrder::Bggr, 10), Some(MbusCode(0x3007)));
        assert_eq!(raw_code(BayerOrder::Grbg, 12), Some(MbusCode(0x3011)));
        assert_eq!(raw_code(BayerOrder::Mono, 8), Some(MbusCode(0x2001)));
        assert_eq!(raw_code(BayerOrder::Rggb, 14), None);
        assert_eq!(raw_code(BayerOrder::Mono, 16), None);
    }
}
