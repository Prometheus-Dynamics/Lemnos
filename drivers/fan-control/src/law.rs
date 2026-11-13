//! The control law: curves, hysteresis, floor, slew, spin-up, timed holds, retries and the
//! stall check. `no_std`, no allocation, no I/O, no clock. The driver calls it from `tick` with
//! the host's time and reports what it read and what it wrote.
//!
//! Ported from dev's `lemnos_device::fan_control`. Three things differ: time is nanoseconds from
//! `tick` (the law works in milliseconds internally, as dev does); a timed hold can override the
//! curves; and a failed fan write releases the fan and waits, doubling each time, capped at 8 s.

use lemnos_contract_fan_control::{MAX_POINTS, Point, Settings, State};
use lemnos_core::{Health, Result, Status};

/// Temperature inputs a controller can take: `t0` (required) and `t1` to `t3`.
pub const SLOTS: usize = 4;
/// Full scale, per-mille.
pub const FULL: i32 = 1000;
/// One control step per second.
pub const PERIOD_NS: u64 = 1_000_000_000;
/// A controller that has not completed a step for this many periods has stalled.
pub const STALL_PERIODS: u64 = 3;
/// The first wait after a failed fan write; each further failure doubles it.
const RETRY_FIRST_NS: u64 = 1_000_000_000;
/// The longest wait between attempts to take the fan again.
const RETRY_CAP_NS: u64 = 8_000_000_000;
/// Nanoseconds in a millisecond.
const NS_PER_MS: u64 = 1_000_000;

/// The health reason while an input is missing or stale.
pub const DEGRADED: &str = "a temperature input is missing or stale: the fan is at full duty";
/// The health reason after a failed fan write.
pub const WRITE_FAILED: &str = "the fan write failed: handed back to the kernel, retrying";

/// One input's curve: 1 to [`MAX_POINTS`] points, values strictly ascending, duties 0 to 1000.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Curve {
    points: [Point; MAX_POINTS],
    len: usize,
}

impl Curve {
    /// Checks `points` and makes a curve of them.
    pub fn new(points: &[Point]) -> Result<Self> {
        if points.is_empty() || points.len() > MAX_POINTS {
            return Err(Status::INVALID_ARGUMENT);
        }
        for (i, point) in points.iter().enumerate() {
            if !(0..=FULL).contains(&point.duty) {
                return Err(Status::INVALID_ARGUMENT);
            }
            if i > 0 && points[i - 1].value >= point.value {
                return Err(Status::INVALID_ARGUMENT);
            }
        }
        let mut buf = [Point::default(); MAX_POINTS];
        buf[..points.len()].copy_from_slice(points);
        Ok(Self {
            points: buf,
            len: points.len(),
        })
    }

    /// The points, in order.
    pub fn points(&self) -> &[Point] {
        &self.points[..self.len]
    }

    /// The duty at `value`: piecewise linear between points, clamped at both ends.
    pub fn eval(&self, value: i32) -> i32 {
        let points = self.points();
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return 0;
        };
        if value <= first.value {
            return first.duty;
        }
        if value >= last.value {
            return last.duty;
        }
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if value <= b.value {
                let span = i64::from(b.value) - i64::from(a.value);
                let along = i64::from(value) - i64::from(a.value);
                let rise = i64::from(b.duty) - i64::from(a.duty);
                return (i64::from(a.duty) + rise * along / span) as i32;
            }
        }
        last.duty
    }
}

/// Parses a curve written as `value:duty,value:duty,...`, for example
/// `30000:300,60000:600,75000:1000`.
pub fn parse_curve(text: &str) -> Result<Curve> {
    let mut buf = [Point::default(); MAX_POINTS];
    let mut count = 0;
    for pair in text.split(',') {
        let (value, duty) = pair.split_once(':').ok_or(Status::INVALID_ARGUMENT)?;
        let value = value
            .trim()
            .parse::<i32>()
            .map_err(|_| Status::INVALID_ARGUMENT)?;
        let duty = duty
            .trim()
            .parse::<i32>()
            .map_err(|_| Status::INVALID_ARGUMENT)?;
        let slot = buf.get_mut(count).ok_or(Status::INVALID_ARGUMENT)?;
        *slot = Point { value, duty };
        count += 1;
    }
    Curve::new(&buf[..count])
}

/// Checks the tuning: floor and slew are per-mille, hysteresis is not negative.
pub fn check_settings(settings: &Settings) -> Result<()> {
    if (0..=FULL).contains(&settings.floor)
        && settings.hysteresis >= 0
        && (0..=FULL).contains(&settings.slew_per_s)
    {
        Ok(())
    } else {
        Err(Status::INVALID_ARGUMENT)
    }
}

/// One temperature input's state.
#[derive(Debug, Clone, Copy)]
struct Input {
    /// `None`: no device is named for this input.
    curve: Option<Curve>,
    /// The last good reading, in millidegrees, and when it was taken (ms).
    last: Option<(i32, u64)>,
    /// The hysteresis peak.
    peak: Option<i32>,
}

/// A timed hold: the duty it asks for, and when it ends (ns).
#[derive(Debug, Clone, Copy)]
struct Hold {
    duty: i32,
    until_ns: u64,
}

/// The controller's state between steps.
#[derive(Debug)]
pub struct Control {
    settings: Settings,
    max_age_ms: u32,
    inputs: [Input; SLOTS],
    enabled: bool,
    /// The duty last commanded (0 while disabled or handed back).
    duty: i32,
    demand: i32,
    source: Option<u8>,
    /// A required input is missing or stale at the last step.
    degraded: bool,
    hold: Option<Hold>,
    /// A hold a method asked for; it starts at the next tick.
    pending_hold: Option<(i32, u32)>,
    last_ms: Option<u64>,
    kick_until_ms: Option<u64>,
    /// When the next step is due (ns).
    next_step_ns: u64,
    /// A method changed something: step at the next tick, due or not.
    urgent: bool,
    /// When the controller last completed a step (ns), for the stall check.
    stepped_ns: Option<u64>,
    /// After a failed write: when the controller may take the fan again (ns).
    retry_at_ns: Option<u64>,
    attempts: u32,
    /// The last write failed.
    fault: bool,
    /// When the fan is due to be handed back (ns). Set when the controller is disabled, and
    /// kept until the hand-back works.
    release_at_ns: Option<u64>,
}

fn hold_peak(peak: &mut Option<i32>, value: i32, hysteresis: i32) -> i32 {
    let next = match *peak {
        None => value,
        Some(p) if value >= p => value,
        Some(p) if value < p.saturating_sub(hysteresis) => value,
        Some(p) => p,
    };
    *peak = Some(next);
    next
}

impl Control {
    /// A controller with `settings`. `curves[i]` is `Some` for each temperature input the
    /// driver was given. The first step is due at once.
    pub fn new(
        settings: Settings,
        max_age_ms: u32,
        curves: [Option<Curve>; SLOTS],
    ) -> Result<Self> {
        check_settings(&settings)?;
        if max_age_ms == 0 {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            settings,
            max_age_ms,
            inputs: curves.map(|curve| Input {
                curve,
                last: None,
                peak: None,
            }),
            enabled: true,
            duty: 0,
            demand: 0,
            source: None,
            degraded: false,
            hold: None,
            pending_hold: None,
            last_ms: None,
            kick_until_ms: None,
            next_step_ns: 0,
            urgent: false,
            stepped_ns: None,
            retry_at_ns: None,
            attempts: 0,
            fault: false,
            release_at_ns: None,
        })
    }

    /// Whether temperature input `slot` is wired in.
    pub fn present(&self, slot: usize) -> bool {
        self.inputs.get(slot).is_some_and(|i| i.curve.is_some())
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        check_settings(&settings)?;
        self.settings = settings;
        self.urgent = true;
        Ok(())
    }

    /// The curve of input `slot`, or `NOT_FOUND` if no device is wired to it.
    pub fn curve(&self, slot: u8) -> Result<&Curve> {
        self.inputs
            .get(usize::from(slot))
            .and_then(|input| input.curve.as_ref())
            .ok_or(Status::NOT_FOUND)
    }

    pub fn set_curve(&mut self, slot: u8, points: &[Point]) -> Result<()> {
        let curve = Curve::new(points)?;
        let input = self
            .inputs
            .get_mut(usize::from(slot))
            .filter(|input| input.curve.is_some())
            .ok_or(Status::NOT_FOUND)?;
        input.curve = Some(curve);
        input.peak = None;
        self.urgent = true;
        Ok(())
    }

    pub fn state(&self) -> State {
        State {
            demand: self.demand,
            duty: self.duty,
            degraded: self.degraded,
            source: self.source,
            held: self.hold.is_some(),
        }
    }

    /// Asks for `duty` for `ms`. It starts at the next tick. Refused while disabled.
    pub fn hold(&mut self, duty: i32, ms: u32) -> Result<()> {
        if !self.enabled {
            return Err(Status::NO_DEVICE);
        }
        if !(0..=FULL).contains(&duty) || ms == 0 {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.pending_hold = Some((duty, ms));
        self.urgent = true;
        Ok(())
    }

    pub fn release_hold(&mut self) {
        self.pending_hold = None;
        self.hold = None;
        self.urgent = true;
    }

    /// Turns the controller on or off. Off stops the steps and releases the fan; on starts
    /// the steps again from a stopped fan.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.hold = None;
        self.pending_hold = None;
        self.kick_until_ms = None;
        self.last_ms = None;
        self.duty = 0;
        self.demand = 0;
        self.source = None;
        self.degraded = false;
        self.fault = false;
        self.attempts = 0;
        self.retry_at_ns = None;
        if enabled {
            self.release_at_ns = None;
            self.next_step_ns = 0;
            self.urgent = true;
        } else {
            self.release_at_ns = Some(0);
            self.urgent = false;
        }
    }

    /// Starts a hold a method asked for, and ends one that has run out. Call at each tick
    /// before the steps.
    pub fn begin(&mut self, now_ns: u64) {
        if let Some((duty, ms)) = self.pending_hold.take() {
            let until_ns = now_ns.saturating_add(u64::from(ms) * NS_PER_MS);
            self.hold = Some(Hold { duty, until_ns });
            self.urgent = true;
        }
        if self.hold.is_some_and(|hold| now_ns >= hold.until_ns) {
            self.hold = None;
            self.urgent = true;
        }
    }

    /// Whether the fan is due to be handed back at `now_ns`.
    pub fn release_due(&self, now_ns: u64) -> bool {
        self.release_at_ns.is_some_and(|at| now_ns >= at)
    }

    /// The hand-back worked.
    pub fn released(&mut self) {
        self.release_at_ns = None;
    }

    /// The hand-back failed. It is tried again in one period.
    pub fn release_failed(&mut self, now_ns: u64) {
        self.release_at_ns = Some(now_ns.saturating_add(PERIOD_NS));
    }

    /// Whether a step is due at `now_ns`. A tick can come early, so this checks.
    pub fn step_due(&self, now_ns: u64) -> bool {
        self.enabled && (self.urgent || now_ns >= self.next_step_ns)
    }

    /// When the driver next needs a tick, if it needs one.
    pub fn next_due(&self) -> Option<u64> {
        let step = self.enabled.then_some(self.next_step_ns);
        let hold = self.hold.map(|hold| hold.until_ns);
        [step, hold, self.release_at_ns].into_iter().flatten().min()
    }

    /// Records a reading of input `slot`. `None` is a failed read: the last good reading stays,
    /// and ages until it is too old to use.
    pub fn observe(&mut self, slot: usize, reading: Option<i32>, now_ns: u64) {
        let Some(input) = self.inputs.get_mut(slot) else {
            return;
        };
        if input.curve.is_none() {
            return;
        }
        if let Some(value) = reading {
            input.last = Some((value, now_ns / NS_PER_MS));
        }
    }

    /// One control step. Returns the duty to write, or `None` while the fan is handed back and
    /// the retry wait has not run out. Completing a step counts toward the stall check either way.
    pub fn step(&mut self, now_ns: u64) -> Option<i32> {
        self.stepped_ns = Some(now_ns);
        self.next_step_ns = now_ns.saturating_add(PERIOD_NS);
        self.urgent = false;
        if let Some(at) = self.retry_at_ns {
            if now_ns < at {
                return None;
            }
            self.retry_at_ns = None;
        }

        let now_ms = now_ns / NS_PER_MS;
        let dt_ms = self.last_ms.map_or(0, |last| now_ms.saturating_sub(last));
        self.last_ms = Some(now_ms);

        let hyst = self.settings.hysteresis.max(0);
        let max_age = u64::from(self.max_age_ms);
        let mut demand = 0;
        let mut source = None;
        let mut stale = false;
        for (i, input) in self.inputs.iter_mut().enumerate() {
            let Some(curve) = input.curve else {
                continue;
            };
            let fresh = input
                .last
                .filter(|&(_, at)| now_ms.saturating_sub(at) <= max_age);
            let Some((value, _)) = fresh else {
                stale = true;
                continue;
            };
            let peak = hold_peak(&mut input.peak, value, hyst);
            let asked = curve.eval(peak);
            if source.is_none() || asked > demand {
                demand = asked;
                source = u8::try_from(i).ok();
            }
        }

        let floor = self.settings.floor;
        let (target, immediate) = if stale {
            (FULL, false)
        } else if let Some(hold) = self.hold {
            (hold.duty.max(floor).clamp(0, FULL), true)
        } else {
            (demand.max(floor).clamp(0, FULL), false)
        };
        self.demand = demand;
        self.source = source;
        self.degraded = stale;

        let duty = self.next_duty(now_ms, dt_ms, target, immediate);
        self.duty = duty;
        Some(duty)
    }

    /// Spin-up kick, then the slew limit on the way down. A hold's duty is taken at once.
    fn next_duty(&mut self, now_ms: u64, dt_ms: u64, target: i32, immediate: bool) -> i32 {
        let s = self.settings;
        if s.floor == 0
            && self.duty == 0
            && target > 0
            && s.kick_ms > 0
            && self.kick_until_ms.is_none()
        {
            self.kick_until_ms = Some(now_ms.saturating_add(u64::from(s.kick_ms)));
        }
        if self.kick_until_ms.is_some_and(|until| now_ms < until) {
            return FULL;
        }
        self.kick_until_ms = None;
        if immediate || target >= self.duty {
            return target;
        }
        let dt = i64::try_from(dt_ms).unwrap_or(i64::MAX);
        let allowed = i64::from(s.slew_per_s.max(0)).saturating_mul(dt) / 1000;
        let lowest = i64::from(self.duty) - allowed;
        i64::from(target).max(lowest) as i32
    }

    /// The fan write worked.
    pub fn wrote(&mut self) {
        self.fault = false;
        self.attempts = 0;
    }

    /// The fan write failed. The fan is handed back (the driver does that) and the next attempt
    /// waits 1 s, then 2, 4 and 8 s, and stays at 8 s.
    pub fn write_failed(&mut self, now_ns: u64) {
        self.attempts = self.attempts.saturating_add(1);
        let shift = (self.attempts - 1).min(3);
        let wait = (RETRY_FIRST_NS << shift).min(RETRY_CAP_NS);
        self.retry_at_ns = Some(now_ns.saturating_add(wait));
        self.fault = true;
        self.duty = 0;
        self.kick_until_ms = None;
    }

    /// Whether the controller is running and completing its steps: true while it is disabled
    /// (a disabled controller is not expected to step), and while it has stepped within
    /// [`STALL_PERIODS`]. A degraded or retrying controller still steps, so it still counts.
    pub fn stepping(&self, now_ns: u64) -> bool {
        !self.enabled
            || self
                .stepped_ns
                .is_some_and(|at| now_ns < at.saturating_add(STALL_PERIODS * PERIOD_NS))
    }

    pub fn health(&self) -> Health {
        if !self.enabled {
            Health::Ok
        } else if self.fault {
            Health::Degraded(WRITE_FAILED)
        } else if self.degraded {
            Health::Degraded(DEGRADED)
        } else {
            Health::Ok
        }
    }
}
