//! The PiSP front end as `camera.isp/1` and `isp.pisp/1`.
//!
//! Everything that touches the block happens in the kernel (`kernel/lemnos_pisp_fe.c`): this driver
//! hands the module the clients' memory, posts jobs on the module's ring and watches it. It does no
//! 3A and holds no tuning. Each job's configuration is the caller's, in the layout
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
//! - `heap`: the DMA heap for the driver's own memory, under `/dev/dma_heap`. Default
//!   `linux,cma`.
//! - `job_slots`: jobs that may wait on the module's ring, a power of two up to 16, default 4.
//!
//! # Streams
//!
//! Named after the needs, as [`CameraIsp::config_stream`] and the others answer, the way the back
//! end's are:
//!
//! - `config`: jobs ([`IspJob`]) are posted on its ring, and its buffers hold configurations, each
//!   at least [`CONFIG_BYTES`]. Every buffer stays with the driver. The client writes a
//!   configuration into one and posts a job naming it; the driver reads it **once**, when it takes
//!   the job, into its own memory. Only that copy is checked, and only that copy goes into the
//!   job's configuration slot, memory of the driver's that the module was given and no client can
//!   write; the module copies it from there when it gets to the job, which may be frames later. So
//!   the client changing its buffer after posting the job changes nothing the block runs. The
//!   driver refuses a copy with any address field or `ilines` set, as the module would, so such a
//!   job is dropped rather than ending the stream.
//! - `statistics`: the statistics, `memory = "dma-buf"`, each buffer at least [`STATS_BYTES`]. A
//!   job whose configuration turns a statistics block on gets its statistics in the `statistics`
//!   buffer **numbered as its configuration buffer**: `IspJob` has no field of its own for it.
//!   That buffer must be one the driver holds: one the client has never had or has given back.
//! - `output0`: output 0's images, `memory = "dma-buf"`. A job names one in
//!   `outputs[OUTPUT_FRONT_END_0]`.
//! - `output1`, optional: output 1's images, `memory = "dma-buf"`, named in
//!   `outputs[OUTPUT_FRONT_END_1]` by a job whose configuration turns output 1 on, and only then.
//!
//! When the frame is finished, its output 0 image, its statistics and its output 1 image are
//! published on their streams, each with the job's `identity`.
//!
//! # Configuration buffers
//!
//! The front end follows the contract's rules for configuration buffers (`contracts/camera-isp`,
//! "Configuration buffers"). A job is accepted when the driver takes it off the ring. It reads
//! its config buffer once, then holds that buffer busy until the job completes (a frame, or a
//! drop or error), and a job naming a config buffer that an accepted job still holds is dropped
//! and counted, with nothing posted, the earlier job undisturbed. A config buffer whose consumer
//! faulted while it was busy is given up when its job completes.
//!
//! # Memory
//!
//! Output 0 is written straight into the `output0` buffer a job names. The module's stream is made
//! with no pool, and the driver hands the module the dma-buf of every `output0` buffer it holds
//! (`StreamEndpoint::buffer_handle`), which the module pins, checks against output 0's size and
//! makes that buffer's memory. Output 0's address is never the driver's to give: the module takes
//! it from the job's buffer. A job may name only a buffer the driver holds whose memory the module
//! has been shown: one the consumer has never had or has given back, and not one another job has.
//!
//! A consumer of `output0` that leaves, and the next one. The stream's memory is made anew for
//! each consumer. The block is not stopped: the module keeps the old memory pinned while a job may
//! be writing it, so nothing is ever written to freed memory, and its frame reaches no one. Each
//! buffer is given the new consumer's memory as soon as no job has it (`fe_device/relay.rs`).
//!
//! Statistics and output 1 are lent job by job, as the back end lends its images: the driver
//! borrows the buffer's dma-buf from its stream, registers it with the module for the one job,
//! names it in the job's ranges, and unregisters it when the frame is finished, before publishing
//! it. The module resolves those ranges to its pinned dma-bufs and fills every address of the
//! configuration in itself.
//!
//! A statistics or output 1 buffer still held for a client that has gone is given up and taken
//! again as the new client's within the job, which then runs. A job is dropped, and nothing
//! posted, when it names a buffer the driver doesn't hold or one another job still has on the
//! block, when a stream it needs is missing or its buffers have no handles, when the module won't
//! register a buffer (it has room for few at once), when a buffer is smaller than what the
//! configuration makes the block write there, or when the module's ring is full.
//!
//! Bytes on the config stream's job queue that are no job are a broken consumer, as the
//! contract says: they are counted as dropped, that consumer's config stream is faulted, and the
//! config buffers held under it are given up.
//!
//! An `output0`, `statistics` or `output1` stream whose buffers have no handles (plain shared
//! memory) is refused when the driver binds, with `NOT_SUPPORTED`: the stream says what its
//! buffers are before any client attaches (`StreamEndpoint::buffer_kind`), so the device shows as
//! failing to bind until the board file is fixed. `start` refuses the same way. `start` without
//! an `output0` stream on the device is `NOT_FOUND`; with buffers smaller than output 0, or more
//! than 64 of them, `INVALID_ARGUMENT` when a client is already attached, and degraded health
//! once one is.
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
    CallerId, Config, Decode, Description, Dev, Health, Item, Lifecycle, MethodId, Reader, Result,
    Status, StreamHandle, driver,
};
use lemnos_driver_pisp_fe_sys::{Job, Range, Start};
use lemnos_hal::blocks::{BufferKind, StreamFrame};

mod relay;

use relay::{MAX_BUFFERS, Relay};

use crate::fe::FE_OUTPUT_ALIGN;
use crate::fe_hw::{
    Finished, Hardware, LinuxHardware, Placed, Plan, Registered, Settings, slot_bytes,
};
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

/// The stream jobs are posted on, whose buffers hold configurations.
pub const CONFIG_STREAM: &str = "config";
/// The stream of statistics buffers.
pub const STATISTICS_STREAM: &str = "statistics";
/// Each output's stream.
pub const OUTPUT_STREAMS: [&str; FE_NUM_OUTPUTS] = ["output0", "output1"];

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

const _: () = {
    assert!(CONFIG_STREAM.len() <= MAX_STREAM_NAME);
    assert!(STATISTICS_STREAM.len() <= MAX_STREAM_NAME);
    assert!(OUTPUT_STREAMS[0].len() <= MAX_STREAM_NAME);
    assert!(OUTPUT_STREAMS[1].len() <= MAX_STREAM_NAME);
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

/// The configured link and ring.
#[derive(Clone, Copy)]
pub struct Options {
    link: Link,
    job_slots: u32,
}

impl Options {
    /// `INVALID_ARGUMENT` for a ring that isn't a power of two up to 16.
    pub fn new(link: Link, job_slots: u32) -> Result<Self> {
        if !job_slots.is_power_of_two() || job_slots > 16 {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self { link, job_slots })
    }
}

/// One output, as last asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Output {
    image: IspOutput,
    layout: OutputLayout,
    format: u32,
}

/// Which buffers of one stream the driver may name in a job.
#[derive(Clone, Copy)]
struct Buffers {
    /// Taken from the stream and not given up: the driver's.
    held: [bool; MAX_BUFFERS],
    /// On the block for a job now.
    busy: [bool; MAX_BUFFERS],
    /// Held and busy when the consumer that had them faulted: given up when the job completes.
    leaving: [bool; MAX_BUFFERS],
}

impl Buffers {
    const NONE: Self = Self {
        held: [false; MAX_BUFFERS],
        busy: [false; MAX_BUFFERS],
        leaving: [false; MAX_BUFFERS],
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

/// A stream whose buffers are lent to the block job by job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lendable {
    Statistics,
    Output1,
}

/// A buffer lent to the block for one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lent {
    /// Its stream's buffer.
    buffer: u32,
    /// What the module registered it as.
    region: u32,
    /// What the block writes into it.
    bytes: u32,
}

/// What a job on the block has besides its output 0 buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    statistics: Option<Lent>,
    output1: Option<Lent>,
    /// The config buffer the job was accepted with, busy until the job completes.
    config: u32,
}

/// The PiSP front end.
#[driver("pisp-fe", provides = [CameraIsp, IspPisp])]
pub struct PispFe {
    #[needs]
    config: StreamHandle,
    #[needs(optional)]
    statistics: Option<StreamHandle>,
    #[needs(optional)]
    output0: Option<StreamHandle>,
    #[needs(optional)]
    output1: Option<StreamHandle>,
    /// The ISP the board feeds this front end to (`link` on the front end): the back end.
    #[link]
    _next: Option<Dev<dyn CameraIsp>>,
    hw: Box<dyn Hardware>,
    options: Options,
    input: Option<IspInput>,
    outputs: [Option<Output>; FE_NUM_OUTPUTS],
    compressed: [bool; FE_NUM_OUTPUTS],
    state: State,
    /// The caller of the call in progress, told by the host (`Lifecycle::caller`).
    caller: Option<CallerId>,
    /// The caller whose START started the front end: its going away stops it (`caller_gone`).
    owner: Option<CallerId>,
    placed: Placed,
    /// Whether the block has been started on this stream: the first job does that.
    started: bool,
    counters: Counters,
    /// Where each `output0` buffer is, and whether the module's memory for it is the stream's.
    relay: Relay,
    /// The `config` buffers the driver holds, and which an accepted job still holds: a job naming
    /// one of those is dropped (contract rule 4).
    configs: Buffers,
    /// The `statistics` buffers the driver holds, and which a job has.
    stats: Buffers,
    /// The `output1` buffers the driver holds, and which a job has.
    out1: Buffers,
    /// Jobs on the block, by their `output0` buffer.
    jobs: [Option<InFlight>; MAX_BUFFERS],
    /// The driver's copy of the job being taken, parsed.
    parsed: Box<FeConfig>,
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

impl Lifecycle for PispFe {
    // The host says who is calling, so a caller that goes away can stop the front end it started.
    const TRACKS_CALLERS: bool = true;

    fn caller(&mut self, caller: CallerId) {
        self.caller = Some(caller);
    }

    fn caller_gone(&mut self, caller: CallerId) {
        if self.owner == Some(caller) {
            self.owner = None;
            let _ = CameraIsp::stop(self);
        }
    }

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
            int("job_slots", Some(4))?,
        )?;
        // The block writes images and statistics only into dma-bufs: known now, before any
        // client attaches.
        for stream in [&needs.output0, &needs.statistics, &needs.output1]
            .into_iter()
            .flatten()
        {
            handles_only(stream)?;
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
                Health::Degraded("the front end's stream ended while running: restarting it")
            }
            State::Running { .. } if self.relay.refused() => Health::Degraded(
                "the output0 stream's buffers are not memory the block can take \
                 (smaller than output 0?): jobs on them are dropped",
            ),
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
    /// A driver over `hw` with the streams in `needs`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, needs: PispFeNeeds, options: Options) -> Self {
        Self {
            config: needs.config,
            statistics: needs.statistics,
            output0: needs.output0,
            output1: needs.output1,
            _next: needs._next,
            hw,
            options,
            input: None,
            outputs: [None; FE_NUM_OUTPUTS],
            compressed: [false; FE_NUM_OUTPUTS],
            state: State::Stopped,
            caller: None,
            owner: None,
            placed: Placed::default(),
            started: false,
            counters: Counters::default(),
            relay: Relay::new(),
            configs: Buffers::NONE,
            stats: Buffers::NONE,
            out1: Buffers::NONE,
            jobs: [None; MAX_BUFFERS],
            parsed: Box::default(),
            bytes: Box::new([0; CONFIG_BYTES]),
        }
    }

    /// Ends the module's stream. What it had registered went with it, so the buffers lent to the
    /// block are the driver's again.
    fn stop_block(&mut self) {
        self.hw.stop();
        self.jobs = [None; MAX_BUFFERS];
        self.stats.busy = [false; MAX_BUFFERS];
        self.out1.busy = [false; MAX_BUFFERS];
        // The jobs that held config buffers are gone with the module's stream.
        self.configs.busy = [false; MAX_BUFFERS];
        for buffer in 0..MAX_BUFFERS as u32 {
            self.let_go_config(buffer);
        }
    }

    /// Ends a config buffer's hold by an accepted job, and gives it up if its consumer faulted
    /// while the job held it. Nothing is behind a faulted consumer's buffer, so this reaches no one.
    fn let_go_config(&mut self, buffer: u32) {
        Buffers::set(&mut self.configs.busy, buffer, false);
        if self.configs.leaving.get(buffer as usize) == Some(&true) {
            Buffers::set(&mut self.configs.leaving, buffer, false);
            Buffers::set(&mut self.configs.held, buffer, false);
            give_up(&mut self.config, buffer);
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

    /// Makes everything from nothing: a new module stream, given the memory of every `output0`
    /// buffer the driver holds. The block starts with the first job.
    fn begin(&mut self) -> Result<()> {
        let output0 = self.outputs[0].ok_or(Status::INVALID_ARGUMENT)?;
        let Some(frames) = self.output0.as_ref() else {
            // Nowhere for output 0 to go.
            return Err(Status::NOT_FOUND);
        };
        for stream in [
            Some(frames),
            self.statistics.as_ref(),
            self.output1.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            handles_only(stream)?;
        }
        let buffers = frames.buffer_count();
        if buffers == 0 || buffers as usize > MAX_BUFFERS {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.stop_block();
        self.started = false;
        self.placed = self.hw.make(&Plan {
            buffers,
            job_slots: self.options.job_slots,
            output0_bytes: output0.layout.size,
        })?;
        self.relay.restart(buffers);
        let ready = match self.output0.as_mut() {
            Some(frames) => match self.relay.settle_all(&mut **frames, &mut *self.hw) {
                Ok(()) => self.relay.take(&mut **frames, &mut *self.hw),
                Err(status) => Err(status),
            },
            None => Err(Status::NOT_FOUND),
        };
        let refusal = if self.relay.no_handles() {
            // A consumer is attached to plain shared memory: the block can't be given it.
            Err(Status::NOT_SUPPORTED)
        } else if self.relay.refused() {
            Err(Status::INVALID_ARGUMENT)
        } else {
            ready
        };
        if refusal.is_err() {
            self.stop_block();
        }
        refusal
    }

    /// Finished frames out, returned buffers back, new jobs in. An error means the stream is over.
    fn pump(&mut self) -> Result<()> {
        while let Some(done) = self.hw.next_finished()? {
            self.counters.jobs = self.counters.jobs.saturating_add(1);
            self.finished(&done)?;
        }
        if let Some(frames) = self.output0.as_mut() {
            self.relay.sweep(&mut **frames);
            self.relay.take(&mut **frames, &mut *self.hw)?;
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
            if self.configs.held.get(buffer as usize) != Some(&true) {
                continue;
            }
            if self.configs.busy.get(buffer as usize) == Some(&true) {
                // An accepted job still holds it: it is given up when that job completes.
                Buffers::set(&mut self.configs.leaving, buffer, true);
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

    /// Takes every buffer the config, statistics and output 1 streams have free.
    fn take_returns(&mut self) -> Result<()> {
        self.configs.take(&mut self.config)?;
        if let Some(stream) = self.statistics.as_mut() {
            self.stats.take(stream)?;
        }
        if let Some(stream) = self.output1.as_mut() {
            self.out1.take(stream)?;
        }
        Ok(())
    }

    fn finished(&mut self, done: &Finished) -> Result<()> {
        let job = self
            .jobs
            .get_mut(done.buffer as usize)
            .and_then(Option::take)
            .unwrap_or(InFlight {
                statistics: None,
                output1: None,
                config: NO_BUFFER,
            });
        // The job is complete: its config buffer is the consumer's again.
        self.let_go_config(job.config);
        // The block is done with them: the module has released the job's hold.
        for lent in [job.statistics, job.output1].iter().flatten() {
            self.hw.unregister(lent.region)?;
        }
        let image = match self.output0.as_mut() {
            Some(frames) => self.relay.finished(done, &mut **frames, &mut *self.hw)?,
            None => {
                self.hw.give_back(done.buffer)?;
                false
            }
        };
        // A frame the block flagged as bad holds no statistics or output 1 worth reading: the
        // driver keeps those buffers.
        let ran = done.bytes != 0;
        let mut delivered = image;
        for (which, lent) in [
            (Lendable::Statistics, job.statistics),
            (Lendable::Output1, job.output1),
        ] {
            let Some(lent) = lent else { continue };
            let (stream, buffers) = self.lendable(which);
            Buffers::set(&mut buffers.busy, lent.buffer, false);
            if ran {
                delivered &= publish(stream, buffers, lent, done);
            }
        }
        if !delivered {
            // Written where its consumer no longer is, or not taken by a stream.
            self.counters.dropped = self.counters.dropped.saturating_add(1);
        }
        Ok(())
    }

    /// Sends one job. `false` for a job that can't be sent, a caller's mistake or no room; an
    /// error means the stream is over.
    fn submit(&mut self, job: &IspJob) -> Result<bool> {
        let named = |index: u8| {
            job.outputs
                .get(usize::from(index))
                .copied()
                .unwrap_or(NO_BUFFER)
        };
        let (output0, output1) = (named(OUTPUT_FRONT_END_0), named(OUTPUT_FRONT_END_1));
        let others = (0..MAX_OUTPUTS as u8)
            .filter(|&i| i != OUTPUT_FRONT_END_0 && i != OUTPUT_FRONT_END_1)
            .any(|i| named(i) != NO_BUFFER);
        if job.layout != LAYOUT_FRONT_END
            || job.input_buffer != NO_BUFFER
            || others
            || output0 == NO_BUFFER
            || output0 as usize >= MAX_BUFFERS
            || !self.configs.free(job.config_buffer)
        {
            return Ok(false);
        }
        // The one read of the client's buffer. Everything after uses the copy.
        match self.config.read(job.config_buffer, 0, &mut self.bytes[..]) {
            Ok(true) => {}
            Ok(false) | Err(_) => return Ok(false),
        }
        if self.parsed.read_from(&self.bytes[..]).is_err() {
            return Ok(false);
        }
        // Refuses, among the rest, every address field and `ilines` the client left set: those
        // are the module's to fill in.
        let Ok(shape) = fe_job::check(&self.parsed, &self.fixed()?) else {
            return Ok(false);
        };
        if shape.output1 != (output1 != NO_BUFFER) || (shape.output1 && self.outputs[1].is_none()) {
            return Ok(false);
        }
        let output1_bytes = if shape.output1 {
            fe_job::output_bytes(&self.parsed, 1)
        } else {
            None
        };

        let mut flight = InFlight {
            statistics: None,
            output1: None,
            config: job.config_buffer,
        };
        let mut ranges = [Range::default(); 2];
        let mut count = 0;
        if shape.stats {
            let Some((addr, lent)) = self.lend(
                Lendable::Statistics,
                job.config_buffer,
                Some(STATS_BYTES as u64),
            )?
            else {
                return Ok(false);
            };
            flight.statistics = Some(lent);
            ranges[count] = Range {
                addr,
                len: STATS_BYTES as u64,
            };
            count += 1;
        }
        if shape.output1 {
            let Some((addr, lent)) = self.lend(Lendable::Output1, output1, output1_bytes)? else {
                self.release(&flight)?;
                return Ok(false);
            };
            flight.output1 = Some(lent);
            ranges[count] = Range {
                addr,
                len: u64::from(lent.bytes),
            };
            count += 1;
        }
        // Last, so nothing above leaves a buffer marked as a job's: output 0 must be an `output0`
        // buffer the driver holds, on memory the module was shown, that no job has.
        let claimed = match self.output0.as_mut() {
            Some(frames) => self.relay.claim(output0, &mut **frames),
            None => false,
        };
        if !claimed {
            self.release(&flight)?;
            return Ok(false);
        }
        // The configuration slot is output 0's buffer's: no other job can have it now.
        self.hw.write_config(output0, &self.bytes)?;
        let mut kernel = Job {
            identity: job.identity,
            buffer: output0,
            count: 1 + count as u32,
            ..Default::default()
        };
        kernel.range[0] = Range {
            addr: self
                .placed
                .config
                .saturating_add(u64::from(output0) * slot_bytes(CONFIG_BYTES as u32)),
            len: CONFIG_BYTES as u64,
        };
        for (to, from) in kernel.range[1..].iter_mut().zip(&ranges[..count]) {
            *to = *from;
        }
        match self.hw.post(&kernel) {
            Ok(()) => {}
            Err(Status::BUSY) => {
                self.relay.unclaim(output0);
                self.release(&flight)?;
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        for (which, lent) in [
            (Lendable::Statistics, flight.statistics),
            (Lendable::Output1, flight.output1),
        ] {
            if let Some(lent) = lent {
                Buffers::set(&mut self.lendable(which).1.busy, lent.buffer, true);
            }
        }
        // Held until the job completes: a job naming this config buffer is dropped until then.
        Buffers::set(&mut self.configs.busy, job.config_buffer, true);
        if let Some(slot) = self.jobs.get_mut(output0 as usize) {
            *slot = Some(flight);
        }
        if !self.started {
            // The module takes the first job at START, so it has one to check before it writes
            // a register.
            self.hw.go(&self.start_args()?)?;
            self.started = true;
        }
        Ok(true)
    }

    /// A lendable stream, if the device has it, and the buffers of it the driver holds.
    fn lendable(&mut self, which: Lendable) -> (Option<&mut StreamHandle>, &mut Buffers) {
        match which {
            Lendable::Statistics => (self.statistics.as_mut(), &mut self.stats),
            Lendable::Output1 => (self.output1.as_mut(), &mut self.out1),
        }
    }

    /// Lets go of what a job that won't be sent had registered.
    fn release(&mut self, flight: &InFlight) -> Result<()> {
        for lent in [flight.statistics, flight.output1].iter().flatten() {
            self.hw.unregister(lent.region)?;
        }
        Ok(())
    }

    /// Registers `buffer` of `which` for one job, when it can hold `bytes`: its device address
    /// and what to let go of afterwards. `None` drops the job: the driver must hold `buffer` and
    /// no job may have it.
    fn lend(
        &mut self,
        which: Lendable,
        buffer: u32,
        bytes: Option<u64>,
    ) -> Result<Option<(u64, Lent)>> {
        let Some(need) = bytes.filter(|&b| b > 0 && b <= MAX_IMAGE_BYTES) else {
            return Ok(None);
        };
        let (stream, buffers) = match which {
            Lendable::Statistics => (self.statistics.as_mut(), &mut self.stats),
            Lendable::Output1 => (self.output1.as_mut(), &mut self.out1),
        };
        let Some(stream) = stream else {
            return Ok(None);
        };
        if !buffers.free(buffer) {
            return Ok(None);
        }
        let mut handle = stream.buffer_handle(buffer);
        if matches!(handle, Ok(None)) {
            // Taken for a client that has gone, or before any came. Publishing it reaches no
            // one and gives it up; it is free again at once as the client's now, so it is taken
            // again and lent under that client, and the job runs. Still not the client's:
            // dropped.
            give_up(stream, buffer);
            Buffers::set(&mut buffers.held, buffer, false);
            buffers.take(stream)?;
            if !buffers.free(buffer) {
                return Ok(None);
            }
            handle = stream.buffer_handle(buffer);
        }
        let registered = match handle {
            Ok(Some(handle)) => self.hw.register(handle.fd()),
            Ok(None) => return Ok(None),
            // Buffers without handles: nothing the block writes would reach the client.
            Err(_) => return Ok(None),
        };
        // Full of registrations, or a buffer the module won't take: this job only.
        let Ok(Registered { region, addr, size }) = registered else {
            return Ok(None);
        };
        // The real size, not the stream's: what the block writes must fit what it was given.
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

    fn stopped(&self) -> Result<()> {
        if self.state == State::Stopped {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
    }

    /// The output index of `camera.isp/1` output `output`, when the front end has it: output 1
    /// needs its stream.
    fn branch(&self, output: u8) -> Result<usize> {
        match output {
            OUTPUT_FRONT_END_0 => Ok(0),
            OUTPUT_FRONT_END_1 if self.output1.is_some() => Ok(1),
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

/// Publishes a buffer with nothing behind it, which reaches no one and gives it up.
fn give_up(stream: &mut StreamHandle, buffer: u32) {
    let _ = stream.publish(StreamFrame {
        buffer,
        bytes: 0,
        sequence: 0,
        identity: 0,
        timestamp_ns: 0,
    });
}

/// Publishes what the block wrote into a lent buffer, with its job's identity, when the client it
/// was taken for is still there. Whether the client got it.
fn publish(
    stream: Option<&mut StreamHandle>,
    buffers: &mut Buffers,
    lent: Lent,
    done: &Finished,
) -> bool {
    let Some(stream) = stream else {
        return false;
    };
    match stream.buffer_handle(lent.buffer) {
        Ok(Some(_)) => {}
        Ok(None) => {
            // The client left while the block was writing its memory.
            give_up(stream, lent.buffer);
            Buffers::set(&mut buffers.held, lent.buffer, false);
            return false;
        }
        Err(_) => return false,
    }
    let frame = StreamFrame {
        buffer: lent.buffer,
        bytes: lent.bytes,
        sequence: done.sequence,
        identity: done.identity,
        timestamp_ns: done.timestamp_ns,
    };
    let published = stream.publish(frame).is_ok();
    if published {
        Buffers::set(&mut buffers.held, lent.buffer, false);
    }
    published
}

/// Writes a stream name into `into`.
fn name(name: &str, into: &mut [u8]) -> Result<usize> {
    let bytes = name.as_bytes();
    into.get_mut(..bytes.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(bytes);
    Ok(bytes.len())
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
        let branch = self.branch(output)?;
        self.stopped()?;
        let out = Self::output_of(image, self.compressed[branch])?;
        self.outputs[branch] = Some(out);
        Ok(out.layout)
    }

    fn clear_output(&mut self, output: u8) -> Result<()> {
        let branch = self.branch(output)?;
        self.stopped()?;
        self.outputs[branch] = None;
        Ok(())
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(CONFIG_STREAM, into)
    }

    fn statistics_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        if self.statistics.is_none() {
            return Err(Status::NOT_SUPPORTED);
        }
        name(STATISTICS_STREAM, into)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        // Frames come straight from the receiver.
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, output: u8, into: &mut [u8]) -> Result<usize> {
        let branch = self.branch(output)?;
        if branch == 0 && self.output0.is_none() {
            return Err(Status::NOT_FOUND);
        }
        name(OUTPUT_STREAMS[branch], into)
    }

    fn start(&mut self) -> Result<()> {
        if self.input.is_none() || self.outputs[0].is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.stopped()?;
        // Whoever starts the front end owns it until it stops, even if the start then fails.
        self.owner = self.caller;
        self.counters = Counters::default();
        self.begin()?;
        self.state = State::Running { next_check: None };
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.owner = None;
        self.state = State::Stopped;
        self.stop_block();
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

    /// `None` before `set_input`, then `NOT_SUPPORTED`: the front end's frames come from the
    /// receiver, and the driver programs no stride for them (`input_stream` is refused too).
    fn input_layout(&mut self) -> Result<Option<OutputLayout>> {
        if self.input.is_none() {
            return Ok(None);
        }
        Err(Status::NOT_SUPPORTED)
    }

    fn output_layout(&mut self, output: u8) -> Result<Option<OutputLayout>> {
        let branch = self.branch(output)?;
        Ok(self.outputs[branch].map(|out| out.layout))
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
            stalled: if self.state == State::Stopped {
                0
            } else {
                self.hw.stalled()
            },
        })
    }

    fn set_front_end_compression(&mut self, output: u8, compressed: bool) -> Result<()> {
        let branch = self.branch(output)?;
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
mod frames_tests;
#[cfg(test)]
mod tests;
