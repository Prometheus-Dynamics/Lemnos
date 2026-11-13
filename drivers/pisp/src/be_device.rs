//! The PiSP back end as `camera.isp/1` and `isp.pisp/1`.
//!
//! Everything that touches the block happens in the kernel (`kernel/lemnos_pisp_be.c`): this
//! driver hands the module the clients' memory, posts jobs on the module's ring and watches it. It
//! does no 3A and holds no tuning. Each job's configuration is the caller's, in the layout
//! [`LAYOUT_BACK_END`](lemnos_contract_isp_pisp::LAYOUT_BACK_END); the driver only reads it to
//! find where each image's planes go, and the module proves the result independently.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `device`: the platform device name of the block. Required.
//! - `heap`: the DMA heap for the driver's own memory, under `/dev/dma_heap`. Default
//!   `linux,cma`.
//! - `buffers`: jobs the block may hold at once, 2 to 32, default 4.
//! - `job_slots`: jobs that may wait on the module's ring, a power of two up to 16, default 4.
//!
//! # Streams
//!
//! Named after the needs, as [`CameraIsp::config_stream`] and the others answer:
//!
//! - `config`: jobs ([`IspJob`]) are posted on its ring, and its buffers hold configurations,
//!   each at least [`BeTilesConfig::SIZE`] bytes. Every buffer stays with the driver. The client
//!   writes a configuration into one and posts a job naming it; the driver reads it **once**,
//!   when it takes the job, into its own memory. Only that copy is checked, and only that copy
//!   goes to the module's configuration slot, so the client changing the buffer afterwards
//!   changes nothing the block runs. The module copies the slot again and proves it on its own.
//! - `input`: the raw frames, `memory = "dma-buf"`. Every buffer stays with the driver too; the
//!   client writes a frame into one and names it in a job.
//! - `output0`, and `output1` when the board has it: the images, `memory = "dma-buf"`. A job
//!   names a buffer the driver holds on each output its configuration turns on: one the client
//!   has never had or has given back.
//!
//! For each image of a job, the driver borrows that buffer's dma-buf from its stream, registers
//! it with the module for the one job, and points the image's planes into it, so the block reads
//! and writes the memory the client was handed. When the job ends the driver unregisters it and
//! publishes the outputs. A buffer still held for a client that has gone is given up and taken
//! again as the new client's within the job, which then runs. A job is dropped, and nothing
//! posted, when an image names a buffer the driver doesn't hold or one another job still has on
//! the block, when its stream's buffers have no handles, when the module won't register it, or
//! when the buffer is smaller than the image the configuration describes.
//!
//! Bytes on the config stream's job queue that are no job are a broken consumer, as the
//! contract says: they are counted as dropped, that consumer's config stream is faulted, and the
//! config buffers held under it are given up.
//!
//! The `input` and output streams must be `memory = "dma-buf"`. The driver asks each what its
//! buffers are when it binds (`StreamEndpoint::buffer_kind`) and refuses a stream of plain shared
//! memory there with `NOT_SUPPORTED`, before any client attaches; `start` refuses the same way.
//! `config` may be either: it is only read.
//!
//! What a client's configuration may not name: the bytes where older layouts kept addresses
//! (the head of `pisp_be_config`), the HOG address, the reserved tail and the bus settings are
//! the module's, and a configuration with any of them non-zero is dropped here, as the module
//! would refuse it. Every address the block uses comes from the driver: the client's tiles carry
//! offsets, which the module proves against the images.
//!
//! The module's pool is one page per job: a pool buffer holds no image, it is only how the
//! module counts a job and publishes its end. The two temporal denoise averages are the only
//! image-sized memory the driver makes, because they are the block's own state between frames.
//!
//! Failures. A stream that faulted, a job the module refused, or a wedged block starts recovery:
//! the next ticks stop what's left and make everything again, with the delay between attempts
//! doubling from 250 ms and capped at 8 s, and clearing when one succeeds. The driver never gives
//! up and never turns itself off; `stop` is what ends it. Nothing is kept across a restart.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-pisp/src/device/`) onto the
//! frame stream.

use lemnos_contract_camera_isp::{
    CameraIsp, ConfigLayout, Counters, IspInfo, IspInput, IspJob, IspOutput, MAX_OUTPUTS,
    MAX_STREAM_NAME, NO_BUFFER, OutputLayout,
};
use lemnos_contract_isp_pisp::{
    IspPisp, LAYOUT_BACK_END, OUTPUT_FRONT_END_0, OUTPUT_FRONT_END_1, PispInfo,
};
use lemnos_core::{
    Config, Decode, Description, Health, Item, Lifecycle, MethodId, Reader, Result, Status,
    StreamHandle, driver,
};
use lemnos_driver_pisp_sys::CONFIG_BYTES;
use lemnos_hal::blocks::{BufferKind, StreamFrame};

use crate::be_hw::{Finished, Hardware, LinuxHardware, Placed, Plan, Registered, Settings};
use crate::be_job::{self, JobMemory, KernelJob};
use crate::format::{compute_stride_align, formats};
use crate::uapi::{
    BE_NUM_OUTPUTS, BE_OUTPUT_MIN_ALIGN, BeTilesConfig, ImageFormatConfig, Wire, bayer_enable,
};

const START: MethodId = MethodId {
    contract: 0,
    method: 10,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 11,
};

/// The stream jobs are posted on, whose buffers hold configurations.
pub const CONFIG_STREAM: &str = "config";
/// The stream of raw frames a client gives the block.
pub const INPUT_STREAM: &str = "input";
/// Each output's stream.
pub const OUTPUT_STREAMS: [&str; BE_NUM_OUTPUTS] = ["output0", "output1"];

/// Buffers of a stream the driver tracks: a job naming a later one is dropped.
const MAX_BUFFERS: usize = 32;
/// Jobs the block may hold at once, the most `buffers` allows.
const MAX_JOBS: usize = 32;
/// A buffer's largest size (`LEMNOS_STREAM_MAX_BUFFER_BYTES`).
const MAX_IMAGE_BYTES: u64 = 1 << 28;
/// The input's alignment; the block needs 4, and importers like more.
pub const INPUT_ALIGN: u32 = 64;
/// The block's largest image in each direction.
const MAX_DIMENSION: u32 = 0xffff;
/// Bytes of an encoded job at most: every field at its widest.
const MAX_JOB_BYTES: usize = 128;
/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often jobs and finished frames are looked for.
const POLL_NS: u64 = 1_000_000;
/// How often the module's state is read while running.
const CHECK_NS: u64 = 1_000_000_000;

const _: () = {
    assert!(CONFIG_STREAM.len() <= MAX_STREAM_NAME);
    assert!(INPUT_STREAM.len() <= MAX_STREAM_NAME);
    assert!(OUTPUT_STREAMS[0].len() <= MAX_STREAM_NAME);
    assert!(OUTPUT_STREAMS[1].len() <= MAX_STREAM_NAME);
    assert!(BeTilesConfig::SIZE == CONFIG_BYTES);
};

/// The delay before recovery attempt `attempt` (from 0), doubling to the 8 s cap.
const fn backoff_ns(attempt: u32) -> u64 {
    BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Stopped,
    Running { next_check: Option<u64> },
    Recovering { attempt: u32, due: Option<u64> },
}

/// How many jobs the block may hold and wait for.
#[derive(Clone, Copy)]
pub struct Options {
    buffers: u32,
    job_slots: u32,
}

impl Options {
    /// `INVALID_ARGUMENT` for a count out of range.
    pub fn new(buffers: u32, job_slots: u32) -> Result<Self> {
        if !(2..=MAX_JOBS as u32).contains(&buffers)
            || !job_slots.is_power_of_two()
            || job_slots > 16
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self { buffers, job_slots })
    }
}

/// Which buffers of one stream the driver may name in a job.
#[derive(Clone, Copy)]
struct Buffers {
    /// Taken from the stream and not given up: the driver's.
    held: [bool; MAX_BUFFERS],
    /// On the block for a job now.
    busy: [bool; MAX_BUFFERS],
}

impl Buffers {
    const NONE: Self = Self {
        held: [false; MAX_BUFFERS],
        busy: [false; MAX_BUFFERS],
    };

    fn free(&self, buffer: u32) -> bool {
        let at = buffer as usize;
        self.held.get(at) == Some(&true) && self.busy.get(at) == Some(&false)
    }

    fn set(flags: &mut [bool; MAX_BUFFERS], buffer: u32, value: bool) {
        if let Some(flag) = flags.get_mut(buffer as usize) {
            *flag = value;
        }
    }

    /// Takes every buffer the stream has free: never handed out, or given back.
    fn take(&mut self, stream: &mut StreamHandle) -> Result<()> {
        while let Some(buffer) = stream.next_buffer().map_err(|_| Status::IO)? {
            Self::set(&mut self.held, buffer, true);
        }
        Ok(())
    }
}

/// One image of a job, on the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lent {
    /// Its stream's buffer.
    buffer: u32,
    /// What the module registered it as.
    region: u32,
    /// Its bytes, as the configuration lays it out.
    bytes: u32,
}

/// A job the block has, by the pool buffer the module counts it by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    input: Lent,
    outputs: [Option<Lent>; BE_NUM_OUTPUTS],
    /// The config buffer the job named.
    config: u32,
}

/// Which stream an image is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Image {
    Input,
    Output(usize),
}

/// The PiSP back end.
#[driver("pisp-be", provides = [CameraIsp, IspPisp])]
pub struct PispBe {
    #[needs(job)]
    config: StreamHandle,
    #[needs(job)]
    input: StreamHandle,
    #[needs(job)]
    output0: StreamHandle,
    #[needs(optional, job)]
    output1: Option<StreamHandle>,
    hw: Box<dyn Hardware>,
    options: Options,
    raw: Option<IspInput>,
    /// The layout of a raw frame, from `set_input`: the stride and bytes the configuration must name.
    raw_layout: Option<OutputLayout>,
    /// Each output's layout, from `set_output`; `None` while it is off.
    output_layouts: [Option<OutputLayout>; BE_NUM_OUTPUTS],
    /// Config buffers a job accepted here still names; busy until that job completes.
    config_busy: [bool; MAX_BUFFERS],
    output_bytes: [Option<u32>; BE_NUM_OUTPUTS],
    tdn_compressed: Option<bool>,
    /// Bytes of each temporal denoise average, while it's on.
    tdn_bytes: Option<u32>,
    state: State,
    placed: Placed,
    counters: Counters,
    /// The average that TDN wrote last, once there is one.
    tdn_valid: bool,
    tdn_next: usize,
    tdn_reset: bool,
    /// The `config` buffers the driver holds; never lent, only read.
    configs: Buffers,
    inputs: Buffers,
    outputs: [Buffers; BE_NUM_OUTPUTS],
    /// Jobs on the block, by pool buffer.
    jobs: [Option<InFlight>; MAX_JOBS],
    /// The driver's copy of the job being taken, parsed.
    parsed: Box<BeTilesConfig>,
    /// The driver's copy of the job being taken, as read: what goes to the module.
    bytes: Box<[u8; CONFIG_BYTES]>,
}

/// `NOT_SUPPORTED` for a stream whose buffers have no handles: the block takes an image only as a
/// dma-buf. The stream knows from the board file, with or without a consumer.
fn handles_only(stream: &StreamHandle) -> Result<()> {
    if stream.buffer_kind() == BufferKind::Handles {
        Ok(())
    } else {
        Err(Status::NOT_SUPPORTED)
    }
}

impl Lifecycle for PispBe {
    type Needs = PispBeNeeds;
    const TICKS: bool = true;

    fn start(needs: PispBeNeeds, cfg: &Config<'_>) -> Result<Self> {
        let device = cfg.str("device").ok_or(Status::INVALID_ARGUMENT)?;
        let settings = Settings::new(device, cfg.str("heap").unwrap_or("linux,cma"))?;
        let int = |key, default| match cfg.int(key) {
            Some(v) => u32::try_from(v).map_err(|_| Status::INVALID_ARGUMENT),
            None => Ok(default),
        };
        let options = Options::new(int("buffers", 4)?, int("job_slots", 4)?)?;
        // The block reads and writes images only as dma-bufs: known now, before any client.
        handles_only(&needs.input)?;
        handles_only(&needs.output0)?;
        if let Some(output1) = needs.output1.as_ref() {
            handles_only(output1)?;
        }
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
                Health::Degraded("the back end's stream ended while running: restarting it")
            }
            _ => Health::Ok,
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Running { next_check } => {
                if self.pump().is_err() {
                    return self.fail(now_ns);
                }
                let check_at = match next_check {
                    None => now_ns.saturating_add(CHECK_NS),
                    Some(at) if now_ns < at => at,
                    Some(_) => {
                        if self.hw.check().is_err() {
                            return self.fail(now_ns);
                        }
                        now_ns.saturating_add(CHECK_NS)
                    }
                };
                self.state = State::Running {
                    next_check: Some(check_at),
                };
                Some(now_ns.saturating_add(POLL_NS).min(check_at))
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
                    self.state = State::Running { next_check: None };
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

impl PispBe {
    /// A driver over `hw` with the streams in `needs`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, needs: PispBeNeeds, options: Options) -> Self {
        Self {
            config: needs.config,
            input: needs.input,
            output0: needs.output0,
            output1: needs.output1,
            hw,
            options,
            raw: None,
            raw_layout: None,
            output_layouts: [None; BE_NUM_OUTPUTS],
            config_busy: [false; MAX_BUFFERS],
            output_bytes: [None; BE_NUM_OUTPUTS],
            tdn_compressed: None,
            tdn_bytes: None,
            state: State::Stopped,
            placed: Placed::default(),
            counters: Counters::default(),
            tdn_valid: false,
            tdn_next: 0,
            tdn_reset: false,
            configs: Buffers::NONE,
            inputs: Buffers::NONE,
            outputs: [Buffers::NONE; BE_NUM_OUTPUTS],
            jobs: [None; MAX_JOBS],
            parsed: Box::default(),
            bytes: Box::new([0; CONFIG_BYTES]),
        }
    }

    /// An image's stream, the buffers of it the driver holds, and the block, borrowed apart.
    fn parts(
        &mut self,
        image: Image,
    ) -> (
        Option<&mut StreamHandle>,
        Option<&mut Buffers>,
        &mut dyn Hardware,
    ) {
        let (stream, buffers) = match image {
            Image::Input => (Some(&mut self.input), Some(&mut self.inputs)),
            Image::Output(o) => {
                let stream = match o {
                    0 => Some(&mut self.output0),
                    1 => self.output1.as_mut(),
                    _ => None,
                };
                (stream, self.outputs.get_mut(o))
            }
        };
        (stream, buffers, &mut *self.hw)
    }

    /// Outputs this back end has: output 1 needs its stream.
    fn output_count(&self) -> usize {
        if self.output1.is_some() {
            BE_NUM_OUTPUTS
        } else {
            1
        }
    }

    /// Ends the module's stream. What it had registered went with it, so the buffers the block
    /// had are the driver's again.
    fn stop_block(&mut self) {
        self.hw.stop();
        self.jobs = [None; MAX_JOBS];
        self.inputs.busy = [false; MAX_BUFFERS];
        self.config_busy = [false; MAX_BUFFERS];
        for o in &mut self.outputs {
            o.busy = [false; MAX_BUFFERS];
        }
    }

    fn fail(&mut self, now_ns: u64) -> Option<u64> {
        self.counters.errors = self.counters.errors.saturating_add(1);
        self.stop_block();
        self.state = State::Recovering {
            attempt: 0,
            due: None,
        };
        self.tick(now_ns)
    }

    /// Makes everything from nothing.
    fn begin(&mut self) -> Result<()> {
        handles_only(&self.input)?;
        handles_only(&self.output0)?;
        if let Some(output1) = self.output1.as_ref() {
            handles_only(output1)?;
        }
        let tdn_bytes = match (self.tdn_compressed, self.raw) {
            (Some(compressed), Some(input)) => {
                let per_pixel = if compressed { 1 } else { 2 };
                let stride = (input.width * per_pixel + 63) & !63;
                Some(
                    stride
                        .checked_mul(input.height)
                        .ok_or(Status::INVALID_ARGUMENT)?,
                )
            }
            _ => None,
        };
        self.stop_block();
        self.tdn_valid = false;
        self.tdn_next = 0;
        self.tdn_bytes = tdn_bytes;
        self.take_returns()?;
        self.placed = self.hw.start(&Plan {
            buffers: self.options.buffers,
            job_slots: self.options.job_slots,
            tdn_bytes,
        })?;
        Ok(())
    }

    /// Finished frames out, returned buffers back, new jobs in. An error means the stream is over.
    fn pump(&mut self) -> Result<()> {
        while let Some(done) = self.hw.next_finished()? {
            self.finished(&done)?;
        }
        self.take_returns()?;
        let mut encoded = [0u8; MAX_JOB_BYTES];
        while let Some(len) = self.config.next_job(&mut encoded).map_err(|_| Status::IO)? {
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
            if !self.submit(&job)? {
                self.counters.dropped = self.counters.dropped.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Faults the config stream's consumer. The config buffers held were taken under it and
    /// have nothing behind them now: each is given up, to be taken again under the next one.
    fn fault_config(&mut self) -> Result<()> {
        self.config.fault_consumer().map_err(|_| Status::IO)?;
        for buffer in 0..MAX_BUFFERS as u32 {
            if !self.configs.free(buffer) {
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
            Buffers::set(&mut self.configs.held, buffer, false);
        }
        Ok(())
    }

    /// Takes every buffer each stream has free.
    fn take_returns(&mut self) -> Result<()> {
        self.configs.take(&mut self.config)?;
        self.inputs.take(&mut self.input)?;
        self.outputs[0].take(&mut self.output0)?;
        if let Some(output1) = self.output1.as_mut() {
            self.outputs[1].take(output1)?;
        }
        Ok(())
    }

    fn finished(&mut self, done: &Finished) -> Result<()> {
        let job = self
            .jobs
            .get_mut(done.buffer as usize)
            .and_then(Option::take);
        let Some(job) = job else {
            // Not a job of this driver's: only the pool buffer to give back.
            return self.hw.give_back(done.buffer);
        };
        // The block is done with them: the module has released the job's hold.
        self.hw.unregister(job.input.region)?;
        Buffers::set(&mut self.inputs.busy, job.input.buffer, false);
        Buffers::set(&mut self.config_busy, job.config, false);
        for lent in job.outputs.iter().flatten() {
            self.hw.unregister(lent.region)?;
        }
        self.hw.give_back(done.buffer)?;
        let failed = done.bytes == 0;
        if failed {
            self.counters.errors = self.counters.errors.saturating_add(1);
        } else {
            self.counters.jobs = self.counters.jobs.saturating_add(1);
        }
        for (o, lent) in job.outputs.iter().enumerate() {
            let Some(lent) = lent else { continue };
            if let Some(buffers) = self.outputs.get_mut(o) {
                Buffers::set(&mut buffers.busy, lent.buffer, false);
            }
            if failed {
                // The driver keeps it: nothing in it is the job's image.
                continue;
            }
            let frame = StreamFrame {
                buffer: lent.buffer,
                bytes: lent.bytes,
                sequence: done.sequence,
                identity: done.identity,
                timestamp_ns: done.timestamp_ns,
            };
            let (stream, buffers, _) = self.parts(Image::Output(o));
            let published = stream.map(|stream| stream.publish(frame));
            if matches!(published, Some(Ok(()))) {
                if let Some(buffers) = buffers {
                    Buffers::set(&mut buffers.held, lent.buffer, false);
                }
            } else {
                self.counters.dropped = self.counters.dropped.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Sends one job. `false` for a job that can't be sent, a caller's mistake or no room; an
    /// error means the stream is over.
    fn submit(&mut self, job: &IspJob) -> Result<bool> {
        let named = |index: usize| job.outputs.get(index).copied().unwrap_or(NO_BUFFER);
        if job.layout != LAYOUT_BACK_END
            || !self.configs.free(job.config_buffer)
            || self.config_busy.get(job.config_buffer as usize) != Some(&false)
            || !self.inputs.free(job.input_buffer)
            || (BE_NUM_OUTPUTS..MAX_OUTPUTS).any(|i| named(i) != NO_BUFFER)
        {
            return Ok(false);
        }
        for o in 0..BE_NUM_OUTPUTS {
            let buffer = named(o);
            if buffer == NO_BUFFER {
                continue;
            }
            let on = self.output_bytes.get(o).copied().flatten().is_some();
            if !on || !self.outputs.get(o).is_some_and(|b| b.free(buffer)) {
                // An output that's off, or a buffer the client has or the block is writing.
                return Ok(false);
            }
        }
        let Some(token) = (0..self.options.buffers as usize)
            .find(|&at| self.jobs.get(at).is_some_and(Option::is_none))
        else {
            return Ok(false);
        };
        // The one read of the client's buffer. Everything after uses the copy.
        match self.config.read(job.config_buffer, 0, &mut self.bytes[..]) {
            Ok(true) => {}
            Ok(false) | Err(_) => return Ok(false),
        }
        if self.parsed.read_from(&self.bytes[..]).is_err() || be_job::check(&self.parsed).is_err() {
            return Ok(false);
        }
        let c = &self.parsed.config;
        // The configuration must describe the frame set_input described: its size and stride.
        let frame_ok = self.raw.is_some_and(|r| {
            u32::from(c.input_format.width) == r.width
                && u32::from(c.input_format.height) == r.height
        }) && self
            .raw_layout
            .is_some_and(|l| u32::try_from(c.input_format.stride).ok() == Some(l.stride));
        if !frame_ok {
            return Ok(false);
        }
        let bayer = c.global.bayer_enables;
        let reads_average = bayer & bayer_enable::TDN_INPUT != 0;
        let writes_average = bayer & bayer_enable::TDN_OUTPUT != 0;
        if reads_average && (!self.tdn_valid || self.tdn_reset) {
            return Ok(false);
        }
        // The averages are the driver's memory: an image larger than they are is refused here
        // rather than by the module, which would end the stream.
        let average_fits = |on: bool, image: &ImageFormatConfig| {
            !on || be_job::image_bytes(image)
                .is_some_and(|b| b <= u64::from(self.tdn_bytes.unwrap_or(0)))
        };
        if !average_fits(reads_average, &c.tdn_input_format)
            || !average_fits(writes_average, &c.tdn_output_format)
        {
            return Ok(false);
        }
        let input_bytes = be_job::image_bytes(&c.input_format);
        let output_bytes = c.output_format.map(|o| be_job::image_bytes(&o.image));
        let average = |index: usize| self.placed.tdn.and_then(|t| t.get(index).copied());
        let mut memory = JobMemory::default();
        if reads_average {
            memory.tdn_input = average(1 - self.tdn_next);
        }
        if writes_average {
            memory.tdn_output = average(self.tdn_next);
        }

        let Some(input) = self.lend(Image::Input, job.input_buffer, input_bytes)? else {
            return Ok(false);
        };
        memory.input = input.0;
        let mut flight = InFlight {
            input: input.1,
            outputs: [None; BE_NUM_OUTPUTS],
            config: job.config_buffer,
        };
        for (o, bytes) in output_bytes.into_iter().enumerate() {
            let buffer = named(o);
            if buffer == NO_BUFFER {
                continue;
            }
            match self.lend(Image::Output(o), buffer, bytes)? {
                Some((addr, lent)) => {
                    memory.output[o] = Some(addr);
                    flight.outputs[o] = Some(lent);
                }
                None => {
                    self.release(&flight)?;
                    return Ok(false);
                }
            }
        }
        let Ok(addr) = be_job::addresses(&self.parsed.config, &memory) else {
            self.release(&flight)?;
            return Ok(false);
        };
        let kernel = KernelJob {
            identity: job.identity,
            buffer: token as u32,
            config: 0,
            addr,
        };
        match self.hw.post(&kernel, &self.bytes) {
            Ok(()) => {}
            Err(Status::BUSY) => {
                self.release(&flight)?;
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        Buffers::set(&mut self.inputs.busy, flight.input.buffer, true);
        Buffers::set(&mut self.config_busy, flight.config, true);
        for (o, lent) in flight.outputs.iter().enumerate() {
            if let (Some(lent), Some(buffers)) = (lent, self.outputs.get_mut(o)) {
                Buffers::set(&mut buffers.busy, lent.buffer, true);
            }
        }
        if let Some(slot) = self.jobs.get_mut(token) {
            *slot = Some(flight);
        }
        if writes_average {
            self.tdn_valid = true;
            self.tdn_next = 1 - self.tdn_next;
            self.tdn_reset = false;
        }
        Ok(true)
    }

    /// Lets go of what a job that won't be sent had registered.
    fn release(&mut self, flight: &InFlight) -> Result<()> {
        self.hw.unregister(flight.input.region)?;
        for lent in flight.outputs.iter().flatten() {
            self.hw.unregister(lent.region)?;
        }
        Ok(())
    }

    /// Registers `buffer` of `image`'s stream for one job, when it can hold `bytes`: its device
    /// address and what to let go of afterwards. `None` drops the job; the driver holds `buffer`
    /// and no job has it.
    fn lend(
        &mut self,
        image: Image,
        buffer: u32,
        bytes: Option<u64>,
    ) -> Result<Option<(u64, Lent)>> {
        let Some(need) = bytes.filter(|&b| b > 0 && b <= MAX_IMAGE_BYTES) else {
            return Ok(None);
        };
        let (Some(stream), Some(buffers), hw) = self.parts(image) else {
            return Ok(None);
        };
        let mut handle = stream.buffer_handle(buffer);
        if matches!(handle, Ok(None)) {
            // Taken for a client that has gone, or before any came (a buffer held since the
            // driver bound). Publishing it reaches no one and gives it up; it is free again at
            // once as the client's now, so it is taken again and lent under that client, and the
            // job runs. Still not the client's: dropped.
            let _ = stream.publish(StreamFrame {
                buffer,
                bytes: 0,
                sequence: 0,
                identity: 0,
                timestamp_ns: 0,
            });
            Buffers::set(&mut buffers.held, buffer, false);
            buffers.take(stream)?;
            if !buffers.free(buffer) {
                return Ok(None);
            }
            handle = stream.buffer_handle(buffer);
        }
        let registered = match handle {
            Ok(Some(handle)) => hw.register(handle.fd()),
            Ok(None) => return Ok(None),
            // Buffers without handles: nothing the block touches reaches the client.
            Err(_) => return Ok(None),
        };
        // Full of registrations, or a buffer the module won't take: this job only.
        let Ok(Registered { region, addr, size }) = registered else {
            return Ok(None);
        };
        // The real size, not the stream's: the image must fit in what the block was given.
        if size < need {
            self.hw.unregister(region)?;
            return Ok(None);
        }
        let bytes = u32::try_from(need).map_err(|_| Status::INVALID_ARGUMENT)?;
        Ok(Some((
            addr,
            Lent {
                buffer,
                region,
                bytes,
            },
        )))
    }

    fn stopped(&self) -> Result<()> {
        if self.state == State::Stopped {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
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

/// The block's pixel format for a four-character code.
fn format_of(fourcc: u32) -> Option<u32> {
    match &fourcc.to_le_bytes() {
        b"GREY" => Some(crate::uapi::image_format::BPS_8),
        b"RGB3" => Some(formats::RGB888),
        b"YU12" => Some(formats::YUV420P),
        b"NV12" => Some(formats::NV12),
        b"NV21" => Some(formats::NV21),
        b"YUYV" => Some(formats::YUYV),
        b"UYVY" => Some(formats::UYVY),
        _ => None,
    }
}

fn dimension(v: u32) -> Result<u16> {
    if v == 0 || v > MAX_DIMENSION {
        return Err(Status::INVALID_ARGUMENT);
    }
    u16::try_from(v).map_err(|_| Status::INVALID_ARGUMENT)
}

impl CameraIsp for PispBe {
    fn info(&mut self) -> Result<IspInfo> {
        Ok(IspInfo {
            max_outputs: self.output_count() as u8,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
            config_layouts: 1,
            statistics_layout: 0,
            statistics_bytes: 0,
        })
    }

    fn config_layout(&mut self, index: u8) -> Result<ConfigLayout> {
        if index != 0 {
            return Err(Status::NOT_FOUND);
        }
        Ok(ConfigLayout {
            layout: LAYOUT_BACK_END,
            bytes: BeTilesConfig::SIZE as u32,
        })
    }

    fn set_input(&mut self, input: IspInput) -> Result<()> {
        self.stopped()?;
        let (width, height) = (dimension(input.width)?, dimension(input.height)?);
        if !(8..=16).contains(&input.bit_depth) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut image = ImageFormatConfig {
            width,
            height,
            format: formats::BAYER16,
            ..Default::default()
        };
        compute_stride_align(&mut image, INPUT_ALIGN);
        let bytes = be_job::image_bytes(&image)
            .filter(|&b| b <= MAX_IMAGE_BYTES)
            .ok_or(Status::INVALID_ARGUMENT)?;
        self.raw_layout = Some(OutputLayout {
            stride: u32::try_from(image.stride).map_err(|_| Status::INVALID_ARGUMENT)?,
            size: u32::try_from(bytes).map_err(|_| Status::INVALID_ARGUMENT)?,
        });
        self.raw = Some(input);
        Ok(())
    }

    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout> {
        if usize::from(output) >= self.output_count() {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        let mut format = ImageFormatConfig {
            width: dimension(image.width)?,
            height: dimension(image.height)?,
            format: format_of(image.fourcc).ok_or(Status::INVALID_ARGUMENT)?,
            ..Default::default()
        };
        compute_stride_align(&mut format, BE_OUTPUT_MIN_ALIGN);
        let bytes = be_job::image_bytes(&format)
            .and_then(|b| b.checked_add(u64::from(image.pad_bytes)))
            .filter(|&b| b <= MAX_IMAGE_BYTES)
            .ok_or(Status::INVALID_ARGUMENT)?;
        let size = u32::try_from(bytes).map_err(|_| Status::INVALID_ARGUMENT)?;
        let stride = u32::try_from(format.stride).map_err(|_| Status::INVALID_ARGUMENT)?;
        let layout = OutputLayout { stride, size };
        *self
            .output_bytes
            .get_mut(usize::from(output))
            .ok_or(Status::NOT_FOUND)? = Some(size);
        *self
            .output_layouts
            .get_mut(usize::from(output))
            .ok_or(Status::NOT_FOUND)? = Some(layout);
        Ok(layout)
    }

    fn input_layout(&mut self) -> Result<Option<OutputLayout>> {
        Ok(self.raw_layout)
    }

    fn output_layout(&mut self, output: u8) -> Result<Option<OutputLayout>> {
        if usize::from(output) >= self.output_count() {
            return Err(Status::NOT_FOUND);
        }
        Ok(self
            .output_layouts
            .get(usize::from(output))
            .copied()
            .flatten())
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        if usize::from(output) >= self.output_count() {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        *self
            .output_bytes
            .get_mut(usize::from(output))
            .ok_or(Status::NOT_FOUND)? = None;
        *self
            .output_layouts
            .get_mut(usize::from(output))
            .ok_or(Status::NOT_FOUND)? = None;
        Ok(())
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(CONFIG_STREAM, into)
    }

    fn statistics_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // Statistics are the front end's, which this driver doesn't run.
        Err(Status::NOT_SUPPORTED)
    }

    fn input_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(INPUT_STREAM, into)
    }

    fn output_stream(&mut self, output: u8, into: &mut [u8]) -> Result<usize> {
        let o = usize::from(output);
        if o >= self.output_count() {
            return Err(Status::NOT_FOUND);
        }
        name(OUTPUT_STREAMS.get(o).ok_or(Status::NOT_FOUND)?, into)
    }

    fn start(&mut self) -> Result<()> {
        if self.raw.is_none() || self.output_bytes.iter().all(Option::is_none) {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.stopped()?;
        self.counters = Counters::default();
        self.begin()?;
        self.state = State::Running { next_check: None };
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.state = State::Stopped;
        self.stop_block();
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }
}

impl IspPisp for PispBe {
    fn info(&mut self) -> Result<PispInfo> {
        Ok(PispInfo {
            hw_version: self.hw.hw_version(),
            queue_depth: self.options.job_slots,
            front_end_config_bytes: 0,
            back_end_config_bytes: BeTilesConfig::SIZE as u32,
            statistics_bytes: 0,
            stalled: 0,
        })
    }

    fn set_front_end_compression(&mut self, output: u8, _compressed: bool) -> Result<()> {
        if output == OUTPUT_FRONT_END_0 || output == OUTPUT_FRONT_END_1 {
            Err(Status::NOT_SUPPORTED)
        } else {
            Err(Status::NOT_FOUND)
        }
    }

    fn enable_temporal_denoise(&mut self, compressed: bool) -> Result<()> {
        self.stopped()?;
        self.tdn_compressed = Some(compressed);
        Ok(())
    }

    fn reset_temporal_denoise(&mut self) -> Result<()> {
        self.tdn_reset = true;
        Ok(())
    }

    fn set_line_event(&mut self, _line: u32) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
}

#[cfg(test)]
mod tests;
