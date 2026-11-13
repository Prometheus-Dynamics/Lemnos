//! Simulated camera drivers: a sensor, a receiver and an ISP, each speaking its contract.
//!
//! They keep no state across a restart. Each contract call that changes something is appended to
//! a log, so a test can check the order calls arrived in across devices. The receiver makes frames
//! on the host's ticks at the rate its link implies, and publishes them on the frame stream the
//! board gives it. A driver's stream end can't write pixel bytes (on hardware the block's DMA does
//! that), so simulated frames carry sequence, identity (0, as the receivers in the tree publish) and
//! timestamp, and no pixel content.
//!
//! The sensor's controls go through a simulated pipeline shared by the sensor and its receiver, so
//! a frame shows the values in force for it, with the timing rule of `camera.sensor`: a group whose
//! launch falls in frame `F` shows each control on frame `F + delay`. Frame `k` is the `k`th start
//! since the receiver started, one interval after the first tick; the first `drop_frames` are
//! masked at the sensor and never published, but they are starts and count. With `embedded = true`
//! the sensor reports register embedded data, and the receiver publishes the values in force for
//! each frame on its `embedded` stream, in the layout `encode_embedded` and `decode_embedded` here
//! define. The board file sets the delays, masking and embedded data through the sensor's `config`.
//!
//! Lock-step (the default) is the board the proofs above run on: a post's group launches at once,
//! in the frame in progress. Free-running (`free_running = true`) runs frames on their own clock,
//! `period_us` apart when given (the link's frame time otherwise), and a post's group takes
//! `write_us` to reach the sensor, launching when the write ends. So a write that crosses a frame
//! start lands one frame later than the caller's schedule says, as with group hold on hardware.
//!
//! Landing frames. `post_values_at` reads the receiver's `frame_starts` through the sensor's link
//! before its write and after it, and answers with `launch_frame` of the two reads, the rule in the
//! `camera.sensor` docs. The sim's physics is the frame in progress when the write ends; a test
//! checks the answer against what the embedded data shows. `naive_landing = true` answers from the
//! read before the write alone, a negative control the proofs must fail on.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lemnos_contract_camera_isp::{
    CameraIsp, ConfigLayout, Counters as IspCounters, IspInfo, IspInput, IspOutput, OutputLayout,
};
use lemnos_contract_camera_receiver::FrameStarts;
use lemnos_contract_camera_receiver::{
    BayerOrder, CameraReceiver, Counters, FrameLayout, Limits, LinkConfig,
    MAX_EMBEDDED_STREAM_NAME, MAX_STREAM_NAME, Packing,
};
use lemnos_contract_camera_sensor::{
    AppliedLanding, CameraSensor, Controls, EmbeddedData, FrameValues, GainLaw, GainRange, InForce,
    Landing, Mode, Rect, SensorInfo, landing_of, launch_frame,
};
use lemnos_core::{Config, ConfigValue, Dev, Lifecycle, Result, Status, StreamHandle, driver};
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::SimBoard;

/// Calls in the order they arrived, across every simulated device.
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Serialises tests that share the log and the pipeline.
static SERIAL: Mutex<()> = Mutex::new(());
/// The pipeline the sensor and receiver share: the schedule of values and the frame count.
static PIPELINE: Mutex<Pipeline> = Mutex::new(Pipeline::new());

fn log(entry: &str) {
    LOG.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(entry.to_string());
}

fn pipeline() -> MutexGuard<'static, Pipeline> {
    PIPELINE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Takes the test lock, clears the log and resets the pipeline.
pub fn serial() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    LOG.lock().unwrap_or_else(PoisonError::into_inner).clear();
    *pipeline() = Pipeline::new();
    guard
}

/// The sensor's settings, from its board `config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimConfig {
    /// Exposure, gain and frame length delays, in frames. 0 to 8.
    pub delays: [u8; 3],
    /// Frames masked at the sensor after the receiver starts. 0 to 8.
    pub drop_frames: u8,
    /// Whether the sensor carries embedded data.
    pub embedded: bool,
    /// Free-running: frames on their own clock (`period_us`), and posts take `write_us` to reach
    /// the sensor, so a post whose group launch falls after a frame start lands one frame later.
    /// Without it (lock-step, the default) the frame period is the link's and a write takes no
    /// time.
    pub free_running: bool,
    /// The frame period in nanoseconds, when free-running sets one.
    pub period_ns: Option<u64>,
    /// The group write time in nanoseconds, when free-running.
    pub write_ns: u64,
    /// A negative control: the sensor answers its landing from the frame count read before the
    /// write, as a driver without the straddle rule would. Board `naive_landing = true`.
    pub naive_landing: bool,
}

impl SimConfig {
    /// The delays of the OV9782 (2 for each), no masking, no embedded data.
    pub const DEFAULT: Self = Self {
        delays: [2, 2, 2],
        drop_frames: 0,
        embedded: false,
        free_running: false,
        period_ns: None,
        write_ns: 0,
        naive_landing: false,
    };

    fn from_config(cfg: &Config<'_>) -> Result<Self> {
        let int = |key: &str, default: u8| -> Result<u8> {
            match cfg.get(key) {
                None => Ok(default),
                Some(ConfigValue::Int(v)) if (0..=8).contains(&v) => Ok(v as u8),
                Some(_) => Err(Status::INVALID_ARGUMENT),
            }
        };
        let flag = |key: &str| -> Result<bool> {
            match cfg.get(key) {
                None => Ok(false),
                Some(ConfigValue::Bool(on)) => Ok(on),
                Some(_) => Err(Status::INVALID_ARGUMENT),
            }
        };
        // Microseconds, up to ten seconds.
        let micros = |key: &str| -> Result<Option<u64>> {
            match cfg.get(key) {
                None => Ok(None),
                Some(ConfigValue::Int(v)) if (1..=10_000_000).contains(&v) => {
                    Ok(Some(v as u64 * 1_000))
                }
                Some(_) => Err(Status::INVALID_ARGUMENT),
            }
        };
        let free_running = flag("free_running")?;
        let (period_ns, write_ns) = if free_running {
            (micros("period_us")?, micros("write_us")?.unwrap_or(0))
        } else {
            (None, 0)
        };
        Ok(Self {
            delays: [
                int("exposure_delay", 2)?,
                int("gain_delay", 2)?,
                int("frame_length_delay", 2)?,
            ],
            drop_frames: int("drop_frames", 0)?,
            embedded: flag("embedded")?,
            free_running,
            period_ns,
            write_ns,
            naive_landing: flag("naive_landing")?,
        })
    }
}

/// The values in force before any post: exposure, gain code and frame length.
const INITIAL: [u32; 3] = [100, 16, 1000];
/// Exposure is at least one line; gain codes run 16 (unity) to 255; frame lengths 100 to 4000.
const GAIN_RANGE: GainRange = GainRange {
    min: 16,
    max: 255,
    unity: 16,
    step: 1,
    law: GainLaw::Linear,
};
const FRAME_LENGTH_RANGE: (u32, u32) = (100, 4000);

/// Bytes of embedded data per frame: the frame count, then exposure, gain and frame length.
pub const EMBEDDED_BYTES: usize = 8 + 3 * 4;

/// The register values one frame shows, decoded from its embedded data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub frame: u64,
    pub exposure: u32,
    pub gain: u32,
    pub frame_length: u32,
}

/// The sim's embedded-data layout, little-endian. Its own decode is [`decode_embedded`].
pub fn encode_embedded(frame: u64, values: [u32; 3]) -> [u8; EMBEDDED_BYTES] {
    let mut out = [0u8; EMBEDDED_BYTES];
    out[..8].copy_from_slice(&frame.to_le_bytes());
    for (i, value) in values.iter().enumerate() {
        out[8 + 4 * i..12 + 4 * i].copy_from_slice(&value.to_le_bytes());
    }
    out
}

/// Decodes one frame's embedded data. `None` for a buffer of the wrong length.
pub fn decode_embedded(bytes: &[u8]) -> Option<Shown> {
    let bytes: &[u8; EMBEDDED_BYTES] = bytes.try_into().ok()?;
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    Some(Shown {
        frame: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
        exposure: word(8),
        gain: word(12),
        frame_length: word(16),
    })
}

/// Frames the receiver has emitted since it started, and the schedule of the sensor's controls.
pub struct Pipeline {
    config: SimConfig,
    emitted: u64,
    /// Per control (exposure, gain, frame length): values waiting for their frame, as (frame,
    /// value), in the order they were posted. A frame is -1 before the first.
    queues: [Vec<(i64, u32)>; 3],
    in_force: [u32; 3],
    /// Time of the first frame start, and the frame period. `None` until the receiver's first tick.
    frame0_ns: Option<u64>,
    interval_ns: u64,
    /// The receiver's last tick: the sim's present.
    now_ns: u64,
    /// When set, the receiver's frame count is read as of this time instead of the present. The
    /// sensor sets it for the read after its write, which lies in the sim's future.
    read_at: Option<u64>,
    /// Posts whose group launch fell in a later frame than the one their write began in.
    straddles: u64,
}

impl Pipeline {
    const fn new() -> Self {
        Self {
            config: SimConfig::DEFAULT,
            emitted: 0,
            queues: [Vec::new(), Vec::new(), Vec::new()],
            in_force: INITIAL,
            frame0_ns: None,
            interval_ns: 1,
            now_ns: 0,
            read_at: None,
            straddles: 0,
        }
    }

    /// The frame in progress at `at_ns`: frame `k` starts at `frame0 + k * interval`. -1 before the
    /// first frame starts.
    fn frame_at(&self, at_ns: u64) -> i64 {
        match self.frame0_ns {
            Some(frame0) if at_ns >= frame0 => ((at_ns - frame0) / self.interval_ns) as i64,
            _ => -1,
        }
    }

    /// Schedules `value` for control `control` to show on frame `target`.
    fn land(&mut self, control: usize, value: u32, target: i64) {
        self.queues[control].push((target, value));
    }

    /// Emits the next frame: applies the values due on it and returns its number with the values
    /// in force for it.
    fn emit(&mut self) -> (u64, [u32; 3]) {
        let frame = self.emitted;
        self.emitted += 1;
        for (control, queue) in self.queues.iter_mut().enumerate() {
            let mut i = 0;
            while i < queue.len() {
                if queue[i].0 <= frame as i64 {
                    self.in_force[control] = queue.remove(i).1;
                } else {
                    i += 1;
                }
            }
        }
        (frame, self.in_force)
    }
}

/// Frames the receiver has emitted since it started.
pub fn emitted() -> u64 {
    pipeline().emitted
}

/// Posts whose group launch fell after a frame start that came during their writes.
pub fn straddles() -> u64 {
    pipeline().straddles
}

/// The calls logged since [`serial`], in order.
pub fn calls() -> Vec<String> {
    LOG.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Modes as (width, height, frames per second). Every mode has the same pixel rate, so frame time
/// follows from the link alone.
const MODES: [(u32, u32, u32); 2] = [(320, 40, 120), (640, 40, 60)];
const PIXEL_RATE_HZ: u64 = 1_536_000;

fn mode(index: usize, cfg: SimConfig) -> Option<Mode> {
    let (width, height, fps) = *MODES.get(index)?;
    Some(Mode {
        link: LinkConfig {
            lanes: 2,
            virtual_channel: 0,
            data_type: 0x2b,
            bit_depth: 10,
            continuous_clock: false,
            width,
            height,
            pixel_rate_hz: PIXEL_RATE_HZ,
            link_rate_mbps: 1000,
        },
        bayer: BayerOrder::Rggb,
        max_fps_milli: fps * 1000,
        crop: Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        analogue_gain: GAIN_RANGE,
        digital_gain: GainRange::default(),
        frame_length_min: FRAME_LENGTH_RANGE.0,
        frame_length_max: FRAME_LENGTH_RANGE.1,
        exposure_min: 1,
        exposure_fraction_bits: 0,
        drop_frames: cfg.drop_frames,
        exposure_delay: cfg.delays[0],
        gain_delay: cfg.delays[1],
        frame_length_delay: cfg.delays[2],
        ..Mode::default()
    })
}

/// Frame time in nanoseconds for a link: its active pixels at its pixel rate.
fn frame_interval_ns(link: &LinkConfig) -> u64 {
    let pixels = u128::from(link.width) * u128::from(link.height);
    let rate = u128::from(link.pixel_rate_hz.max(1));
    u64::try_from(pixels * 1_000_000_000 / rate)
        .unwrap_or(u64::MAX)
        .max(1)
}

/// What a sensor's read of the receiver's frame count gave.
#[derive(Clone, Copy)]
enum Probe {
    Count(FrameStarts),
    Unknown,
    Unsupported,
}

/// Where a post's landing stands once it is made.
#[derive(Clone, Copy)]
enum Outcome {
    Known(Landing),
    Unknown,
    Unsupported,
}

#[driver("sim-sensor", provides = [CameraSensor])]
pub struct SimSensor {
    selected: Option<u16>,
    streaming: bool,
    applied: Option<FrameValues>,
    /// The last post accepted, with what its landing was.
    last: Option<(FrameValues, Outcome)>,
    config: SimConfig,
    /// The receiver the board links this sensor to: where landings read the frame count from.
    #[link]
    _receiver: Option<Dev<dyn CameraReceiver>>,
}

impl Lifecycle for SimSensor {
    type Needs = SimSensorNeeds;

    fn start(needs: SimSensorNeeds, cfg: &Config<'_>) -> Result<Self> {
        let config = SimConfig::from_config(cfg)?;
        pipeline().config = config;
        Ok(Self {
            selected: None,
            streaming: false,
            applied: None,
            last: None,
            config,
            _receiver: needs._receiver,
        })
    }
}

impl CameraSensor for SimSensor {
    fn info(&mut self) -> Result<SensorInfo> {
        Ok(SensorInfo {
            modes: MODES.len() as u16,
            array_width: 1280,
            array_height: 800,
            test_patterns: 0,
            embedded_data: if self.config.embedded {
                EmbeddedData::Registers
            } else {
                EmbeddedData::None
            },
        })
    }

    fn mode(&mut self, index: u16) -> Result<Mode> {
        mode(usize::from(index), self.config).ok_or(Status::NOT_FOUND)
    }

    fn select_mode(&mut self, index: u16) -> Result<()> {
        if self.streaming {
            return Err(Status::BUSY);
        }
        if mode(usize::from(index), self.config).is_none() {
            return Err(Status::NOT_FOUND);
        }
        log("sensor.select_mode");
        self.selected = Some(index);
        Ok(())
    }

    fn selected(&mut self) -> Result<Option<u16>> {
        Ok(self.selected)
    }

    fn start(&mut self) -> Result<()> {
        if self.selected.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        log("sensor.start");
        self.streaming = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        log("sensor.stop");
        self.streaming = false;
        Ok(())
    }

    fn set_controls(&mut self, controls: Controls) -> Result<()> {
        if self.selected.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        check_limits(&controls)?;
        log("sensor.set_controls");
        // Written at once: the group launches in the frame in progress, and shows on the next.
        let now = pipeline().now_ns;
        let launch = pipeline().frame_at(now);
        schedule(&controls, launch, [0; 3]);
        Ok(())
    }

    fn post_values(&mut self, values: FrameValues) -> Result<()> {
        self.post(values, false).map(|_| ())
    }

    fn post_values_at(&mut self, values: FrameValues) -> Result<Landing> {
        match self.post(values, true)? {
            Outcome::Known(landing) => Ok(landing),
            Outcome::Unknown => Err(Status::BUSY),
            Outcome::Unsupported => Err(Status::NOT_SUPPORTED),
        }
    }

    fn applied(&mut self) -> Result<Option<FrameValues>> {
        Ok(self.applied)
    }

    fn applied_landing(&mut self) -> Result<Option<AppliedLanding>> {
        match self.last {
            None => Ok(None),
            Some((values, Outcome::Known(landing))) => Ok(Some(AppliedLanding { values, landing })),
            Some((_, Outcome::Unknown)) => Err(Status::BUSY),
            Some((_, Outcome::Unsupported)) => Err(Status::NOT_SUPPORTED),
        }
    }
    fn in_force(&mut self) -> Result<InForce> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_exposure(&mut self, _lines: u32) -> Result<Landing> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_analogue_gain(&mut self, _code: u32) -> Result<Landing> {
        Err(Status::NOT_SUPPORTED)
    }
    fn set_frame_length(&mut self, _lines: u32) -> Result<Landing> {
        Err(Status::NOT_SUPPORTED)
    }

    fn set_flips(&mut self, _horizontal: bool, _vertical: bool) -> Result<BayerOrder> {
        Err(Status::NOT_SUPPORTED)
    }

    fn set_test_pattern(&mut self, code: u16) -> Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(Status::INVALID_ARGUMENT)
        }
    }

    fn identity(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }
}

impl SimSensor {
    /// Reads the receiver's frame count as of `at_ns`, through the link.
    fn probe(&mut self, at_ns: u64) -> Result<Probe> {
        let Some(receiver) = self._receiver.as_mut() else {
            return Ok(Probe::Unsupported);
        };
        pipeline().read_at = Some(at_ns);
        let answer = receiver.frame_starts();
        pipeline().read_at = None;
        match answer {
            Ok(Some(count)) => Ok(Probe::Count(count)),
            Ok(None) => Ok(Probe::Unknown),
            Err(Status::NOT_SUPPORTED) => Ok(Probe::Unsupported),
            Err(status) => Err(status),
        }
    }

    /// One post. Its group is written from the present, and launches when the write has taken
    /// `write_ns`. The launch frame is the one in progress then (the sim's physics). The sensor
    /// answers that frame with the landing rule: the counts read before the write and after it,
    /// through `launch_frame`. With `must_know`, a post whose landing cannot be read is refused
    /// before anything is written.
    fn post(&mut self, values: FrameValues, must_know: bool) -> Result<Outcome> {
        if self.selected.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        check_limits(&values.controls)?;
        let (t0, write_ns, delays, naive) = {
            let p = pipeline();
            (
                p.now_ns,
                p.config.write_ns,
                p.config.delays,
                p.config.naive_landing,
            )
        };
        let before = match self.probe(t0)? {
            Probe::Count(count) => Ok(count),
            Probe::Unknown => Err(Outcome::Unknown),
            Probe::Unsupported => Err(Outcome::Unsupported),
        };
        if let (true, Err(outcome)) = (must_know, before) {
            return Err(match outcome {
                Outcome::Unknown => Status::BUSY,
                _ => Status::NOT_SUPPORTED,
            });
        }
        let t1 = t0 + write_ns;
        let (first, launch) = {
            let p = pipeline();
            (p.frame_at(t0), p.frame_at(t1))
        };
        if launch != first {
            pipeline().straddles += 1;
        }
        schedule(&values.controls, launch, delays);
        self.applied = Some(values);
        let outcome = match before {
            Err(outcome) => outcome,
            Ok(before) => match self.probe(t1).unwrap_or(Probe::Unknown) {
                Probe::Count(after) => {
                    let frame = if naive {
                        launch_frame(before, before)
                    } else {
                        launch_frame(before, after)
                    };
                    Outcome::Known(landing_of(frame, delays, &values.controls))
                }
                Probe::Unknown => Outcome::Unknown,
                Probe::Unsupported => Outcome::Unsupported,
            },
        };
        self.last = Some((values, outcome));
        Ok(outcome)
    }
}

/// Applies a post's controls to the pipeline: each control shows on frame `launch + delay`, where
/// `launch` is the frame its group launched in.
fn schedule(controls: &Controls, launch: i64, delays: [u8; 3]) {
    let mut p = pipeline();
    let wanted = [
        controls.exposure,
        controls.analogue_gain,
        controls.frame_length,
    ];
    for (control, value) in wanted.into_iter().enumerate() {
        if let Some(value) = value {
            p.land(control, value, launch + i64::from(delays[control]));
        }
    }
}

/// The limits a control must be inside: exposure of at least a line, gains in the gain range and
/// frame lengths in the frame length range. Digital gain has none.
fn check_limits(controls: &Controls) -> Result<()> {
    if controls.digital_gain.is_some() {
        return Err(Status::INVALID_ARGUMENT);
    }
    let fl = FRAME_LENGTH_RANGE;
    let gain = GAIN_RANGE;
    if controls.exposure == Some(0)
        || controls
            .analogue_gain
            .is_some_and(|g| !(gain.min..=gain.max).contains(&g))
        || controls
            .frame_length
            .is_some_and(|f| !(fl.0..=fl.1).contains(&f))
    {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(())
}

/// A frame stream's producer, and what it makes: one frame per interval, while running.
#[derive(Clone, Copy)]
struct Run {
    interval_ns: u64,
    next_ns: Option<u64>,
}

#[driver("sim-receiver", provides = [CameraReceiver])]
pub struct SimReceiver {
    #[needs(free)]
    frames: StreamHandle,
    /// The embedded-data stream, when the board gives the receiver one.
    #[needs(optional, free)]
    embedded: Option<StreamHandle>,
    configured: Option<LinkConfig>,
    layout: Option<FrameLayout>,
    run: Option<Run>,
    counters: Counters,
}

impl Lifecycle for SimReceiver {
    type Needs = SimReceiverNeeds;
    const TICKS: bool = true;

    fn start(needs: SimReceiverNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            frames: needs.frames,
            embedded: needs.embedded,
            configured: None,
            layout: None,
            run: None,
            counters: Counters::default(),
        })
    }

    /// Makes every frame due by `now_ns`, and returns when the next one is due.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        pipeline().now_ns = now_ns;
        let run = self.run.as_mut()?;
        let mut next = match run.next_ns {
            Some(next) => next,
            None => {
                // The first frame starts one interval after the first tick.
                let next = now_ns.saturating_add(run.interval_ns);
                run.next_ns = Some(next);
                pipeline().frame0_ns = Some(next);
                return Some(next);
            }
        };
        let size = self.layout.map_or(0, |layout| layout.size);
        let (masked, embedded_on) = {
            let p = pipeline();
            (u64::from(p.config.drop_frames), p.config.embedded)
        };
        while next <= now_ns {
            let (sequence, in_force) = pipeline().emit();
            if sequence >= masked {
                match self.frames.next_buffer() {
                    Ok(Some(buffer)) => {
                        let frame = StreamFrame {
                            buffer,
                            bytes: size,
                            sequence,
                            identity: 0,
                            timestamp_ns: next,
                        };
                        match self.frames.publish(frame) {
                            Ok(()) => self.counters.frames += 1,
                            Err(_) => self.counters.errors += 1,
                        }
                    }
                    Ok(None) => self.counters.dropped += 1,
                    Err(_) => self.counters.errors += 1,
                }
                if let (true, Some(embedded)) = (embedded_on, self.embedded.as_mut()) {
                    publish_embedded(embedded, sequence, in_force, next);
                }
            }
            next = next.saturating_add(run.interval_ns);
        }
        run.next_ns = Some(next);
        Some(next)
    }
}

impl CameraReceiver for SimReceiver {
    fn limits(&mut self) -> Result<Limits> {
        Ok(Limits {
            max_lanes: 2,
            max_link_rate_mbps: 1500,
            max_width: 4096,
            max_height: 4096,
        })
    }

    fn configure(&mut self, link: LinkConfig, packing: Packing) -> Result<FrameLayout> {
        if self.run.is_some() {
            return Err(Status::BUSY);
        }
        if link.lanes == 0
            || link.lanes > 2
            || link.width == 0
            || link.height == 0
            || link.pixel_rate_hz == 0
            || !matches!(link.bit_depth, 8 | 10 | 12)
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        log("receiver.configure");
        let line = match packing {
            Packing::Packed => (u64::from(link.width) * u64::from(link.bit_depth)).div_ceil(8),
            Packing::Unpacked16 => u64::from(link.width) * 2,
        };
        let stride = line.next_multiple_of(16);
        let layout = FrameLayout {
            width: link.width,
            height: link.height,
            bit_depth: link.bit_depth,
            packing,
            stride: u32::try_from(stride).map_err(|_| Status::INVALID_ARGUMENT)?,
            size: u32::try_from(stride * u64::from(link.height))
                .map_err(|_| Status::INVALID_ARGUMENT)?,
        };
        self.configured = Some(link);
        self.layout = Some(layout);
        Ok(layout)
    }

    fn link(&mut self) -> Result<Option<LinkConfig>> {
        Ok(self.configured)
    }

    fn frame_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = b"frames";
        debug_assert!(name.len() <= MAX_STREAM_NAME);
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    fn start(&mut self) -> Result<()> {
        let link = self.configured.ok_or(Status::INVALID_ARGUMENT)?;
        if self.run.is_some() {
            return Err(Status::BUSY);
        }
        log("receiver.start");
        self.counters = Counters::default();
        let config = pipeline().config;
        let interval_ns = config.period_ns.unwrap_or_else(|| frame_interval_ns(&link));
        *pipeline() = Pipeline {
            config,
            interval_ns,
            ..Pipeline::new()
        };
        self.run = Some(Run {
            interval_ns,
            next_ns: None,
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        log("receiver.stop");
        self.run = None;
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(self.counters)
    }

    fn feeds(&mut self, _into: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    fn embedded_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        if self.embedded.is_none() {
            return Ok(0);
        }
        let name = b"embedded";
        debug_assert!(name.len() <= MAX_EMBEDDED_STREAM_NAME);
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }

    /// The simulated wiring is not described to the kernel, so the link is unknown.
    fn source(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn layout(&mut self) -> Result<Option<FrameLayout>> {
        Ok(self.layout)
    }

    /// Frames start on the clock from the first tick, every interval. Masked frames are starts
    /// too: they are counted and their sequence is taken, but nothing is published for them.
    /// `None` while the receiver is not running or before the first frame has started.
    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        if self.run.is_none() {
            return Ok(None);
        }
        let p = pipeline();
        let at = p.read_at.unwrap_or(p.now_ns);
        let frame = p.frame_at(at);
        let Some(frame0) = p.frame0_ns.filter(|_| frame >= 0) else {
            return Ok(None);
        };
        let frame = frame as u64;
        Ok(Some(FrameStarts {
            count: frame + 1,
            latest_ns: frame0 + frame * p.interval_ns,
        }))
    }

    fn inputs(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn input(&mut self) -> Result<u32> {
        Err(Status::NOT_SUPPORTED)
    }
}

/// Writes one frame's embedded data into a free buffer and publishes it with the frame's sequence.
/// A full stream drops it, as the frame stream does.
fn publish_embedded(stream: &mut StreamHandle, sequence: u64, values: [u32; 3], at_ns: u64) {
    let Ok(Some(buffer)) = stream.next_buffer() else {
        return;
    };
    let bytes = encode_embedded(sequence, values);
    if stream.write(buffer, 0, &bytes).is_err() {
        return;
    }
    let frame = StreamFrame {
        buffer,
        bytes: EMBEDDED_BYTES as u32,
        sequence,
        identity: 0,
        timestamp_ns: at_ns,
    };
    let _ = stream.publish(frame);
}

#[driver("sim-isp", provides = [CameraIsp])]
pub struct SimIsp {
    input: Option<IspInput>,
    running: bool,
}

impl Lifecycle for SimIsp {
    type Needs = SimIspNeeds;

    fn start(_needs: SimIspNeeds, _cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            input: None,
            running: false,
        })
    }
}

impl CameraIsp for SimIsp {
    fn info(&mut self) -> Result<IspInfo> {
        Ok(IspInfo {
            max_outputs: 0,
            max_width: 4096,
            max_height: 4096,
            config_layouts: 0,
            statistics_layout: 0,
            statistics_bytes: 0,
        })
    }

    fn config_layout(&mut self, _index: u8) -> Result<ConfigLayout> {
        Err(Status::NOT_FOUND)
    }

    fn set_input(&mut self, input: IspInput) -> Result<()> {
        if self.running {
            return Err(Status::BUSY);
        }
        log("isp.set_input");
        self.input = Some(input);
        Ok(())
    }

    fn set_output(&mut self, _output: u8, _image: IspOutput) -> Result<OutputLayout> {
        Err(Status::NOT_FOUND)
    }

    fn clear_output(&mut self, _output: u8) -> Result<()> {
        Err(Status::NOT_FOUND)
    }

    fn input_layout(&mut self) -> Result<Option<OutputLayout>> {
        Err(Status::NOT_SUPPORTED)
    }

    fn output_layout(&mut self, _output: u8) -> Result<Option<OutputLayout>> {
        Err(Status::NOT_FOUND)
    }

    fn config_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn statistics_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, _output: u8, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_FOUND)
    }

    fn start(&mut self) -> Result<()> {
        if self.input.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        log("isp.start");
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        log("isp.stop");
        self.running = false;
        Ok(())
    }

    fn counters(&mut self) -> Result<IspCounters> {
        Ok(IspCounters::default())
    }
}

/// A board with one sensor, one receiver it links to, and the ISP that receiver links to. A decoy
/// receiver with no link is listed before the real one, so a tool that takes the first receiver
/// fails. `sensor_link` replaces the sensor's link when given.
pub fn board(sensor_link: Option<&str>) -> String {
    board_with(sensor_link, "", false)
}

/// A board as [`board`], with the sensor's `config` given as `config`, written inside the braces of
/// an inline table (for example `exposure_delay = 3`), and the receiver's `embedded` stream when
/// `embedded_stream` is set.
pub fn board_with(sensor_link: Option<&str>, config: &str, embedded_stream: bool) -> String {
    let link = match sensor_link {
        Some(target) => format!("link = \"{target}\""),
        None => String::new(),
    };
    let config = if config.is_empty() {
        String::new()
    } else {
        format!("config = {{ {config} }}\n")
    };
    let embedded = if embedded_stream {
        "[devices.streams.embedded]\nbuffers = 8\nbuffer_bytes = 64\n"
    } else {
        ""
    };
    format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"sim-camera\"\n\
         [[devices]]\nid = \"isp0\"\ndriver = \"sim-isp\"\n\
         [[devices]]\nid = \"csi-spare\"\ndriver = \"sim-receiver\"\n\
         [devices.streams.frames]\nbuffers = 8\nbuffer_bytes = 32000\n\
         [[devices]]\nid = \"csi0\"\ndriver = \"sim-receiver\"\nlink = \"isp0\"\n\
         [devices.streams.frames]\nbuffers = 8\nbuffer_bytes = 32000\n\
         {embedded}\
         [[devices]]\nid = \"cam0-sensor\"\ndriver = \"sim-sensor\"\n{link}\n{config}"
    )
}

/// A board with two sensors, both linked to the receiver.
pub fn board_with_two_sensors() -> String {
    format!(
        "{}[[devices]]\nid = \"cam1-sensor\"\ndriver = \"sim-sensor\"\nlink = \"csi0\"\n",
        board(Some("csi0"))
    )
}

/// The host and a bus over it, started from `text` with the simulated drivers.
pub fn bus(text: &str) -> lemnos_camera_proof::HostBus {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(text).expect("board parses"),
        &[
            entry::<SimSensor>(),
            entry::<SimReceiver>(),
            entry::<SimIsp>(),
        ],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .expect("host starts");
    lemnos_camera_proof::HostBus::new(host, clock)
}
