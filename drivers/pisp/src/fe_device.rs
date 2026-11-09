//! The PiSP front end as `camera.isp/1` and `isp.pisp/1`.
//!
//! Everything that touches the block happens in the kernel (`kernel/lemnos_pisp_fe.c`): this driver
//! sets up the memory, posts jobs on the module's ring and watches it. It does no 3A and holds no
//! tuning. Each job's configuration is the caller's, in the layout
//! [`LAYOUT_FRONT_END`](lemnos_contract_isp_pisp::LAYOUT_FRONT_END); the driver reads it only to
//! see which buffers it calls for, and the module proves it independently, from its own copy.
//!
//! The front end shares the receiver's block and node, so the receiver's driver can't run on the
//! same device. Settings come from the device's `config` in the board file:
//!
//! - `device`: the platform device name of the block. Required.
//! - `lanes`, `mbps`, `data_type`: the CSI-2 link. Required. `camera.isp/1` has no way to carry a
//!   link, and it is the sensor's, fixed by the board.
//! - `vc`: the virtual channel, default 0.
//! - `heap`: the DMA heap, under `/dev/dma_heap`. Default `linux,cma`.
//! - `buffers`: how many images each output has, 2 to 32, default 4.
//! - `job_slots`: jobs that may be queued, a power of two up to 16, default 4.
//! - `config_stream`, `statistics_stream`, `output0_stream`, `output1_stream`: stream names.
//!
//! Streams: `jobs`, when the board provides it, carries [`IspJob`]s, and `frames`, when provided,
//! receives output 0's frames. A job's `outputs` index is by [`OUTPUT_FRONT_END_0`] and
//! [`OUTPUT_FRONT_END_1`]; its statistics go in the slot numbered as its configuration.
//!
//! Failures. A stream that faulted, a job the module refused or a wedged block starts recovery:
//! the next ticks stop what's left and make everything again, with the delay between attempts
//! doubling from 250 ms and capped at 8 s, and clearing when one succeeds. The driver never gives
//! up and never turns itself off; `stop` is what ends it. Nothing is kept across a restart.
//!
//! Provenance: written for the frame stream, from the dev branch's
//! `crates/lemnos-drivers-pisp/src/device/fe.rs`.

use lemnos_contract_camera_isp::{
    CameraIsp, ConfigLayout, Counters, IspInfo, IspInput, IspJob, IspOutput, MAX_OUTPUTS,
    MAX_STREAM_NAME, NO_BUFFER, OutputLayout,
};
use lemnos_contract_isp_pisp::{
    IspPisp, LAYOUT_FRONT_END, LAYOUT_STATISTICS, OUTPUT_FRONT_END_0, OUTPUT_FRONT_END_1, PispInfo,
};
use lemnos_core::{
    Config, Decode, Description, Health, Item, Lifecycle, MethodId, Reader, Result, Status,
    StreamHandle, driver,
};
use lemnos_driver_pisp_fe_sys::{Job, Range, Start};
use lemnos_hal::blocks::StreamFrame;

use crate::fe::FE_OUTPUT_ALIGN;
use crate::fe_hw::{Finished, Hardware, LinuxHardware, Placed, Plan, Settings, slot_bytes};
use crate::fe_job::{self, CONFIG_BYTES, Fixed, STATS_BYTES};
use crate::format::{compute_stride_align, formats};
use crate::uapi::{FE_NUM_OUTPUTS, FeConfig, ImageFormatConfig, Wire};

const START: MethodId = MethodId {
    contract: 0,
    method: 10,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 11,
};

/// A buffer's largest size (`LEMNOS_STREAM_MAX_BUFFER_BYTES`).
const MAX_IMAGE_BYTES: u64 = 1 << 28;
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

/// The CSI-2 link, as the board fixes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    /// Data lanes.
    pub lanes: u32,
    /// Per-lane rate, Mbit/s.
    pub mbps: u32,
    /// Virtual channel, 0 to 3.
    pub vc: u32,
    /// CSI-2 data type, 1 to 63.
    pub data_type: u32,
}

/// The configured link, stream names and sizes.
#[derive(Clone, Copy)]
pub struct Options {
    link: Link,
    buffers: u32,
    job_slots: u32,
    config_stream: Name,
    statistics_stream: Name,
    output_stream: [Name; FE_NUM_OUTPUTS],
}

impl Options {
    /// `INVALID_ARGUMENT` for a count out of range or an empty or long name.
    pub fn new(
        link: Link,
        buffers: u32,
        job_slots: u32,
        config_stream: &str,
        statistics_stream: &str,
        output_stream: [&str; FE_NUM_OUTPUTS],
    ) -> Result<Self> {
        if !(2..=32).contains(&buffers) || !job_slots.is_power_of_two() || job_slots > 16 {
            return Err(Status::INVALID_ARGUMENT);
        }
        let [zero, one] = output_stream;
        Ok(Self {
            link,
            buffers,
            job_slots,
            config_stream: Name::new(config_stream)?,
            statistics_stream: Name::new(statistics_stream)?,
            output_stream: [Name::new(zero)?, Name::new(one)?],
        })
    }
}

/// One output, as last asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Output {
    image: IspOutput,
    layout: OutputLayout,
    format: u32,
}

/// The PiSP front end.
#[driver("pisp-fe", provides = [CameraIsp, IspPisp])]
pub struct PispFe {
    #[needs(optional)]
    jobs: Option<StreamHandle>,
    #[needs(optional)]
    frames: Option<StreamHandle>,
    hw: Box<dyn Hardware>,
    options: Options,
    input: Option<IspInput>,
    outputs: [Option<Output>; FE_NUM_OUTPUTS],
    compressed: [bool; FE_NUM_OUTPUTS],
    state: State,
    placed: Placed,
    /// Whether the block has been started on this stream: the first job does that.
    started: bool,
    counters: Counters,
    /// Whose return the `frames` stream may give back: only what was published.
    out: [bool; 32],
    config: Box<FeConfig>,
    bytes: Box<[u8; CONFIG_BYTES]>,
}

impl Lifecycle for PispFe {
    type Needs = PispFeNeeds;
    const TICKS: bool = true;

    fn start(needs: PispFeNeeds, cfg: &Config<'_>) -> Result<Self> {
        let device = cfg.str("device").ok_or(Status::INVALID_ARGUMENT)?;
        let settings = Settings::new(device, cfg.str("heap").unwrap_or("linux,cma"))?;
        let int = |key, default: Option<u32>| match (cfg.int(key), default) {
            (Some(v), _) => u32::try_from(v).map_err(|_| Status::INVALID_ARGUMENT),
            (None, Some(v)) => Ok(v),
            (None, None) => Err(Status::INVALID_ARGUMENT),
        };
        let options = Options::new(
            Link {
                lanes: int("lanes", None)?,
                mbps: int("mbps", None)?,
                vc: int("vc", Some(0))?,
                data_type: int("data_type", None)?,
            },
            int("buffers", Some(4))?,
            int("job_slots", Some(4))?,
            cfg.str("config_stream").unwrap_or("fe-config"),
            cfg.str("statistics_stream").unwrap_or("fe-statistics"),
            [
                cfg.str("output0_stream").unwrap_or("fe-output0"),
                cfg.str("output1_stream").unwrap_or("fe-output1"),
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
                Health::Degraded("the front end's stream ended while running: restarting it")
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

impl PispFe {
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
            outputs: [None; FE_NUM_OUTPUTS],
            compressed: [false; FE_NUM_OUTPUTS],
            state: State::Stopped,
            placed: Placed::default(),
            started: false,
            counters: Counters::default(),
            out: [false; 32],
            config: Box::default(),
            bytes: Box::new([0; CONFIG_BYTES]),
        }
    }

    fn fail(&mut self, now_ns: u64) -> Option<u64> {
        self.counters.errors = self.counters.errors.saturating_add(1);
        self.hw.stop();
        self.state = State::Recovering {
            attempt: 0,
            due: None,
        };
        self.tick(now_ns)
    }

    /// Makes everything from nothing. The block starts with the first job.
    fn begin(&mut self) -> Result<()> {
        let output0 = self.outputs[0].ok_or(Status::INVALID_ARGUMENT)?;
        self.hw.stop();
        self.started = false;
        self.out = [false; 32];
        // A stream's free buffers come out of `next_buffer` first. None of them is a return.
        if let Some(frames) = self.frames.as_mut() {
            while frames.next_buffer().map_err(|_| Status::IO)?.is_some() {}
        }
        self.placed = self.hw.make(&Plan {
            buffers: self.options.buffers,
            job_slots: self.options.job_slots,
            output0_bytes: output0.layout.size,
            output1_bytes: self.outputs[1].map(|o| o.layout.size),
        })?;
        Ok(())
    }

    /// Finished frames out, returned buffers back, new jobs in. An error means the stream is over.
    fn pump(&mut self) -> Result<()> {
        while let Some(done) = self.hw.next_finished()? {
            self.counters.jobs = self.counters.jobs.saturating_add(1);
            self.finished(&done)?;
        }
        if let Some(frames) = self.frames.as_mut() {
            while let Some(buffer) = frames.next_buffer().map_err(|_| Status::IO)? {
                // Only what was published comes back; a stream's initial free buffers don't.
                if let Some(held @ true) = self.out.get_mut(buffer as usize) {
                    *held = false;
                    self.hw.give_back(buffer)?;
                }
            }
        }
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

    fn finished(&mut self, done: &Finished) -> Result<()> {
        let Some(frames) = self.frames.as_mut() else {
            return self.hw.give_back(done.buffer);
        };
        let frame = StreamFrame {
            buffer: done.buffer,
            bytes: done.bytes,
            sequence: done.sequence,
            identity: done.identity,
            timestamp_ns: done.timestamp_ns,
        };
        if frames.publish(frame).is_ok() {
            if let Some(held) = self.out.get_mut(done.buffer as usize) {
                *held = true;
            }
            Ok(())
        } else {
            self.counters.dropped = self.counters.dropped.saturating_add(1);
            self.hw.give_back(done.buffer)
        }
    }

    /// Sends one job, or counts it as dropped. An error means the stream is over.
    fn submit(&mut self, job: &IspJob) -> Result<()> {
        let Some(kernel) = self.translate(job)? else {
            self.counters.dropped = self.counters.dropped.saturating_add(1);
            return Ok(());
        };
        self.hw.post(&kernel)?;
        if !self.started {
            // The module takes the first job at START, so it has one to check before it writes
            // a register.
            self.hw.go(&self.start_args()?)?;
            self.started = true;
        }
        Ok(())
    }

    fn fixed(&self) -> Result<Fixed> {
        let input = self.input.ok_or(Status::INVALID_ARGUMENT)?;
        let out = self.outputs[0].ok_or(Status::INVALID_ARGUMENT)?;
        Ok(Fixed {
            width: dimension(input.width)?,
            height: dimension(input.height)?,
            output_width: dimension(out.image.width)?,
            output_height: dimension(out.image.height)?,
            output_stride: out.layout.stride,
            output_format: out.format,
        })
    }

    fn start_args(&self) -> Result<Start> {
        let input = self.input.ok_or(Status::INVALID_ARGUMENT)?;
        let out = self.outputs[0].ok_or(Status::INVALID_ARGUMENT)?;
        let link = self.options.link;
        Ok(Start {
            stream_fd: -1,
            lanes: link.lanes,
            mbps: link.mbps,
            vc: link.vc,
            dt: link.data_type,
            width: input.width,
            height: input.height,
            output_width: out.image.width,
            output_height: out.image.height,
            output_stride: out.layout.stride,
            output_size: out.layout.size,
            output_format: out.format,
            flags: 0,
            reserved: [0; 2],
        })
    }

    /// `None` for a job that can't be sent: a caller's mistake, not the stream's end.
    fn translate(&mut self, job: &IspJob) -> Result<Option<Job>> {
        let output = |index: u8| {
            job.outputs
                .get(usize::from(index))
                .copied()
                .unwrap_or(NO_BUFFER)
        };
        let buffers = self.options.buffers;
        let (output0, output1) = (output(OUTPUT_FRONT_END_0), output(OUTPUT_FRONT_END_1));
        if job.layout != LAYOUT_FRONT_END
            || job.input_buffer != NO_BUFFER
            || job.config_buffer >= self.options.job_slots
            || output0 >= buffers
            || (output1 != NO_BUFFER && output1 >= buffers)
        {
            return Ok(None);
        }
        self.hw.read_config(job.config_buffer, &mut self.bytes)?;
        if self.config.read_from(&self.bytes[..]).is_err() {
            return Ok(None);
        }
        let Ok(shape) = fe_job::check(&self.config, &self.fixed()?) else {
            return Ok(None);
        };
        if shape.output1 != (output1 != NO_BUFFER) {
            return Ok(None);
        }
        let slot = u64::from(job.config_buffer);
        let mut out = Job {
            identity: job.identity,
            buffer: output0,
            ..Default::default()
        };
        let mut count = 0;
        let mut push = |addr: u64, len: u64| {
            if let Some(range) = out.range.get_mut(count) {
                *range = Range { addr, len };
            }
            count += 1;
        };
        push(
            self.placed
                .config
                .saturating_add(slot * CONFIG_BYTES as u64),
            CONFIG_BYTES as u64,
        );
        if shape.stats {
            push(
                self.placed
                    .stats
                    .saturating_add(slot * slot_bytes(STATS_BYTES as u32)),
                STATS_BYTES as u64,
            );
        }
        if shape.output1 {
            let (Some(base), Some(o)) = (self.placed.output1, self.outputs[1]) else {
                return Ok(None);
            };
            push(
                base.saturating_add(u64::from(output1) * slot_bytes(o.layout.size)),
                u64::from(o.layout.size),
            );
        }
        out.count = count as u32;
        Ok(Some(out))
    }

    fn stopped(&self) -> Result<()> {
        if self.state == State::Stopped {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
    }

    /// The output index of `camera.isp/1` output `output`, when it's the front end's.
    fn branch(output: u8) -> Result<usize> {
        match output {
            OUTPUT_FRONT_END_0 => Ok(0),
            OUTPUT_FRONT_END_1 => Ok(1),
            _ => Err(Status::NOT_FOUND),
        }
    }

    fn output_of(image: IspOutput, compressed: bool) -> Result<Output> {
        if !is_raw16(image.fourcc) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let format = if compressed {
            formats::PISP_COMP1
        } else {
            formats::BAYER16
        };
        let mut f = ImageFormatConfig {
            width: dimension(image.width)?,
            height: dimension(image.height)?,
            format,
            ..Default::default()
        };
        compute_stride_align(&mut f, FE_OUTPUT_ALIGN);
        let stride = u32::try_from(f.stride).map_err(|_| Status::INVALID_ARGUMENT)?;
        let bytes = u64::from(stride)
            .checked_mul(u64::from(image.height))
            .and_then(|b| b.checked_add(u64::from(image.pad_bytes)))
            .filter(|&b| b <= MAX_IMAGE_BYTES)
            .ok_or(Status::INVALID_ARGUMENT)?;
        let size = u32::try_from(bytes).map_err(|_| Status::INVALID_ARGUMENT)?;
        Ok(Output {
            image,
            layout: OutputLayout { stride, size },
            format,
        })
    }
}

/// Whether `fourcc` is a 16-bit raw format the front end writes.
fn is_raw16(fourcc: u32) -> bool {
    matches!(
        &fourcc.to_le_bytes(),
        b"BYR2" | b"GB16" | b"GR16" | b"RG16" | b"Y16 "
    )
}

fn dimension(v: u32) -> Result<u16> {
    if v == 0 || v > MAX_DIMENSION {
        return Err(Status::INVALID_ARGUMENT);
    }
    u16::try_from(v).map_err(|_| Status::INVALID_ARGUMENT)
}

impl CameraIsp for PispFe {
    fn info(&mut self) -> Result<IspInfo> {
        Ok(IspInfo {
            max_outputs: MAX_OUTPUTS as u8,
            max_width: MAX_DIMENSION,
            max_height: MAX_DIMENSION,
            config_layouts: 1,
            statistics_layout: LAYOUT_STATISTICS,
            statistics_bytes: STATS_BYTES as u32,
        })
    }

    fn config_layout(&mut self, index: u8) -> Result<ConfigLayout> {
        if index != 0 {
            return Err(Status::NOT_FOUND);
        }
        Ok(ConfigLayout {
            layout: LAYOUT_FRONT_END,
            bytes: CONFIG_BYTES as u32,
        })
    }

    fn set_input(&mut self, input: IspInput) -> Result<()> {
        self.stopped()?;
        dimension(input.width)?;
        dimension(input.height)?;
        if !(8..=16).contains(&input.bit_depth) {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.input = Some(input);
        Ok(())
    }

    fn set_output(&mut self, output: u8, image: IspOutput) -> Result<OutputLayout> {
        let branch = Self::branch(output)?;
        self.stopped()?;
        let out = Self::output_of(image, self.compressed[branch])?;
        self.outputs[branch] = Some(out);
        Ok(out.layout)
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        let branch = Self::branch(output)?;
        self.stopped()?;
        self.outputs[branch] = None;
        Ok(())
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        self.options.config_stream.write(into)
    }

    fn statistics_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        self.options.statistics_stream.write(into)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // Frames come straight from the receiver.
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, output: u8, into: &mut [u8]) -> Result<usize> {
        self.options.output_stream[Self::branch(output)?].write(into)
    }

    fn start(&mut self) -> Result<()> {
        if self.input.is_none() || self.outputs[0].is_none() {
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
        self.hw.stop();
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        let mut out = self.counters;
        if self.state != State::Stopped
            && let Ok(block) = self.hw.counters()
        {
            out.dropped = out.dropped.saturating_add(block.dropped);
            out.errors = out.errors.saturating_add(block.errors);
        }
        Ok(out)
    }
}

impl IspPisp for PispFe {
    fn info(&mut self) -> Result<PispInfo> {
        Ok(PispInfo {
            hw_version: self.hw.hw_version(),
            queue_depth: self.options.job_slots,
            front_end_config_bytes: CONFIG_BYTES as u32,
            back_end_config_bytes: 0,
            statistics_bytes: STATS_BYTES as u32,
        })
    }

    fn set_front_end_compression(&mut self, output: u8, compressed: bool) -> Result<()> {
        let branch = Self::branch(output)?;
        self.stopped()?;
        self.compressed[branch] = compressed;
        if let Some(out) = self.outputs[branch] {
            self.outputs[branch] = Some(Self::output_of(out.image, compressed)?);
        }
        Ok(())
    }

    fn enable_temporal_denoise(&mut self, _compressed: bool) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }

    fn reset_temporal_denoise(&mut self) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }

    fn set_line_event(&mut self, _line: u32) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
}

#[cfg(test)]
mod tests;
