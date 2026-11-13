//! The OV9782 colour global-shutter image sensor on I2C, served as `camera.sensor/1`.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `address`: the 7-bit I2C address, default `0x60`.
//! - `continuous_clock`: the CSI-2 clock lane runs between lines as well, default false (the lane
//!   is gated, which needs a longer line).
//!
//! The driver only speaks to the sensor's registers. Power, the 24 MHz input clock and the reset
//! line are the board's. Binding does not touch the bus. Nothing is kept across a restart: each
//! `start` writes the whole configuration from the sensor's reset defaults upward.
//!
//! Power-up. The first `start` after the driver is created does not touch the bus. It answers at
//! once, in `PowerUp`, and the first chip-ID read is made by the first tick at least
//! [`POWER_UP_NS`] later. The datasheet's power-up timing puts the first SCCB transaction at least
//! 1 ms after the supplies are up (t2), at least 5 ms after the reset line rises (t3), and after
//! 8192 XVCLK cycles (about 0.34 ms at 24 MHz). The board's power switch is started before the
//! sensor, so the delay runs from the sensor's own start, which comes after the supplies rise. A
//! chip that still does not answer then is recovered with the backoff below, as any bus fault is.
//!
//! Modes are one table (`chip::MODES`): 1280x800, 1280x720 and a 2x2-binned 640x400, each at RAW10
//! and RAW8. A mode's array crop is reported as `Mode::crop`; the contract has no way to change a
//! crop at runtime, so a different crop is a different row of that table.
//!
//! Exposure is in sixteenths of a line (the sensor's register format). `set_controls` writes at
//! once with no group hold; `post_values` writes the same registers inside a group hold (group 0,
//! launched at the next vertical blanking) so they take effect together. Values are checked
//! against the selected mode's limits before anything is written, including an exposure already
//! held against a shortened frame.
//!
//! Timing. `post_values` writes the exposure, gain and frame length in one group, and the group is
//! launched at the next vertical blanking. A group whose launch completes during frame `F` is
//! latched at the start of frame `F + 1` and shows on frame `F + 2`: all three controls have a
//! delay of 2 in every mode, so they show together (see `contracts/camera-sensor`'s timing rule and
//! `testing/board-runs/D10.md`). The driver does not stagger.
//!
//! Landing frame. The driver can name `F` only through the board's link to its receiver. Every
//! post reads the receiver's frame-start count (`camera.receiver` `frame_starts`) before the
//! group's writes and again after them, and `F` is the later count less one, as the landing rule in
//! `contracts/camera-sensor` states. `post_values_at` answers each posted control's frame, and
//! `applied_landing` reads back the last post and that answer. With no link, or a receiver that
//! answers `NOT_SUPPORTED`, `post_values_at` refuses before writing and `applied_landing` answers
//! `NOT_SUPPORTED`; while no frame start has been counted, the same calls answer `BUSY`.
//!
//! Values from a ring. A board that gives the sensor a `values` stream (`[devices.streams.values]`
//! in its board entry, with `job_bytes` of at least 64) takes values as jobs: the consumer pushes an
//! encoded `FrameValues` on that stream's job queue, and the driver reads the queue every
//! `VALUES_POLL_NS` while streaming. Each poll applies the last decodable job it read, under group
//! hold and launched at the next vertical blanking, exactly as `post_values` does, so the same
//! timing applies from the group's write. A value is not written until the poll that reads it: the
//! write completes inside a frame at most one poll later than the post, and that frame is `F`. A
//! consumer that needs a value on a given frame `N` posts it by the frame `N - 2`, at least one poll
//! and one group write (six register transactions) earlier. Jobs that are read while the sensor is
//! stopped or recovering, or that the limits refuse, are dropped and never kept, and a `start` drops
//! whatever was posted before it. The driver keeps no value across a restart.
//!
//! Gain. Analogue gain codes are sixteenths: `0x10` is a gain of one and the law is linear
//! (`GainLaw::Linear`). The sensor has no digital gain the contract can reach.
//!
//! Embedded data. None: `SensorInfo::embedded_data` is `None`, and the driver has no read-back of
//! the values in force for a frame. A caller checks its posts from the timing rule above.
//!
//! The first frame after a start is dropped at the sensor (one frame masked through the frame
//! control block), so `Mode::drop_frames` is 0.
//!
//! The register that programs the OTP memory is never written: `chip::write` is the only write
//! path and refuses it.
//!
//! Failures. A bus error on a control call while streaming is reported to its caller and starts
//! recovery: the next ticks reprogram the sensor and restart it, with the delay between attempts
//! doubling from 250 ms and capped at 8 s, and clearing when an attempt succeeds. While recovering,
//! calls that write to the sensor return `BUSY` and health is degraded. The driver never gives up
//! and never switches the sensor off on its own. A failed `start` or `stop` is reported and retried
//! by its caller.
//!
//! The driver does not carry calibration, 3A or any tuning: those belong to the caller.

#![no_std]

pub mod chip;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ring_tests;

use chip::{
    ANA_CORE_2, ANALOGUE_GAIN, BAYER, CHIP_ID, CHIP_IDS, COMMON, DEFAULT_ADDRESS,
    DEFAULT_EXPOSURE_LINES, DIGITAL_BINNING, EXPOSURE, EXPOSURE_FRACTION_BITS, EXPOSURE_MIN,
    FIRST_FRAMES_MASKED, FLIP_BIT, FORMAT1, FORMAT2, FRAME_LENGTH, FRAME_OFF_COUNT, GATED_CLOCK,
    GROUP_ACCESS, GROUP_HOLD_END, GROUP_HOLD_START, GROUP_LAUNCH, LINE_LENGTH, MIPI_CTRL00,
    MODE_SELECT, MODES, ModeEntry, PLL2_MULTIPLIER, STANDBY, STREAMING, TEST_PATTERN,
    TEST_PATTERNS,
};
use lemnos_contract_camera_receiver::{CameraReceiver, FrameStarts, MAX_FEED_NAME};
use lemnos_contract_camera_sensor::{
    AppliedLanding, BayerOrder, CameraSensor, Controls, FrameValues, InForce, Landing, LinkCheck,
    Mode, SensorInfo, check_link, landing_of, launch_frame,
};
use lemnos_core::{
    Config, ConfigValue, Decode, Description, Dev, Health, I2c, Item, Lifecycle, MethodId, Reader,
    Result, Status, StreamHandle, Unit, driver,
};

/// The name the kernel gives this sensor, when some kernel driver binds it. None here: this driver
/// runs the sensor itself, so no kernel names it, and a link is never confirmed from this side.
const IDENTITY: Option<&[u8]> = None;

const START: MethodId = MethodId {
    contract: 0,
    method: 5,
};
const STOP: MethodId = MethodId {
    contract: 0,
    method: 6,
};
/// The controls in force, and the three setters the items for exposure, analogue gain and frame
/// length call (`camera.sensor` methods 15 to 18).
const IN_FORCE: MethodId = MethodId {
    contract: 0,
    method: 15,
};
const SET_EXPOSURE: MethodId = MethodId {
    contract: 0,
    method: 16,
};
const SET_ANALOGUE_GAIN: MethodId = MethodId {
    contract: 0,
    method: 17,
};
const SET_FRAME_LENGTH: MethodId = MethodId {
    contract: 0,
    method: 18,
};

/// The time between a sensor's `start` and its first chip-ID read. The datasheet's power-up
/// timing: at least 1 ms from the supplies to the reset line (t2), at least 5 ms from the reset
/// line to system ready (t3), and 8192 XVCLK cycles before the first SCCB transaction. 10 ms covers
/// those with the board's own rise times.
pub const POWER_UP_NS: u64 = 10_000_000;
/// First delay between recovery attempts.
const BACKOFF_FIRST_NS: u64 = 250_000_000;
/// Longest delay between recovery attempts.
const BACKOFF_MAX_NS: u64 = 8_000_000_000;

/// How often a streaming sensor reads its values ring. A posted value waits at most this long.
pub const VALUES_POLL_NS: u64 = 1_000_000;
/// How often a streaming sensor with a linked receiver asks that receiver whether it is still there.
/// A receiver that has gone (unbound, or its handle stale) takes the stream with it: see
/// [`Ov9782::lost_receiver`].
pub const RECEIVER_WATCH_NS: u64 = 100_000_000;
/// Most jobs one poll reads. The ring holds at most 64 (`lemnos-wire` `MAX_JOB_SLOTS`).
const MAX_JOBS_PER_POLL: usize = 64;
/// Bytes read of one job: an encoded `FrameValues` fits, and this runs on small stacks. A longer
/// job isn't one; the ring refuses to hand it over and stays stuck on it until the stream restarts.
const JOB_BYTES: usize = 64;

/// The delay before recovery attempt `attempt` (from 0), doubling to the cap.
const fn backoff_ns(attempt: u32) -> u64 {
    let shifted = BACKOFF_FIRST_NS << if attempt > 5 { 5 } else { attempt };
    if shifted > BACKOFF_MAX_NS {
        BACKOFF_MAX_NS
    } else {
        shifted
    }
}

/// Where the driver is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not streaming.
    Stopped,
    /// Started on a chip never read: the first chip-ID read is due at `due`, which a tick sets
    /// [`POWER_UP_NS`] after the start. Nothing has been written to the bus.
    PowerUp {
        due: Option<u64>,
    },
    Streaming,
    /// A write failed while streaming: ticks are reprogramming and restarting the sensor.
    Recovering {
        attempt: u32,
        /// When the next attempt is due. `None` until a tick has had the host's clock.
        due: Option<u64>,
    },
}

/// The control values in force: what `start` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Current {
    exposure: u32,
    analogue_gain: u32,
    frame_length: u32,
}

impl Current {
    fn defaults(mode: &ModeEntry) -> Self {
        Self {
            exposure: DEFAULT_EXPOSURE_LINES << EXPOSURE_FRACTION_BITS,
            analogue_gain: chip::ANALOGUE_GAIN_RANGE.unity,
            frame_length: mode.frame_length_default(),
        }
    }

    /// `controls` over these values, if the result is in `mode`'s limits. Nothing is written.
    fn merged(self, controls: &Controls, mode: &ModeEntry) -> Result<Self> {
        // The sensor has no digital gain the contract can reach: its range is all zero.
        if controls.digital_gain.is_some() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let merged = Self {
            exposure: controls.exposure.unwrap_or(self.exposure),
            analogue_gain: controls.analogue_gain.unwrap_or(self.analogue_gain),
            frame_length: controls.frame_length.unwrap_or(self.frame_length),
        };
        let gain = chip::ANALOGUE_GAIN_RANGE;
        if !(mode.frame_length_min()..=mode.frame_length_max()).contains(&merged.frame_length)
            || !(gain.min..=gain.max).contains(&merged.analogue_gain)
            || !(EXPOSURE_MIN..=chip::exposure_max(merged.frame_length)).contains(&merged.exposure)
        {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(merged)
    }
}

/// The bind-time check of the board's link: asks the linked receiver what is wired to its input,
/// and compares it with this sensor. No link is nothing to confirm; a receiver that can't be asked
/// fails the bind, which is retried.
fn link_check(receiver: Option<&mut Dev<dyn CameraReceiver>>) -> Result<LinkCheck> {
    let Some(receiver) = receiver else {
        return Ok(LinkCheck::Unknown(
            "the board names no receiver for this sensor",
        ));
    };
    let mut name = [0; MAX_FEED_NAME];
    let answer = receiver.source(&mut name).map(|len| &name[..len]);
    check_link(answer, IDENTITY)
}

/// What a read of the linked receiver's frame-start count gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    Count(FrameStarts),
    /// No frame start counted yet, or the receiver cannot say right now.
    Unknown,
    /// The receiver cannot count frame starts, or no receiver is linked.
    Unsupported,
}

/// Where a post's landing stands once it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Known(Landing),
    Unknown,
    Unsupported,
}

/// An OV9782 sensor.
#[driver("ov9782", provides = [CameraSensor])]
pub struct Ov9782 {
    #[needs]
    bus: I2c,
    /// Jobs carrying `FrameValues`, from the consumer. Absent when the board gives no stream.
    #[needs(optional)]
    values: Option<StreamHandle>,
    /// The receiver the board links this sensor to. Asked once, at bind, whether it is wired to
    /// this sensor ([`LinkCheck`]); kept so the link stays in the board's hands.
    #[link]
    _receiver: Option<Dev<dyn CameraReceiver>>,
    /// What the bind found about the link; reported by `health`.
    link_check: LinkCheck,
    address: u8,
    continuous_clock: bool,
    /// The selected mode and its control values in force.
    selected: Option<(u16, Current)>,
    state: State,
    flip_horizontal: bool,
    flip_vertical: bool,
    test_pattern: u16,
    applied: Option<FrameValues>,
    /// The last post accepted, with where it landed.
    last: Option<(FrameValues, Outcome)>,
    /// Set once the chip-ID read has answered. Until then a start goes through `PowerUp`.
    powered: bool,
}

impl Lifecycle for Ov9782 {
    type Needs = Ov9782Needs;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;
    const TICKS: bool = true;

    fn start(needs: Ov9782Needs, cfg: &Config<'_>) -> Result<Self> {
        let mut needs = needs;
        let link_check = link_check(needs._receiver.as_mut())?;
        Ok(Self {
            bus: needs.bus,
            values: needs.values,
            _receiver: needs._receiver,
            link_check,
            address: address_from_config(cfg)?,
            continuous_clock: continuous_clock_from_config(cfg)?,
            selected: None,
            state: State::Stopped,
            flip_horizontal: false,
            flip_vertical: false,
            test_pattern: 0,
            applied: None,
            last: None,
            powered: false,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::action("start", START));
        out.add(Item::action("stop", STOP));
        // The controls in force, and their setters. Exposure and frame length take their ranges
        // from the selected mode, so they have none before a mode is selected. Analogue gain's
        // codes are the same in every mode.
        let gain = chip::ANALOGUE_GAIN_RANGE;
        let (exposure_max, frame) = match self.selected {
            Some((index, current)) => {
                let mode = MODES[usize::from(index)].describe(self.continuous_clock);
                (
                    Some(i64::from(
                        chip::exposure_max(current.frame_length) >> EXPOSURE_FRACTION_BITS,
                    )),
                    Some((
                        i64::from(mode.frame_length_min),
                        i64::from(mode.frame_length_max),
                    )),
                )
            }
            None => (None, None),
        };
        let lines = i64::from(EXPOSURE_MIN >> EXPOSURE_FRACTION_BITS);
        let mut exposure = Item::value("exposure", IN_FORCE)
            .field("exposure_lines")
            .unit(Unit::Line, 0);
        let mut exposure_set = Item::control("exposure", SET_EXPOSURE).unit(Unit::Line, 0);
        if let Some(max) = exposure_max {
            exposure = exposure.range(lines, max).step(1);
            exposure_set = exposure_set.range(lines, max).step(1);
        }
        out.add(exposure);
        out.add(exposure_set);
        let mut frame_value = Item::value("frame_length", IN_FORCE)
            .field("frame_length")
            .unit(Unit::Line, 0);
        let mut frame_set = Item::control("frame_length", SET_FRAME_LENGTH).unit(Unit::Line, 0);
        if let Some((min, max)) = frame {
            frame_value = frame_value.range(min, max).step(1);
            frame_set = frame_set.range(min, max).step(1);
        }
        out.add(frame_value);
        out.add(frame_set);
        out.add(
            Item::value("analogue_gain", IN_FORCE)
                .field("analogue_gain")
                .range(i64::from(gain.min), i64::from(gain.max))
                .step(i64::from(gain.step)),
        );
        out.add(
            Item::control("analogue_gain", SET_ANALOGUE_GAIN)
                .range(i64::from(gain.min), i64::from(gain.max))
                .step(i64::from(gain.step)),
        );
    }

    fn health(&self) -> Health {
        // A mismatch is the first thing to report. Recovery, when it's under way, is next; an
        // unconfirmed link is what remains.
        if let LinkCheck::Mismatch(reason) = self.link_check {
            return Health::Failed(reason);
        }
        match (self.state, self.link_check) {
            (State::Recovering { .. }, _) => {
                Health::Degraded("bus error while streaming: restarting the sensor")
            }
            (State::PowerUp { .. }, _) => Health::Degraded("powering up"),
            (_, LinkCheck::Unknown(reason)) => Health::Degraded(reason),
            _ => Health::Ok,
        }
    }

    /// Streaming with a values stream: reads the ring and applies its latest job, then waits one
    /// poll. Recovering: drops what the ring holds and runs a due recovery attempt, polling too when
    /// there is a ring. Stopped: nothing is due.
    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.state {
            State::Stopped => None,
            State::PowerUp { due } => {
                let next = match due {
                    None => {
                        let at = now_ns.saturating_add(POWER_UP_NS);
                        self.state = State::PowerUp { due: Some(at) };
                        Some(at)
                    }
                    Some(at) if now_ns < at => Some(at),
                    Some(_) => match self.program() {
                        Ok(()) => {
                            self.state = State::Streaming;
                            None
                        }
                        // The first attempt failed: recovery takes over, from the bottom of its
                        // backoff.
                        Err(_) => {
                            self.state = State::Recovering {
                                attempt: 0,
                                due: None,
                            };
                            self.recover(now_ns, 0, None)
                        }
                    },
                };
                if self.values.is_some() && matches!(self.state, State::Streaming) {
                    let poll = now_ns.saturating_add(VALUES_POLL_NS);
                    Some(next.map_or(poll, |at| at.min(poll)))
                } else {
                    next
                }
            }
            State::Streaming => {
                if self.receiver_lost() {
                    self.lost_receiver();
                    return None;
                }
                self.take_values();
                let poll = self
                    .values
                    .as_ref()
                    .map(|_| now_ns.saturating_add(VALUES_POLL_NS));
                if self._receiver.is_some() {
                    let watch = now_ns.saturating_add(RECEIVER_WATCH_NS);
                    Some(poll.map_or(watch, |at| at.min(watch)))
                } else {
                    poll
                }
            }
            State::Recovering { attempt, due } => {
                self.discard_values();
                let next = self.recover(now_ns, attempt, due);
                if self.values.is_some() {
                    let poll = now_ns.saturating_add(VALUES_POLL_NS);
                    Some(next.map_or(poll, |at| at.min(poll)))
                } else {
                    next
                }
            }
        }
    }
}

impl Ov9782 {
    /// Runs a due recovery attempt. Returns when the next is due.
    fn recover(&mut self, now_ns: u64, attempt: u32, due: Option<u64>) -> Option<u64> {
        match due {
            None => {
                let at = now_ns.saturating_add(backoff_ns(attempt));
                self.state = State::Recovering {
                    attempt,
                    due: Some(at),
                };
                Some(at)
            }
            Some(at) if now_ns < at => Some(at),
            Some(_) => match self.program() {
                Ok(()) => {
                    self.state = State::Streaming;
                    None
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

    /// Reads every job the poll finds and applies the last one that decodes. A job that does not
    /// decode is skipped. A job the limits refuse, or a bus error, drops it: the earlier jobs of the
    /// same poll are dropped with it, as the later one is the one the sensor would latch.
    fn take_values(&mut self) {
        let Some(values) = self.values.as_mut() else {
            return;
        };
        let mut buf = [0u8; JOB_BYTES];
        let mut latest = None;
        for _ in 0..MAX_JOBS_PER_POLL {
            match values.next_job(&mut buf) {
                Ok(Some(len)) => {
                    if let Some(decoded) = decode_job(&buf[..len]) {
                        latest = Some(decoded);
                    }
                }
                _ => break,
            }
        }
        if let Some(values) = latest {
            let _ = self.apply(values);
        }
    }

    /// Reads and drops every job the ring holds, so none of them is applied later.
    fn discard_values(&mut self) {
        let Some(values) = self.values.as_mut() else {
            return;
        };
        let mut buf = [0u8; JOB_BYTES];
        for _ in 0..MAX_JOBS_PER_POLL {
            if !matches!(values.next_job(&mut buf), Ok(Some(_))) {
                break;
            }
        }
    }

    /// Writes `values` under group hold, checked against the limits first. A bus error while
    /// writing starts recovery; nothing is recorded as applied. Values read from the ring make no
    /// caller to answer to, so their landing is recorded and not returned.
    fn apply(&mut self, values: FrameValues) -> Result<()> {
        self.post(values, false).map(|_| ())
    }

    /// Asks the linked receiver for its frame-start count. A read that fails is not a count, so it
    /// is `Unknown` here; the caller decides whether that refuses a post.
    fn probe(&mut self) -> Probe {
        let Some(receiver) = self._receiver.as_mut() else {
            return Probe::Unsupported;
        };
        match receiver.frame_starts() {
            Ok(Some(count)) => Probe::Count(count),
            Ok(None) => Probe::Unknown,
            Err(Status::NOT_SUPPORTED) => Probe::Unsupported,
            Err(_) => Probe::Unknown,
        }
    }

    /// Posts `values` under group hold, and reads where the group lands.
    ///
    /// The count is read before the group's writes and again after them, the launch being the last
    /// write. The landing frame is `launch_frame` of the two reads (the later frame when a frame
    /// start came during the writes), and each posted control shows `delay` frames after it, with
    /// the mode's delays. With `must_know`, a post whose landing cannot be read is refused before
    /// anything is written: `BUSY` when no frame start is counted yet, `NOT_SUPPORTED` when there
    /// is no link or the receiver cannot count.
    fn post(&mut self, values: FrameValues, must_know: bool) -> Result<Outcome> {
        let (entry, current) = self.mode_entry()?;
        self.check_not_recovering()?;
        let merged = current.merged(&values.controls, entry)?;
        let mode = entry.describe(self.continuous_clock);
        let delays = [
            mode.exposure_delay,
            mode.gain_delay,
            mode.frame_length_delay,
        ];
        let before = self.probe();
        if must_know {
            match before {
                Probe::Count(_) => {}
                Probe::Unknown => return Err(Status::BUSY),
                Probe::Unsupported => return Err(Status::NOT_SUPPORTED),
            }
        }
        match self.write_group(&values.controls) {
            Ok(()) => {
                self.keep(merged);
                self.applied = Some(values);
            }
            Err(status) => return Err(self.fail(status)),
        }
        let after = self.probe();
        let outcome = match (before, after) {
            (Probe::Count(before), Probe::Count(after)) => Outcome::Known(landing_of(
                launch_frame(before, after),
                delays,
                &values.controls,
            )),
            (Probe::Unsupported, _) | (_, Probe::Unsupported) => Outcome::Unsupported,
            _ => Outcome::Unknown,
        };
        self.last = Some((values, outcome));
        Ok(outcome)
    }

    /// Writes one register.
    fn reg(&mut self, register: u16, value: u8) -> Result<()> {
        chip::write(&mut self.bus, self.address, register, value)
    }

    /// Writes `value` across `count` consecutive registers from `register`, high byte first.
    fn multi(&mut self, register: u16, value: u32, count: u16) -> Result<()> {
        for offset in 0..count {
            let shift = 8 * u32::from(count - 1 - offset);
            self.reg(register + offset, (value >> shift) as u8)?;
        }
        Ok(())
    }

    /// The selected mode, or `INVALID_ARGUMENT` before one is selected.
    fn mode_entry(&self) -> Result<(&'static ModeEntry, Current)> {
        let (index, current) = self.selected.ok_or(Status::INVALID_ARGUMENT)?;
        Ok((&MODES[usize::from(index)], current))
    }

    /// Remembers the control values in force for the selected mode.
    fn keep(&mut self, current: Current) {
        if let Some((_, slot)) = &mut self.selected {
            *slot = current;
        }
    }

    /// Whether the linked receiver has gone: unbound, or its handle no longer names it. A receiver
    /// that answers with any other status is still there.
    fn receiver_lost(&mut self) -> bool {
        match self._receiver.as_mut() {
            Some(receiver) => matches!(
                receiver.frame_starts(),
                Err(Status::NO_DEVICE | Status::STALE_HANDLE)
            ),
            None => false,
        }
    }

    /// The receiver went while the sensor streamed, so the stream went with it. The sensor is put
    /// to standby (best effort: the bus may be the thing that failed) and is Stopped, which is what
    /// a later `select_mode` and `start` need. The driver keeps nothing else across the loss; the
    /// next start programs the whole mode again.
    fn lost_receiver(&mut self) {
        let _ = self.reg(MODE_SELECT, STANDBY);
        self.state = State::Stopped;
    }

    /// Whether a method may write to the sensor now.
    fn check_not_recovering(&self) -> Result<()> {
        if matches!(self.state, State::Recovering { .. } | State::PowerUp { .. }) {
            Err(Status::BUSY)
        } else {
            Ok(())
        }
    }

    /// Records a failed write. While streaming, this starts recovery.
    fn fail(&mut self, status: Status) -> Status {
        if self.state == State::Streaming {
            self.state = State::Recovering {
                attempt: 0,
                due: None,
            };
        }
        status
    }

    /// Writes the controls that are set, exposure first when the frame is shortening so a held
    /// exposure is never outside the frame between writes.
    fn write_controls(&mut self, controls: &Controls, shortening: bool) -> Result<()> {
        if shortening {
            self.write_exposure(controls.exposure)?;
            self.write_gain(controls.analogue_gain)?;
            self.write_frame_length(controls.frame_length)
        } else {
            self.write_frame_length(controls.frame_length)?;
            self.write_exposure(controls.exposure)?;
            self.write_gain(controls.analogue_gain)
        }
    }

    fn write_exposure(&mut self, exposure: Option<u32>) -> Result<()> {
        match exposure {
            Some(exposure) => self.multi(EXPOSURE, exposure, 3),
            None => Ok(()),
        }
    }

    fn write_gain(&mut self, gain: Option<u32>) -> Result<()> {
        match gain {
            Some(gain) => self.reg(ANALOGUE_GAIN, gain as u8),
            None => Ok(()),
        }
    }

    fn write_frame_length(&mut self, frame_length: Option<u32>) -> Result<()> {
        match frame_length {
            Some(frame_length) => self.multi(FRAME_LENGTH, frame_length, 2),
            None => Ok(()),
        }
    }

    /// Writes `controls` under a group hold and launches them at the next vertical blanking.
    /// A write that fails inside the group leaves the group closed and not launched, so the next
    /// group starts clean and no later write is captured by a hold that never ended.
    fn write_group(&mut self, controls: &Controls) -> Result<()> {
        self.reg(GROUP_ACCESS, GROUP_HOLD_START)?;
        let written = self
            .write_controls(controls, false)
            .and_then(|()| self.reg(GROUP_ACCESS, GROUP_HOLD_END));
        if let Err(status) = written {
            let _ = self.reg(GROUP_ACCESS, GROUP_HOLD_END);
            return Err(status);
        }
        self.reg(GROUP_ACCESS, GROUP_LAUNCH)
    }

    /// `FORMAT1` and `FORMAT2` for the flips in force.
    fn format_regs(&self, entry: &ModeEntry) -> (u8, u8) {
        let bit = |on: bool| if on { FLIP_BIT } else { 0 };
        (
            entry.window.format1 | bit(self.flip_vertical),
            entry.window.format2 | bit(self.flip_horizontal),
        )
    }

    /// Takes the sensor to standby, writes the whole configuration of the selected mode, and
    /// starts it streaming.
    fn program(&mut self) -> Result<()> {
        let (entry, current) = self.mode_entry()?;
        let id = chip::read_u16(&mut self.bus, self.address, CHIP_ID)?;
        if !CHIP_IDS.contains(&id) {
            return Err(Status::NO_DEVICE);
        }
        self.powered = true;

        self.reg(MODE_SELECT, STANDBY)?;
        self.reg(
            MIPI_CTRL00,
            if self.continuous_clock {
                0
            } else {
                GATED_CLOCK
            },
        )?;
        for &(register, value) in COMMON {
            self.reg(register, value)?;
        }

        self.reg(PLL2_MULTIPLIER, entry.depth.pll2_multiplier)?;
        self.reg(ANA_CORE_2, entry.depth.ana_core_2)?;

        let window = &entry.window;
        self.reg(0x3778, window.analog)?;
        self.multi(0x3800, window.array[0].into(), 2)?;
        self.multi(0x3802, window.array[1].into(), 2)?;
        self.multi(0x3804, window.array[2].into(), 2)?;
        self.multi(0x3806, window.array[3].into(), 2)?;
        self.multi(0x3808, window.width.into(), 2)?;
        self.multi(0x380a, window.height.into(), 2)?;
        self.multi(0x3810, window.offset.0.into(), 2)?;
        self.multi(0x3812, window.offset.1.into(), 2)?;
        self.reg(0x3814, window.x_inc)?;
        self.reg(0x3815, window.y_inc)?;
        let (format1, format2) = self.format_regs(entry);
        self.reg(FORMAT1, format1)?;
        self.reg(FORMAT2, format2)?;
        self.reg(DIGITAL_BINNING, window.digital_binning)?;
        self.reg(0x4003, window.blc.target)?;
        self.reg(0x4008, window.blc.up.0)?;
        self.reg(0x4009, window.blc.up.1)?;
        self.reg(0x400c, window.blc.down.0)?;
        self.reg(0x400d, window.blc.down.1)?;
        self.reg(0x4507, window.blc.read_4507)?;
        self.reg(0x4509, window.blc.read_4509)?;

        let line_length = chip::line_length(self.continuous_clock);
        self.multi(LINE_LENGTH, line_length, 2)?;
        self.multi(FRAME_LENGTH, current.frame_length, 2)?;
        self.multi(EXPOSURE, current.exposure, 3)?;
        self.reg(ANALOGUE_GAIN, current.analogue_gain as u8)?;

        self.reg(FRAME_OFF_COUNT, FIRST_FRAMES_MASKED)?;
        let pattern = pattern_value(self.test_pattern);
        self.reg(TEST_PATTERN, pattern)?;

        self.reg(MODE_SELECT, STREAMING)
    }
}

impl CameraSensor for Ov9782 {
    fn info(&mut self) -> Result<SensorInfo> {
        Ok(chip::sensor_info())
    }

    fn mode(&mut self, index: u16) -> Result<Mode> {
        MODES
            .get(usize::from(index))
            .map(|entry| entry.describe(self.continuous_clock))
            .ok_or(Status::NOT_FOUND)
    }

    fn select_mode(&mut self, index: u16) -> Result<()> {
        // A stream still marked running may have lost its receiver before the watch ran: then it
        // is over, and the sensor is free to take a mode.
        if self.state == State::Streaming && self.receiver_lost() {
            self.lost_receiver();
        }
        if self.state != State::Stopped {
            return Err(Status::BUSY);
        }
        let entry = MODES.get(usize::from(index)).ok_or(Status::NOT_FOUND)?;
        self.selected = Some((index, Current::defaults(entry)));
        Ok(())
    }

    fn selected(&mut self) -> Result<Option<u16>> {
        Ok(self.selected.map(|(index, _)| index))
    }

    fn start(&mut self) -> Result<()> {
        if self.selected.is_none() {
            return Err(Status::INVALID_ARGUMENT);
        }
        if matches!(self.state, State::Streaming | State::PowerUp { .. }) {
            return Ok(());
        }
        self.check_not_recovering()?;
        self.discard_values();
        if !self.powered {
            // The first start: the chip gets its power-up time before it is read (`POWER_UP_NS`).
            self.state = State::PowerUp { due: None };
            return Ok(());
        }
        self.program()?;
        self.state = State::Streaming;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if self.state == State::Stopped {
            return Ok(());
        }
        if matches!(self.state, State::PowerUp { .. }) {
            // Nothing was written: the chip was never read.
            self.state = State::Stopped;
            return Ok(());
        }
        self.reg(MODE_SELECT, STANDBY)?;
        self.state = State::Stopped;
        Ok(())
    }

    fn set_controls(&mut self, controls: Controls) -> Result<()> {
        let (entry, current) = self.mode_entry()?;
        self.check_not_recovering()?;
        let merged = current.merged(&controls, entry)?;
        let shortening = merged.frame_length < current.frame_length;
        match self.write_controls(&controls, shortening) {
            Ok(()) => {
                self.keep(merged);
                Ok(())
            }
            Err(status) => Err(self.fail(status)),
        }
    }

    fn post_values(&mut self, values: FrameValues) -> Result<()> {
        self.mode_entry()?;
        self.check_not_recovering()?;
        if self.state != State::Streaming {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.apply(values)
    }

    fn post_values_at(&mut self, values: FrameValues) -> Result<Landing> {
        self.mode_entry()?;
        self.check_not_recovering()?;
        if self.state != State::Streaming {
            return Err(Status::INVALID_ARGUMENT);
        }
        match self.post(values, true)? {
            Outcome::Known(landing) => Ok(landing),
            Outcome::Unknown => Err(Status::BUSY),
            Outcome::Unsupported => Err(Status::NOT_SUPPORTED),
        }
    }

    fn applied(&mut self) -> Result<Option<FrameValues>> {
        Ok(self.applied)
    }

    fn in_force(&mut self) -> Result<InForce> {
        Ok(match self.selected {
            Some((_, current)) => InForce {
                exposure_lines: Some(current.exposure >> EXPOSURE_FRACTION_BITS),
                analogue_gain: Some(current.analogue_gain),
                frame_length: Some(current.frame_length),
            },
            None => InForce::default(),
        })
    }

    // Each setter is a post of one control, so it lands as `post_values_at` lands it: the same
    // refusals, and the same frame.
    fn set_exposure(&mut self, lines: u32) -> Result<Landing> {
        if lines > u32::MAX >> EXPOSURE_FRACTION_BITS {
            return Err(Status::INVALID_ARGUMENT);
        }
        let controls = Controls {
            exposure: Some(lines << EXPOSURE_FRACTION_BITS),
            ..Controls::default()
        };
        self.post_values_at(FrameValues {
            identity: 0,
            controls,
        })
    }

    fn set_analogue_gain(&mut self, code: u32) -> Result<Landing> {
        let controls = Controls {
            analogue_gain: Some(code),
            ..Controls::default()
        };
        self.post_values_at(FrameValues {
            identity: 0,
            controls,
        })
    }

    fn set_frame_length(&mut self, lines: u32) -> Result<Landing> {
        let controls = Controls {
            frame_length: Some(lines),
            ..Controls::default()
        };
        self.post_values_at(FrameValues {
            identity: 0,
            controls,
        })
    }

    fn applied_landing(&mut self) -> Result<Option<AppliedLanding>> {
        match self.last {
            None => Ok(None),
            Some((values, Outcome::Known(landing))) => Ok(Some(AppliedLanding { values, landing })),
            Some((_, Outcome::Unknown)) => Err(Status::BUSY),
            Some((_, Outcome::Unsupported)) => Err(Status::NOT_SUPPORTED),
        }
    }

    fn set_flips(&mut self, horizontal: bool, vertical: bool) -> Result<BayerOrder> {
        self.check_not_recovering()?;
        let (old_horizontal, old_vertical) = (self.flip_horizontal, self.flip_vertical);
        self.flip_horizontal = horizontal;
        self.flip_vertical = vertical;
        if let Some((index, _)) = self.selected {
            let entry = &MODES[usize::from(index)];
            let (format1, format2) = self.format_regs(entry);
            let written = self
                .reg(FORMAT1, format1)
                .and_then(|()| self.reg(FORMAT2, format2));
            if let Err(status) = written {
                self.flip_horizontal = old_horizontal;
                self.flip_vertical = old_vertical;
                return Err(self.fail(status));
            }
        }
        Ok(BAYER)
    }

    fn set_test_pattern(&mut self, code: u16) -> Result<()> {
        if usize::from(code) > TEST_PATTERNS.len() {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.check_not_recovering()?;
        match self.reg(TEST_PATTERN, pattern_value(code)) {
            Ok(()) => {
                self.test_pattern = code;
                Ok(())
            }
            Err(status) => Err(self.fail(status)),
        }
    }

    /// The kernel names this sensor only when a kernel driver binds it, and this one has none.
    fn identity(&mut self, into: &mut [u8]) -> Result<usize> {
        let name = IDENTITY.ok_or(Status::NOT_SUPPORTED)?;
        into.get_mut(..name.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(name);
        Ok(name.len())
    }
}

/// The `TEST_PATTERN` register value for a contract code. Code 0 is no pattern.
fn pattern_value(code: u16) -> u8 {
    match code {
        0 => 0,
        code => TEST_PATTERNS
            .get(usize::from(code) - 1)
            .copied()
            .unwrap_or(0),
    }
}

/// One job as the consumer encoded it: exactly one `FrameValues`, with no bytes left over.
fn decode_job(job: &[u8]) -> Option<FrameValues> {
    let mut from = Reader::new(job);
    let values = FrameValues::decode(&mut from).ok()?;
    (from.remaining() == 0).then_some(values)
}

/// The `address` config: a 7-bit address that is not reserved.
fn address_from_config(cfg: &Config<'_>) -> Result<u8> {
    match cfg.get("address") {
        None => Ok(DEFAULT_ADDRESS),
        Some(ConfigValue::Int(address)) if (0x08..=0x77).contains(&address) => Ok(address as u8),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The `continuous_clock` config, default false.
fn continuous_clock_from_config(cfg: &Config<'_>) -> Result<bool> {
    match cfg.get("continuous_clock") {
        None => Ok(false),
        Some(ConfigValue::Bool(on)) => Ok(on),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}
