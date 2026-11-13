//! The `camera.isp/1` driver: the ISP's tuning registers in, its statistics registers out.
//!
//! It reaches the block one of two ways, and the board file chooses which (see Settings): through
//! the register requests and the interrupt events of the vendor kernel's `vvcam-isp`, or through
//! the configuration role of the block as the Lemnos kernel module drives it, with the ISP bound
//! to the module, beside `k230-capture` running frames through the module's frames role. Either
//! way every register access goes through the sys crate's table. It does no 3A and holds no
//! tuning: a
//! configuration is a list of register writes ([`config`](crate::config)) whose values it never
//! reads, and statistics are the statistics registers copied out unread
//! ([`stats`](crate::stats)).
//!
//! # Frames: what this driver does and does not do
//!
//! It does **not** deliver frames, and it does not set up the frame path: it programs neither
//! the sensor, the receiver, the ISP's input nor its memory interface, and no image ever reaches
//! a Lemnos stream through it. The ISP has statistics to give only while something else runs
//! frames through it (`k230-capture`, on either way). So [`IspInfo::max_outputs`] is 0, `set_input` is `NOT_SUPPORTED` (the input
//! is whatever the path that feeds the ISP delivers), every output is `NOT_FOUND`, and
//! `input_stream` is `NOT_SUPPORTED`. A job names no input buffer and no output buffer. The vendor
//! node can't be given a buffer whose range is checked, so frames go through
//! [`K230IspFrames`](crate::K230IspFrames) and `lemnos.ko` instead.
//!
//! # Streams
//!
//! - `config`: jobs ([`IspJob`]) are posted on its ring, and its buffers hold configurations in
//!   [`LAYOUT_WRITES`]. Every buffer of this stream stays with the driver. The driver copies a
//!   job's configuration out **once**, into its own memory, and only that copy is checked and
//!   written.
//! - `statistics`: one buffer per finished job, filled in [`LAYOUT_STATISTICS`], carrying the
//!   job's identity, the frame's number (frames seen leaving the ISP since the driver started)
//!   and the time the kernel took the frame's interrupt.
//!
//! # Configuration buffers
//!
//! This driver follows the contract's configuration buffer rules, 2 to 5 (the `# Configuration
//! buffers` section of the `camera.isp/1` contract). It reads a job's buffer once, when the job is
//! taken off the ring, into its own copy, and checks and programs from that copy. It accepts one
//! job at a time and takes the next only when the one with the block has completed, so no two
//! accepted jobs hold a configuration buffer together: a job that names a buffer the block is
//! still using waits on the ring and is never refused for it.
//!
//! # Layouts
//!
//! `input_layout` is `NOT_SUPPORTED`, as `input_stream` is. `output_layout` is `NOT_FOUND` for
//! every output, since there are none.
//!
//! # A job, step by step
//!
//! One job is with the block at a time; the next waits on the ring.
//!
//! 1. The job names layout [`LAYOUT_WRITES`], a config buffer on the config stream that this
//!    driver holds, no input buffer and no output buffer.
//! 2. The configuration is read into the driver's copy and checked whole
//!    ([`config::check`](crate::config::check)): every write names a register in the sys crate's
//!    table and sets only that register's bits. A job that fails 1 or 2 is dropped and nothing of
//!    it reaches the block.
//! 3. Queued interrupts are taken, the writes are made in order, then `isp_ctrl`'s
//!    `GEN_CFG_UPD`, which makes the ISP take them at its next frame boundary. Interrupts queued
//!    meanwhile are taken again and not counted for the job.
//! 4. The first frame to leave the ISP after that may have started before the update; the
//!    second started after the first ended, so the update was in force for all of it. When the
//!    second frame-out interrupt is seen, and no later one is queued, the statistics registers
//!    are read. If another frame left while they were read, they may be torn, and the job counts
//!    as an error; otherwise they are written into a statistics buffer and published.
//! 5. A job whose frame passed between two looks, or that sees no such frame within
//!    [`JOB_TIMEOUT_NS`] (no frames running), counts as an error and the next job goes.
//!
//! Nothing on the way allocates.
//!
//! # Settings
//!
//! The way to the block is the board file's, and there is no falling back from one to the other:
//!
//! - **The module**: `config` gives `block`, the ISP's platform device name as `lemnos.ko` lists
//!   it (the ISP bound to the module and listed in its `k230_isp` parameter), and optionally
//!   `node`, the module's control node, default `/dev/lemnos`. The driver holds the block's
//!   configuration session; see [`ModuleHardware`](crate::hw::ModuleHardware). `device` must not
//!   be set.
//! - **The vendor device**: no `block`; `config` may give `device`, the vendor driver's node,
//!   default `/dev/vvcam-isp.0`.
//!
//! `block` with `device`, or an empty `block`, is refused when the device binds
//! (`INVALID_ARGUMENT`). Neither way falls back to the other.
//!
//! # Failures
//!
//! A failed request starts recovery, and so does a reset of the block by the module's frames role
//! (which undoes what was written): the device is closed and opened again on the next ticks,
//! with the delay between attempts doubling from 250 ms and capped at 8 s, clearing when one
//! succeeds. A job with the block when it failed counts as an error. The driver never gives up and
//! never turns itself off; `stop` is what ends it. While recovering, health is degraded.

use lemnos_contract_camera_isp::{
    CameraIsp, ConfigLayout, Counters, IspInfo, IspInput, IspJob, IspOutput, MAX_STREAM_NAME,
    NO_BUFFER, OutputLayout,
};
use lemnos_core::{
    Config, ConfigError, Decode, Description, Health, Item, Key, Kind, Lifecycle, MethodId, Reader,
    Result, Status, StreamHandle, driver,
};
use lemnos_driver_k230isp_sys::regs::{CTRL_GEN_CFG_UPD, ISP_CTRL, MIS_FRAME_OUT};
use lemnos_hal::blocks::StreamFrame;

use crate::config::{self, CONFIG_BYTES, LAYOUT_WRITES};
use crate::frames::NODE;
use crate::hw::{Hardware, LinuxConfigSession, LinuxHardware, ModuleHardware, Settings};
use crate::stats::{self, LAYOUT_STATISTICS, STATISTICS_BYTES};

const START: MethodId = MethodId {
    contract: 0,
    method: 10,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 11,
};

/// The stream jobs and configurations come on: the need's name.
pub const CONFIG_STREAM: &str = "config";
/// The stream statistics go out on.
pub const STATISTICS_STREAM: &str = "statistics";
/// Most buffers a stream this driver uses may have: one bit each in a `u64`.
pub const MAX_STREAM_BUFFERS: u32 = 64;
/// How long a job waits for its frame before it counts as an error.
pub const JOB_TIMEOUT_NS: u64 = 2_000_000_000;
/// Frame-out interrupts after a job's update before its statistics are read (see step 4).
pub const FRAMES_TO_SETTLE: u64 = 2;
/// Bytes of an encoded job at most: every field at its widest, with room.
const MAX_JOB_BYTES: usize = 128;
/// Jobs looked at in one tick at most.
const MAX_JOBS_PER_TICK: usize = 64;
/// Interrupts taken in one look at most: more than the kernel queues.
const MAX_EVENTS: usize = 64;
/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// How often the block and the streams are looked at while running.
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

/// Whether bit `index` of `mask` is set; `false` past the mask.
pub(crate) const fn has(mask: u64, index: u32) -> bool {
    index < 64 && mask & (1 << index) != 0
}

/// `mask` with bit `index` set or cleared; unchanged past the mask.
pub(crate) const fn with(mask: u64, index: u32, set: bool) -> u64 {
    if index >= 64 {
        mask
    } else if set {
        mask | (1 << index)
    } else {
        mask & !(1 << index)
    }
}

/// The lowest set bit of `mask`.
pub(crate) const fn lowest(mask: u64) -> Option<u32> {
    if mask == 0 {
        None
    } else {
        Some(mask.trailing_zeros())
    }
}

/// The job with the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    identity: u64,
    /// The frame-out count at which its statistics are read.
    frame: u64,
    /// When it was taken (the host's clock).
    taken_ns: u64,
}

/// Frames seen leaving the ISP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Frames {
    /// Frame-out interrupts since the driver was made.
    out: u64,
    /// The kernel's time of the last one.
    last_ns: u64,
}

/// Takes the buffers `stream` has handed back into `mask`, at most one round of the stream.
pub(crate) fn take_returned(stream: &mut StreamHandle, mask: &mut u64) -> Result<()> {
    for _ in 0..MAX_STREAM_BUFFERS {
        let Some(buffer) = stream.next_buffer().map_err(|_| Status::IO)? else {
            break;
        };
        *mask = with(*mask, buffer, true);
    }
    Ok(())
}

/// The buffer a statistics result goes into, which the driver picks: one it holds, or else one the
/// stream hands out now. `None` when neither is there: the job's result cannot be published, and
/// the stream counts the ask as refused.
pub(crate) fn statistics_buffer(stream: &mut StreamHandle, mask: &mut u64) -> Result<Option<u32>> {
    if let Some(buffer) = lowest(*mask) {
        return Ok(Some(buffer));
    }
    let Some(buffer) = stream.ask_buffer().map_err(|_| Status::IO)? else {
        return Ok(None);
    };
    *mask = with(*mask, buffer, true);
    Ok(Some(buffer))
}

/// The ISP, behind `vvcam-isp` or through `lemnos.ko`'s configuration role, as `camera.isp/1`.
#[driver("k230isp", provides = [CameraIsp])]
pub struct K230Isp {
    #[needs(job)]
    config: StreamHandle,
    #[needs(job)]
    statistics: StreamHandle,
    hw: Box<dyn Hardware>,
    state: State,
    counters: Counters,
    /// Buffers on the config stream, set at `start`.
    config_buffers: u32,
    /// The stream buffers this driver holds, one bit per buffer.
    held_config: u64,
    held_statistics: u64,
    pending: Option<Pending>,
    frames: Frames,
    /// The driver's copy of a job's configuration: exactly [`CONFIG_BYTES`].
    copy: Box<[u8]>,
    /// One statistics buffer's bytes, as read.
    stats: [u8; STATISTICS_BYTES],
}

/// The settings this driver reads from its board file config.
const KEYS: &[Key] = &[
    Key::optional("block", Kind::Str),
    Key::optional("device", Kind::Str),
    Key::optional("node", Kind::Str),
];

impl Lifecycle for K230Isp {
    type Needs = K230IspNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        if cfg.str("block") == Some("") {
            return Err(ConfigError::new(
                "block",
                "must be a platform device name, not empty",
            ));
        }
        // One way to the ISP, not both. Which way the device has is checked at bind, against its needs.
        if cfg.str("block").is_some() && cfg.str("device").is_some() {
            return Err(ConfigError::new("device", "cannot be given with block"));
        }
        if cfg.str("device") == Some("") {
            return Err(ConfigError::new(
                "device",
                "must be a device path, not empty",
            ));
        }
        Ok(())
    }

    fn start(needs: K230IspNeeds, cfg: &Config<'_>) -> Result<Self> {
        let hw: Box<dyn Hardware> = match (cfg.str("block"), cfg.str("device")) {
            (Some(block), None) if !block.is_empty() => {
                let node = std::path::PathBuf::from(cfg.str("node").unwrap_or(NODE));
                Box::new(ModuleHardware::new(Box::new(LinuxConfigSession::new(
                    node,
                    block.to_owned(),
                ))))
            }
            (None, device) => Box::new(LinuxHardware::new(Settings::new(device)?)),
            // Both ways at once, or no block: the board file is wrong, and nothing is guessed.
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        Ok(Self::with_hardware(hw, needs))
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
    }

    fn health(&self) -> Health {
        match self.state {
            State::Recovering { .. } => {
                Health::Degraded("the ISP's device failed while running: opening it again")
            }
            _ => Health::Ok,
        }
    }

    /// Carries jobs and statistics when running, and runs a due recovery attempt.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::Running => {
                if self.pump(now_ns).is_err() {
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

impl K230Isp {
    /// A driver over `hw` with the streams in `needs`, for a simulated board or a test.
    #[must_use]
    pub fn with_hardware(hw: Box<dyn Hardware>, needs: K230IspNeeds) -> Self {
        Self {
            config: needs.config,
            statistics: needs.statistics,
            hw,
            state: State::Stopped,
            counters: Counters::default(),
            config_buffers: 0,
            held_config: 0,
            held_statistics: 0,
            pending: None,
            frames: Frames::default(),
            copy: vec![0; CONFIG_BYTES].into_boxed_slice(),
            stats: [0; STATISTICS_BYTES],
        }
    }

    fn error(&mut self) {
        self.counters.errors = self.counters.errors.saturating_add(1);
    }

    /// Ends the job with the block as an error: its frame will not be looked for.
    fn abandon(&mut self) {
        if self.pending.take().is_some() {
            self.error();
        }
    }

    fn fail(&mut self) {
        self.hw.close();
        self.abandon();
        self.state = State::Recovering {
            attempt: 0,
            due: None,
        };
    }

    /// Opens the device from nothing.
    fn begin(&mut self) -> Result<()> {
        self.hw.close();
        self.abandon();
        self.hw.open()
    }

    /// Takes every queued interrupt and counts the frames that left the ISP.
    fn take_events(&mut self) -> Result<()> {
        for _ in 0..MAX_EVENTS {
            let Some(event) = self.hw.next_event()? else {
                break;
            };
            if event.status & MIS_FRAME_OUT != 0 {
                self.frames.out = self.frames.out.wrapping_add(1);
                self.frames.last_ns = event.timestamp_ns;
            }
        }
        Ok(())
    }

    /// Stream buffers back, interrupts in, a finished job's statistics out, the next job in. An
    /// error means a request to the device or a stream failed.
    fn pump(&mut self, now_ns: u64) -> Result<()> {
        take_returned(&mut self.config, &mut self.held_config)?;
        take_returned(&mut self.statistics, &mut self.held_statistics)?;
        self.take_events()?;
        self.finish(now_ns)?;
        if self.pending.is_none() {
            self.take_jobs(now_ns)?;
        }
        Ok(())
    }

    /// Reads and delivers the statistics of the job with the block, when its frame has left.
    fn finish(&mut self, now_ns: u64) -> Result<()> {
        let Some(p) = self.pending else {
            return Ok(());
        };
        if self.frames.out < p.frame {
            if now_ns.saturating_sub(p.taken_ns) > JOB_TIMEOUT_NS {
                self.abandon();
            }
            return Ok(());
        }
        self.pending = None;
        if self.frames.out > p.frame {
            // A later frame has left since: the registers hold its measurements.
            self.error();
            return Ok(());
        }
        for (addr, into) in stats::registers().zip(self.stats.as_chunks_mut::<4>().0) {
            *into = self.hw.read(addr)?.to_le_bytes();
        }
        self.take_events()?;
        if self.frames.out != p.frame {
            // Another frame left while they were read.
            self.error();
            return Ok(());
        }
        let Some(buffer) = statistics_buffer(&mut self.statistics, &mut self.held_statistics)?
        else {
            self.error();
            return Ok(());
        };
        let frame = StreamFrame {
            buffer,
            bytes: STATISTICS_BYTES as u32,
            sequence: p.frame,
            identity: p.identity,
            timestamp_ns: self.frames.last_ns,
        };
        let reached = match self.statistics.write(buffer, 0, &self.stats) {
            Ok(true) => true,
            // Nobody to land on: hand the buffer back empty.
            Ok(false) => false,
            Err(_) => {
                self.error();
                return Ok(());
            }
        };
        let frame = if reached {
            frame
        } else {
            StreamFrame { bytes: 0, ..frame }
        };
        if self.statistics.publish(frame).is_err() {
            self.error();
            return Ok(());
        }
        self.held_statistics = with(self.held_statistics, buffer, false);
        if reached {
            self.counters.jobs = self.counters.jobs.saturating_add(1);
        } else {
            self.error();
        }
        Ok(())
    }

    /// Takes posted jobs until one goes to the block, counting the others as dropped.
    fn take_jobs(&mut self, now_ns: u64) -> Result<()> {
        let mut encoded = [0u8; MAX_JOB_BYTES];
        for _ in 0..MAX_JOBS_PER_TICK {
            let Some(len) = self.config.next_job(&mut encoded).map_err(|_| Status::IO)? else {
                break;
            };
            let job = encoded
                .get(..len)
                .and_then(|bytes| IspJob::decode(&mut Reader::new(bytes)).ok());
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look.
            take_returned(&mut self.config, &mut self.held_config)?;
            let sent = match job {
                Some(job) => self.submit(&job, now_ns)?,
                None => false,
            };
            if sent {
                break;
            }
            self.counters.dropped = self.counters.dropped.saturating_add(1);
        }
        Ok(())
    }

    /// Writes one job's configuration. `false` for a job that can't go, which is the caller's
    /// mistake; an error means a request to the device failed.
    fn submit(&mut self, job: &IspJob, now_ns: u64) -> Result<bool> {
        if job.layout != LAYOUT_WRITES
            || job.config_buffer >= self.config_buffers
            || !has(self.held_config, job.config_buffer)
            || job.input_buffer != NO_BUFFER
            || job.outputs.iter().any(|&b| b != NO_BUFFER)
        {
            return Ok(false);
        }
        // Frames that left before now are behind the job, whatever happens to it.
        self.take_events()?;
        // The one read of the caller's buffer. Everything after uses the copy.
        match self.config.read(job.config_buffer, 0, &mut self.copy) {
            Ok(true) => {}
            Ok(false) | Err(_) => return Ok(false),
        }
        // Checked whole before the first write.
        let Some(writes) = config::check(&self.copy) else {
            return Ok(false);
        };
        for (addr, value) in writes {
            self.hw.write(addr, value)?;
        }
        self.hw.write(ISP_CTRL, CTRL_GEN_CFG_UPD)?;
        // Frames that left before the update was asked for don't count for the job.
        self.take_events()?;
        self.pending = Some(Pending {
            identity: job.identity,
            frame: self.frames.out.wrapping_add(FRAMES_TO_SETTLE),
            taken_ns: now_ns,
        });
        Ok(true)
    }
}

/// Writes a stream name into `into`.
pub(crate) fn name(name: &str, into: &mut [u8]) -> Result<usize> {
    let bytes = name.as_bytes();
    into.get_mut(..bytes.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(bytes);
    Ok(bytes.len())
}

const _: () = {
    assert!(CONFIG_STREAM.len() <= MAX_STREAM_NAME);
    assert!(STATISTICS_STREAM.len() <= MAX_STREAM_NAME);
    assert!(CONFIG_BYTES <= u32::MAX as usize);
    assert!(STATISTICS_BYTES <= u32::MAX as usize);
};

impl CameraIsp for K230Isp {
    fn info(&mut self) -> Result<IspInfo> {
        Ok(IspInfo {
            max_outputs: 0,
            max_width: 0,
            max_height: 0,
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
            layout: LAYOUT_WRITES,
            bytes: CONFIG_BYTES as u32,
        })
    }

    fn set_input(&mut self, _input: IspInput) -> Result<()> {
        // The frame path is not this driver's: it takes what feeds the ISP.
        Err(Status::NOT_SUPPORTED)
    }

    fn set_output(&mut self, _output: u8, _image: IspOutput) -> Result<OutputLayout> {
        Err(Status::NOT_FOUND)
    }

    fn clear_output(&mut self, _output: u8) -> Result<()> {
        Err(Status::NOT_FOUND)
    }

    /// `NOT_SUPPORTED`: no raw frame is ever in a buffer this driver's consumer writes (see
    /// `input_stream`).
    fn input_layout(&mut self) -> Result<Option<OutputLayout>> {
        Err(Status::NOT_SUPPORTED)
    }

    /// Every output is `NOT_FOUND`: this driver has none (`max_outputs` is 0).
    fn output_layout(&mut self, _output: u8) -> Result<Option<OutputLayout>> {
        Err(Status::NOT_FOUND)
    }

    fn config_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(CONFIG_STREAM, into)
    }

    fn statistics_stream(&mut self, into: &mut [u8]) -> Result<usize> {
        name(STATISTICS_STREAM, into)
    }

    fn input_stream(&mut self, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_SUPPORTED)
    }

    fn output_stream(&mut self, _output: u8, _into: &mut [u8]) -> Result<usize> {
        Err(Status::NOT_FOUND)
    }

    fn start(&mut self) -> Result<()> {
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        let count = |n: u32| {
            if n > MAX_STREAM_BUFFERS {
                Err(Status::INVALID_ARGUMENT)
            } else {
                Ok(n)
            }
        };
        let config = count(self.config.buffer_count())?;
        count(self.statistics.buffer_count())?;
        self.counters = Counters::default();
        self.begin()?;
        self.config_buffers = config;
        self.state = State::Running;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Stop means stop, whatever the driver was doing. Recovery is over either way.
        self.state = State::Stopped;
        self.hw.close();
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
    fn masks_ignore_bits_past_their_width() {
        assert!(!has(u64::MAX, 64));
        assert!(!has(u64::MAX, u32::MAX));
        assert_eq!(with(0, 64, true), 0);
        assert_eq!(with(0, 63, true), 1 << 63);
        assert_eq!(lowest(0b1000), Some(3));
        assert_eq!(lowest(0), None);
    }
}
