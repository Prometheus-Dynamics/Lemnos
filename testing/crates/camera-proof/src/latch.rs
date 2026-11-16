//! The sensor latch: when a posted exposure lands in the frames.
//!
//! The camera runs through `lemnosd` (mode 0, the scene) at a short exposure, and for each offset
//! after a frame's start posts a long exposure, then the short one again. The frame a post lands
//! on is the first whose brightness is past halfway to the long exposure's. The prediction is the
//! frame the call completed in plus 2: each post is counted as landing on it, earlier or later.
//! Then two posts in one frame, a middle exposure at 4 ms and the long one at 12 ms: which shows.
//!
//! Time is the caller's ([`Timing`]): on a board the offsets are waited for on `CLOCK_MONOTONIC`
//! and the sensor makes its own frames ([`Monotonic`]); in the simulator the clock moves only when
//! the simulated sensor makes a start, so the same code gives the same numbers on any host.

use std::time::{Duration, Instant};

use lemnos_client::{FrameStream, HeldFrame};
use lemnos_contract_camera_sensor::{Controls, FrameValues};

use crate::{Camera, Error, Options, SocketBus, assemble_with};

/// The offsets (µs after a frame's start) the latch posts at, in order.
pub const OFFSETS_US: [u64; 11] = [
    500, 2000, 4000, 6000, 8000, 10000, 12000, 14000, 15000, 16000, 16500,
];

/// The clock a latch reads and waits on, and what makes the next frame.
pub trait Timing {
    /// The time now, in nanoseconds, on the clock frame starts are stamped with.
    fn now_ns(&self) -> u64;
    /// Lets time pass until `ns`. Returns at or after it.
    fn wait_until(&self, ns: u64);
    /// Called before each frame is read. A board's sensor makes its frames on its own.
    fn before_frame(&self);
}

/// `CLOCK_MONOTONIC`, spun on: the board's timing.
pub struct Monotonic;

impl Timing for Monotonic {
    fn now_ns(&self) -> u64 {
        lemnos_linux_sys::time::monotonic_now()
            .map(|t| u64::try_from(t.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    fn wait_until(&self, ns: u64) {
        while self.now_ns() < ns {
            std::hint::spin_loop();
        }
    }

    fn before_frame(&self) {}
}

/// What the latch is run with.
pub struct Settings<'a> {
    /// The sensor's board id, or the board's one sensor.
    pub sensor: Option<String>,
    /// Posts per offset (and per check).
    pub trials: u32,
    /// The offsets to post at, in µs after a frame's start.
    pub offsets_us: &'a [u64],
    /// How long a frame read waits for the next frame to arrive (wall time).
    pub frame_wait: Duration,
}

/// What one offset's posts did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub offset_us: u64,
    pub trials: u32,
    /// Landed on the predicted frame.
    pub predicted: u32,
    /// Landed earlier than predicted.
    pub early: u32,
    /// Landed later than predicted.
    pub late: u32,
    /// Never seen (no frame past halfway in the frames read after the post).
    pub lost: u32,
    /// The post call's median and worst wall time, ns (zero in the simulator).
    pub call_median_ns: u64,
    pub call_max_ns: u64,
}

/// The whole latch run.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub frame_length: u32,
    pub period_ns: u64,
    pub short: u32,
    pub middle: u32,
    pub long: u32,
    pub brightness_short: f64,
    pub brightness_middle: f64,
    pub brightness_long: f64,
    pub offsets: Vec<Counts>,
    /// The largest offset at which every post landed on the predicted frame.
    pub deadline_us: Option<u64>,
    /// Two posts in one frame: the long one showed, and the middle one showed, of `trials`.
    pub second_long: u32,
    pub first_middle: u32,
    /// Whether the verdict passed: nothing early or lost, and the two-post check as predicted.
    pub passed: bool,
}

/// Mean of the high bytes of a packed RAW10 frame, every 61st sample.
fn brightness(bytes: &[u8]) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for (i, &b) in bytes.iter().enumerate().step_by(61) {
        if i % 5 != 4 {
            sum += u64::from(b);
            n += 1;
        }
    }
    sum as f64 / n.max(1) as f64
}

struct Seen {
    sequence: u64,
    start_ns: u64,
    level: f64,
}

/// The next frame, after `timing` makes it if it is the simulator's.
fn next(timing: &dyn Timing, stream: &mut FrameStream, wait: Duration) -> Result<Seen, String> {
    timing.before_frame();
    let frame: HeldFrame = stream
        .next_frame(wait)
        .map_err(|e| format!("frames: {e:?}"))?
        .ok_or_else(|| format!("no frame for {} ms", wait.as_millis()))?;
    let seen = Seen {
        sequence: frame.sequence(),
        start_ns: frame.timestamp_ns(),
        level: brightness(stream.bytes(&frame)),
    };
    stream
        .release(frame)
        .map_err(|e| format!("release: {e:?}"))?;
    Ok(seen)
}

/// Runs `call` again while the sensor answers BUSY, for up to two seconds: a sensor that has just
/// been started powers up for 10 ms before it takes a write, and a post refuses until the receiver
/// has counted a frame start. Each retry lets the timing make a frame (the simulator's does).
fn while_busy<T>(
    timing: &dyn Timing,
    mut call: impl FnMut() -> Result<T, Error>,
) -> Result<T, Error> {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match call() {
            Err(Error::Call {
                status: lemnos_core::Status::BUSY,
                ..
            }) if Instant::now() < until => {
                timing.before_frame();
                std::thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

/// Runs the latch. Each line of its report goes to `out`.
///
/// # Errors
/// The camera cannot be assembled or started, a frame does not arrive, or the scene does not
/// change with exposure (so no landing can be told).
#[allow(clippy::too_many_lines)]
pub fn run(
    bus: &SocketBus,
    settings: &Settings<'_>,
    timing: &dyn Timing,
    out: &mut dyn FnMut(String),
) -> Result<Report, String> {
    let trials = settings.trials;
    let wait = settings.frame_wait;
    let mut camera: Camera<'_, SocketBus> = assemble_with(
        bus,
        &Options {
            sensor: settings.sensor.clone(),
            mode: 0,
        },
        false,
    )
    .map_err(|e: Error| format!("{e}"))?;
    // Beside a chain that already runs the mode, the frame length is the chain's: the latch does
    // not change the rate the chain is measured at. On its own it uses its usual one.
    let beside = camera.keep_selected().map_err(|e| format!("{e}"))?;
    let held = if beside {
        camera.frame_length_in_force().map_err(|e| format!("{e}"))?
    } else {
        None
    };
    let frame_length = held.unwrap_or_else(|| camera.mode().frame_length_min.max(1822));
    let max_exposure = (frame_length - 25) * 16;
    let (short, middle, long) = (max_exposure / 8, max_exposure / 3, max_exposure);
    let controls = |exposure| Controls {
        exposure: Some(exposure),
        frame_length: Some(frame_length),
        ..Controls::default()
    };
    // The scene: the sensor keeps a test pattern an earlier caller chose.
    while_busy(timing, || camera.set_test_pattern(0)).map_err(|e| format!("{e}"))?;
    while_busy(timing, || camera.set_controls(controls(short))).map_err(|e| format!("{e}"))?;
    let receiver = camera.receiver().clone();
    let name = camera.stream().to_string();
    let mut stream = bus
        .take_stream(&receiver, &name, false)
        .map_err(|e| format!("{e}"))?;
    camera.start().map_err(|e| format!("{e}"))?;
    let mut identity = 0u64;
    let mut post = |camera: &mut Camera<'_, SocketBus>, exposure| {
        identity += 1;
        while_busy(timing, || {
            camera.post_values(FrameValues {
                identity,
                controls: controls(exposure),
            })
        })
    };
    // Settle, and the three levels.
    let mut level = |stream: &mut FrameStream, camera: &mut Camera<'_, SocketBus>, exposure| {
        post(camera, exposure).map_err(|e| format!("{e}"))?;
        let mut last = 0.0;
        for _ in 0..8 {
            last = next(timing, stream, wait)?.level;
        }
        Ok::<f64, String>(last)
    };
    let l_long = level(&mut stream, &mut camera, long)?;
    let l_middle = level(&mut stream, &mut camera, middle)?;
    let l_short = level(&mut stream, &mut camera, short)?;
    let period = {
        let a = next(timing, &mut stream, wait)?;
        let b = next(timing, &mut stream, wait)?;
        b.start_ns.saturating_sub(a.start_ns)
    };
    out(format!(
        "frame length {frame_length}, period {} us; exposures short {short} middle {middle} long {long}: brightness {l_short:.1}, {l_middle:.1}, {l_long:.1}",
        period / 1000
    ));
    // A landing is told by brightness: with no light on the sensor the exposures look alike, and
    // every count below would be noise.
    if l_long - l_short < 8.0 {
        let _ = if beside {
            camera.stop_receiver()
        } else {
            camera.stop()
        };
        return Err(format!(
            "the scene does not change with exposure (brightness {l_short:.1} short, {l_long:.1} long): light it and run again"
        ));
    }
    let halfway = (l_short + l_long) / 2.0;
    let mut report = Report {
        frame_length,
        period_ns: period,
        short,
        middle,
        long,
        brightness_short: l_short,
        brightness_middle: l_middle,
        brightness_long: l_long,
        offsets: Vec::new(),
        deadline_us: None,
        second_long: 0,
        first_middle: 0,
        passed: false,
    };
    let mut all_ok = true;
    for &offset_us in settings.offsets_us {
        let mut counts = Counts {
            offset_us,
            trials,
            ..Counts::default()
        };
        let mut wire = Vec::new();
        for _ in 0..trials {
            let f = next(timing, &mut stream, wait)?;
            // The frame start the offset counts from is this frame's; the post goes in that long
            // after it, which may already be into the next frame.
            let due = f.start_ns + offset_us * 1000;
            timing.wait_until(due);
            let posted = timing.now_ns();
            post(&mut camera, long).map_err(|e| format!("{e}"))?;
            let done = timing.now_ns();
            wire.push(done - posted);
            // Frame sequence numbers count frame starts: the frame the call completed in.
            let completed_in = f.sequence + (done - f.start_ns) / period.max(1);
            // The launch is the call's last write, somewhere between `posted` and `done`: a call
            // that straddles a frame start may have launched in either frame, and either landing
            // is the predicted one.
            let started_in = f.sequence + (posted - f.start_ns) / period.max(1);
            let mut landed = None;
            for _ in 0..8 {
                let g = next(timing, &mut stream, wait)?;
                if g.level > halfway && landed.is_none() {
                    landed = Some(g.sequence);
                }
            }
            match landed {
                Some(s) if (started_in + 2..=completed_in + 2).contains(&s) => {
                    counts.predicted += 1;
                }
                Some(s) if s < started_in + 2 => counts.early += 1,
                Some(_) => counts.late += 1,
                None => counts.lost += 1,
            }
            post(&mut camera, short).map_err(|e| format!("{e}"))?;
            for _ in 0..5 {
                next(timing, &mut stream, wait)?;
            }
        }
        wire.sort_unstable();
        counts.call_median_ns = wire.get(wire.len() / 2).copied().unwrap_or(0);
        counts.call_max_ns = wire.last().copied().unwrap_or(0);
        out(format!(
            "post at {:.1} ms: {trials} posts, {} on the predicted frame (completed in + 2), {} earlier, {} later, {} never; call median {} us, max {} us",
            offset_us as f64 / 1000.0,
            counts.predicted,
            counts.early,
            counts.late,
            counts.lost,
            counts.call_median_ns / 1000,
            counts.call_max_ns / 1000
        ));
        all_ok &= counts.early == 0 && counts.lost == 0;
        if counts.predicted == trials {
            report.deadline_us = Some(offset_us);
        }
        report.offsets.push(counts);
    }
    out(format!(
        "largest offset with every post on the predicted frame: {:?} us",
        report.deadline_us
    ));

    // Two posts in one frame: middle at 4 ms, long at 12 ms.
    for _ in 0..trials {
        let f = next(timing, &mut stream, wait)?;
        for (at, exposure) in [(4000u64, middle), (12000, long)] {
            timing.wait_until(f.start_ns + at * 1000);
            post(&mut camera, exposure).map_err(|e| format!("{e}"))?;
        }
        let (mut saw_middle, mut saw_long) = (false, false);
        for _ in 0..6 {
            let g = next(timing, &mut stream, wait)?;
            let to_long = (g.level - l_long).abs();
            let to_middle = (g.level - l_middle).abs();
            let to_short = (g.level - l_short).abs();
            saw_long |= to_long < to_middle && to_long < to_short;
            saw_middle |= to_middle < to_long && to_middle < to_short;
        }
        report.second_long += u32::from(saw_long);
        report.first_middle += u32::from(saw_middle);
        post(&mut camera, short).map_err(|e| format!("{e}"))?;
        for _ in 0..5 {
            next(timing, &mut stream, wait)?;
        }
    }
    out(format!(
        "two posts in one frame (middle at 4 ms, long at 12 ms), {trials} times: the long one showed {} times, the middle one {} times",
        report.second_long, report.first_middle
    ));
    let counters = camera.counters().map_err(|e| format!("{e}"))?;
    out(format!("receiver counters: {counters:?}"));
    // Beside a chain the sensor is the chain's: only the receiver this tool started is stopped.
    if beside {
        camera.stop_receiver()
    } else {
        camera.stop()
    }
    .map_err(|e| format!("{e}"))?;
    report.passed = all_ok && report.second_long == trials && report.first_middle == 0;
    Ok(report)
}
