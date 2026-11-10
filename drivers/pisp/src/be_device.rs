//! The PiSP back end as `camera.isp/1` and `isp.pisp/1`.
//!
//! Everything that touches the block happens in the kernel (`kernel/lemnos_pisp_be.c`): this
//! driver sets up the memory, posts jobs on the module's ring and watches it. It does no 3A and
//! holds no tuning. Each job's configuration is the caller's, in the layout
//! [`LAYOUT_BACK_END`](lemnos_contract_isp_pisp::LAYOUT_BACK_END); the driver only reads it to
//! find where each image's planes go, and the module proves the result independently.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `device`: the platform device name of the block. Required.
//! - `heap`: the DMA heap for images, under `/dev/dma_heap`. Default `linux,cma`.
//! - `buffers`: how many images each stream has, 2 to 32, default 4.
//! - `job_slots`: jobs that may be queued, a power of two up to 16, default 4.
//! - `config_stream`, `output0_stream`, `output1_stream`: stream names, for callers.
//!
//! Streams: `jobs`, when the board provides it, carries [`IspJob`]s, and `frames`, when provided,
//! receives output 0's frames. Without `frames` a finished buffer goes straight back.
//!
//! Output 0 is written straight into the `frames` buffer a job names, which must be declared
//! `memory = "dma-buf"`: the driver borrows that buffer's dma-buf from the stream, registers it
//! with the module for the one job, and points output 0's planes into it, so the block's DMA lands
//! in the memory the consumer was handed. The module checks every plane against what is
//! registered, as it does every other address. A job may name only a buffer the driver holds: one
//! the consumer has never had, or has given back. A job naming any other, or on a stream whose
//! buffers have no handles, is dropped; nothing is written where the consumer can't see it. The
//! pool buffer of the same number is what the module counts the job by, and is given straight
//! back when the job finishes.
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
use lemnos_hal::blocks::StreamFrame;

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

/// Buffers of a stream the driver tracks, the most `buffers` allows.
const MAX_BUFFERS: usize = 32;
/// A buffer's largest size (`LEMNOS_STREAM_MAX_BUFFER_BYTES`).
const MAX_IMAGE_BYTES: u64 = 1 << 28;
/// The input's alignment; the block needs 4, and importers like more.
const INPUT_ALIGN: u32 = 64;
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

/// A stream name, held by value.
#[derive(Clone, Copy)]
struct Name {
    bytes: [u8; MAX_STREAM_NAME],
    len: usize,
}

impl Name {
    fn new(name: &str) -> Result<Self> {
        let from = name.as_bytes();
        if from.is_empty() || from.len() > MAX_STREAM_NAME {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut bytes = [0; MAX_STREAM_NAME];
        bytes
            .get_mut(..from.len())
            .ok_or(Status::INVALID_ARGUMENT)?
            .copy_from_slice(from);
        Ok(Self {
            bytes,
            len: from.len(),
        })
    }

    fn write(&self, into: &mut [u8]) -> Result<usize> {
        let name = self.bytes.get(..self.len).unwrap_or(&[]);
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }
}

/// The configured stream names and sizes.
#[derive(Clone, Copy)]
pub struct Options {
    buffers: u32,
    job_slots: u32,
    config_stream: Name,
    output_stream: [Name; BE_NUM_OUTPUTS],
}

impl Options {
    /// `INVALID_ARGUMENT` for a count out of range or an empty or long name.
    pub fn new(
        buffers: u32,
        job_slots: u32,
        config_stream: &str,
        output_stream: [&str; BE_NUM_OUTPUTS],
    ) -> Result<Self> {
        if !(2..=32).contains(&buffers) || !job_slots.is_power_of_two() || job_slots > 16 {
            return Err(Status::INVALID_ARGUMENT);
        }
        let [zero, one] = output_stream;
        Ok(Self {
            buffers,
            job_slots,
            config_stream: Name::new(config_stream)?,
            output_stream: [Name::new(zero)?, Name::new(one)?],
        })
    }
}

/// The PiSP back end.
#[driver("pisp-be", provides = [CameraIsp, IspPisp])]
pub struct PispBe {
    #[needs(optional)]
    jobs: Option<StreamHandle>,
    #[needs(optional)]
    frames: Option<StreamHandle>,
    hw: Box<dyn Hardware>,
    options: Options,
    input: Option<IspInput>,
    /// Input bytes per frame, set with `input`.
    input_bytes: u32,
    output_bytes: [Option<u32>; BE_NUM_OUTPUTS],
    tdn_compressed: Option<bool>,
    state: State,
    placed: Placed,
    counters: Counters,
    /// The average that TDN wrote last, once there is one.
    tdn_valid: bool,
    tdn_next: usize,
    tdn_reset: bool,
    /// The `frames` buffers the driver holds and no job is writing: what a job may name.
    held: [bool; MAX_BUFFERS],
    /// The `frames` buffers the block is writing, by the region they were registered as.
    lent: [Option<u32>; MAX_BUFFERS],
    config: Box<BeTilesConfig>,
    bytes: Box<[u8; CONFIG_BYTES]>,
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
        let options = Options::new(
            int("buffers", 4)?,
            int("job_slots", 4)?,
            cfg.str("config_stream").unwrap_or("isp-config"),
            [
                cfg.str("output0_stream").unwrap_or("isp-output0"),
                cfg.str("output1_stream").unwrap_or("isp-output1"),
            ],
        )?;
        Ok(Self::with_hardware(
            Box::new(LinuxHardware::new(settings)),
            needs.jobs,
            needs.frames,
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
    /// A driver over `hw`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(
        hw: Box<dyn Hardware>,
        jobs: Option<StreamHandle>,
        frames: Option<StreamHandle>,
        options: Options,
    ) -> Self {
        Self {
            jobs,
            frames,
            hw,
            options,
            input: None,
            input_bytes: 0,
            output_bytes: [None; BE_NUM_OUTPUTS],
            tdn_compressed: None,
            state: State::Stopped,
            placed: Placed::default(),
            counters: Counters::default(),
            tdn_valid: false,
            tdn_next: 0,
            tdn_reset: false,
            held: [false; MAX_BUFFERS],
            lent: [None; MAX_BUFFERS],
            config: Box::default(),
            bytes: Box::new([0; CONFIG_BYTES]),
        }
    }

    /// Ends the module's stream. What it had registered went with it, so the buffers the block
    /// was writing are the driver's again.
    fn stop_block(&mut self) {
        self.hw.stop();
        for (lent, held) in self.lent.iter_mut().zip(self.held.iter_mut()) {
            if lent.take().is_some() {
                *held = true;
            }
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
        let output0_bytes = self.output_bytes[0].ok_or(Status::INVALID_ARGUMENT)?;
        let tdn_bytes = match (self.tdn_compressed, self.input) {
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
        self.take_returns()?;
        self.placed = self.hw.start(&Plan {
            buffers: self.options.buffers,
            job_slots: self.options.job_slots,
            input_bytes: self.input_bytes,
            output0_bytes,
            output1_bytes: self.output_bytes[1],
            tdn_bytes,
        })?;
        Ok(())
    }

    /// Finished frames out, returned buffers back, new jobs in. An error means the stream is over.
    fn pump(&mut self) -> Result<()> {
        while let Some(done) = self.hw.next_finished()? {
            self.counters.jobs = self.counters.jobs.saturating_add(1);
            self.finished(&done)?;
        }
        self.take_returns()?;
        let mut encoded = [0u8; MAX_JOB_BYTES];
        while let Some(len) = match self.jobs.as_mut() {
            Some(jobs) => jobs.next_job(&mut encoded).map_err(|_| Status::IO)?,
            None => None,
        } {
            let job = encoded
                .get(..len)
                .and_then(|bytes| IspJob::decode(&mut Reader::new(bytes)).ok());
            match job {
                Some(job) => self.submit(&job)?,
                None => self.counters.dropped = self.counters.dropped.saturating_add(1),
            }
        }
        Ok(())
    }

    /// Takes every `frames` buffer the stream has free: the ones never handed out, and the ones
    /// the consumer gave back. Each is the driver's to name in a job.
    fn take_returns(&mut self) -> Result<()> {
        if let Some(frames) = self.frames.as_mut() {
            while let Some(buffer) = frames.next_buffer().map_err(|_| Status::IO)? {
                if let Some(held) = self.held.get_mut(buffer as usize) {
                    *held = true;
                }
            }
        }
        Ok(())
    }

    fn finished(&mut self, done: &Finished) -> Result<()> {
        let lent = self
            .lent
            .get_mut(done.buffer as usize)
            .and_then(Option::take);
        // The pool buffer only counted the job: nothing in it is anyone's.
        let Some(region) = lent else {
            return self.hw.give_back(done.buffer);
        };
        // The block is done with it: the module has released the job's hold.
        self.hw.unregister(region)?;
        self.hw.give_back(done.buffer)?;
        let frame = StreamFrame {
            buffer: done.buffer,
            bytes: done.bytes,
            sequence: done.sequence,
            identity: done.identity,
            timestamp_ns: done.timestamp_ns,
        };
        let published = self.frames.as_mut().map(|frames| frames.publish(frame));
        if !matches!(published, Some(Ok(()))) {
            self.counters.dropped = self.counters.dropped.saturating_add(1);
            if let Some(held) = self.held.get_mut(done.buffer as usize) {
                *held = true;
            }
        }
        Ok(())
    }

    /// Sends one job, or counts it as dropped. An error means the stream is over.
    fn submit(&mut self, job: &IspJob) -> Result<()> {
        if let Some((kernel, region)) = self.translate(job)? {
            if let Some(region) = region {
                let at = kernel.buffer as usize;
                if let (Some(lent), Some(held)) = (self.lent.get_mut(at), self.held.get_mut(at)) {
                    *lent = Some(region);
                    *held = false;
                }
            }
            self.hw.post(&kernel)?;
            if self.config.config.global.bayer_enables & bayer_enable::TDN_OUTPUT != 0 {
                self.tdn_valid = true;
                self.tdn_next = 1 - self.tdn_next;
                self.tdn_reset = false;
            }
        } else {
            self.counters.dropped = self.counters.dropped.saturating_add(1);
        }
        Ok(())
    }

    /// `None` for a job that can't be sent: a caller's mistake, not the stream's end. With a
    /// `frames` stream, the region its output 0 buffer was registered as comes with it.
    fn translate(&mut self, job: &IspJob) -> Result<Option<(KernelJob, Option<u32>)>> {
        let buffers = self.options.buffers;
        let extra = |index: usize| job.outputs.get(index).copied().unwrap_or(NO_BUFFER);
        let output0 = extra(0);
        let output1 = extra(1);
        if job.layout != LAYOUT_BACK_END
            || job.config_buffer >= self.options.job_slots
            || job.input_buffer >= buffers
            || output0 >= buffers
            || (output1 != NO_BUFFER && output1 >= buffers)
            || (2..MAX_OUTPUTS).any(|i| extra(i) != NO_BUFFER)
        {
            return Ok(None);
        }
        if self.frames.is_some() && !self.held.get(output0 as usize).copied().unwrap_or(false) {
            // Not the driver's to give: the consumer has it, or the block is writing it.
            return Ok(None);
        }
        self.hw.read_config(job.config_buffer, &mut self.bytes)?;
        if self.config.read_from(&self.bytes[..]).is_err() || be_job::check(&self.config).is_err() {
            return Ok(None);
        }
        let bayer = self.config.config.global.bayer_enables;
        let tdn = self.placed.tdn;
        let reads_average = bayer & bayer_enable::TDN_INPUT != 0;
        if reads_average && (!self.tdn_valid || self.tdn_reset) {
            return Ok(None);
        }
        let average = |index: usize| tdn.and_then(|t| t.get(index).copied());
        let mut memory = JobMemory {
            input: self
                .placed
                .input
                .saturating_add(u64::from(job.input_buffer) * u64::from(self.input_bytes)),
            ..Default::default()
        };
        if reads_average {
            memory.tdn_input = average(1 - self.tdn_next);
        }
        if bayer & bayer_enable::TDN_OUTPUT != 0 {
            memory.tdn_output = average(self.tdn_next);
        }
        let region = match self.lend(output0)? {
            Lend::Pool => {
                memory.output[0] = Some(
                    self.placed
                        .pool
                        .saturating_add(u64::from(output0) * self.placed.pool_stride),
                );
                None
            }
            Lend::Registered(r) => {
                memory.output[0] = Some(r.addr);
                Some(r.region)
            }
            Lend::Nothing => return Ok(None),
        };
        if output1 != NO_BUFFER {
            let bytes = u64::from(self.output_bytes[1].unwrap_or(0));
            memory.output[1] = self
                .placed
                .output1
                .map(|base| base.saturating_add(u64::from(output1) * bytes));
        }
        let Ok(addr) = be_job::addresses(&self.config.config, &memory) else {
            if let Some(region) = region {
                self.hw.unregister(region)?;
            }
            return Ok(None);
        };
        Ok(Some((
            KernelJob {
                identity: job.identity,
                buffer: output0,
                config: job.config_buffer,
                addr,
            },
            region,
        )))
    }

    /// Where output 0 of a job on `buffer` goes: the pool when there is no `frames` stream, and
    /// otherwise that buffer's own dma-buf, registered for the job. The driver holds `buffer`.
    fn lend(&mut self, buffer: u32) -> Result<Lend> {
        let Some(frames) = self.frames.as_mut() else {
            return Ok(Lend::Pool);
        };
        let need = u64::from(self.output_bytes[0].unwrap_or(u32::MAX));
        let registered = match frames.buffer_handle(buffer) {
            Ok(Some(handle)) => self.hw.register(handle.fd()),
            Ok(None) => {
                // The consumer it was taken for has gone. Publishing it reaches no one and gives
                // it up; the stream's free buffers, the new consumer's, come with the next look.
                if let Some(held) = self.held.get_mut(buffer as usize) {
                    *held = false;
                }
                let _ = frames.publish(StreamFrame {
                    buffer,
                    bytes: 0,
                    sequence: 0,
                    identity: 0,
                    timestamp_ns: 0,
                });
                return Ok(Lend::Nothing);
            }
            // Buffers without handles: nothing the block can write reaches the consumer.
            Err(_) => return Ok(Lend::Nothing),
        };
        // Full of registrations, or a buffer the module won't take: this job only.
        let Ok(r) = registered else {
            return Ok(Lend::Nothing);
        };
        // The real size, not the stream's: the image must fit in what the block was given.
        if r.size < need {
            self.hw.unregister(r.region)?;
            return Ok(Lend::Nothing);
        }
        Ok(Lend::Registered(r))
    }

    fn stopped(&self) -> Result<()> {
        if self.state == State::Stopped {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
    }
}

/// Where a job's output 0 goes.
enum Lend {
    /// The module's pool: there is no `frames` stream.
    Pool,
    /// The `frames` buffer's dma-buf, registered for the job.
    Registered(Registered),
    /// Nowhere the consumer would see: the job is dropped.
    Nothing,
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
            max_outputs: BE_NUM_OUTPUTS as u8,
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
        self.input_bytes = u32::try_from(bytes).map_err(|_| Status::INVALID_ARGUMENT)?;
        self.input = Some(input);
        Ok(())
    }

    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout> {
        if usize::from(output) >= BE_NUM_OUTPUTS {
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
        *self
            .output_bytes
            .get_mut(usize::from(output))
            .ok_or(Status::NOT_FOUND)? = Some(size);
        Ok(OutputLayout { stride, size })
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        if usize::from(output) >= BE_NUM_OUTPUTS {
            return Err(Status::NOT_FOUND);
        }
        self.stopped()?;
        *self
            .output_bytes
            .get_mut(usize::from(output))
            .ok_or(Status::NOT_FOUND)? = None;
        Ok(())
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        self.options.config_stream.write(into)
    }

    fn statistics_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // Statistics are the front end's, which this driver doesn't run.
        Err(Status::NOT_SUPPORTED)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // The input is memory this driver makes; no stream hands it over yet.
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, output: u8, into: &mut [u8]) -> Result<usize> {
        self.options
            .output_stream
            .get(usize::from(output))
            .ok_or(Status::NOT_FOUND)?
            .write(into)
    }

    fn start(&mut self) -> Result<()> {
        if self.input.is_none() || self.output_bytes[0].is_none() {
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
