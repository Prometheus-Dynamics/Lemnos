//! The fan controller's control law: `no_std`, no allocation, no I/O.
//!
//! A composite device (`docs/composite-devices.md`) reads inputs that other
//! devices report (temperatures, power) and drives one fan. [`FanController`]
//! is the pure part: given the latest sample of each input, it returns the
//! duty to command. The host (`lemnosd`) owns the reads, the writes, the
//! failsafe and the schedule.
//!
//! Duties are per-mille of full scale (0..=1000). Input values are integers in
//! the input's own unit (millidegrees Celsius for a thermal zone).

/// Most inputs one controller takes.
pub const MAX_INPUTS: usize = 4;
/// Most curve points per input.
pub const MAX_POINTS: usize = 8;
/// Full scale, in per-mille.
pub const FULL: i32 = 1000;

/// One point of a curve: at `value` the input asks for `duty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub value: i32,
    pub duty: i32,
}

/// Why a curve was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveError {
    /// No points, or more than [`MAX_POINTS`].
    Length,
    /// Values are not strictly ascending.
    Order,
    /// A duty is outside 0..=1000.
    Duty,
}

/// Checks that `points` is a usable curve.
pub fn check_curve(points: &[Point]) -> Result<(), CurveError> {
    if points.is_empty() || points.len() > MAX_POINTS {
        return Err(CurveError::Length);
    }
    for (i, p) in points.iter().enumerate() {
        if !(0..=FULL).contains(&p.duty) {
            return Err(CurveError::Duty);
        }
        if i > 0 && points[i - 1].value >= p.value {
            return Err(CurveError::Order);
        }
    }
    Ok(())
}

/// One input: its curve, and how stale its sample may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input<'a> {
    pub points: &'a [Point],
    /// A required input that is missing or older than `max_age_ms` forces full
    /// cooling and a degraded controller; an optional one is left out.
    pub required: bool,
    pub max_age_ms: u32,
}

/// The latest sample of one input: `present` is false when it has never been
/// read or its read failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub value: i32,
    pub age_ms: u32,
    pub present: bool,
}

impl Sample {
    /// No sample.
    pub const NONE: Self = Self {
        value: 0,
        age_ms: 0,
        present: false,
    };
}

/// The tuning: floor, hysteresis, slew and spin-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// The lowest duty while the controller runs (per-mille). The fan never
    /// stops under a floor above zero.
    pub floor: i32,
    /// How far an input must fall below its peak before its curve lets go, in
    /// the input's unit.
    pub hysteresis: i32,
    /// The most the duty falls per second (per-mille per second). Rising is not
    /// limited.
    pub slew_per_s: i32,
    /// From a stopped fan, full duty for this long before the target (ms; 0
    /// disables). Only applies when the floor is zero.
    pub kick_ms: u32,
}

/// Whether the controller is trusting its inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Running,
    /// A required input is missing or stale: the duty is full.
    Degraded,
}

/// One step's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The duty to command (per-mille).
    pub duty: i32,
    pub health: Health,
    /// The input whose curve asked for the most (`None`: no input contributed).
    pub source: Option<usize>,
}

/// The controller's state between steps.
#[derive(Debug, Clone)]
pub struct FanController {
    settings: Settings,
    /// Each input's peak, the value its curve is evaluated at.
    peak: [Option<i32>; MAX_INPUTS],
    duty: i32,
    last_ms: Option<u64>,
    kick_until_ms: Option<u64>,
}

impl FanController {
    /// A controller with `settings`. The inputs come with each step; the host
    /// checks its curves with [`check_curve`].
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            peak: [None; MAX_INPUTS],
            duty: 0,
            last_ms: None,
            kick_until_ms: None,
        }
    }

    /// The duty last commanded.
    pub fn duty(&self) -> i32 {
        self.duty
    }

    /// One step at `now_ms` (any monotonic clock, milliseconds). `inputs[i]`
    /// and `samples[i]` are input `i` and its latest sample; missing samples
    /// count as absent. At most [`MAX_INPUTS`] inputs are used.
    pub fn step(&mut self, now_ms: u64, inputs: &[Input<'_>], samples: &[Sample]) -> Decision {
        let dt_ms = self.last_ms.map_or(0, |last| now_ms.saturating_sub(last));
        self.last_ms = Some(now_ms);

        let mut demand = 0;
        let mut source = None;
        let mut stale = false;
        for (i, input) in inputs.iter().enumerate().take(MAX_INPUTS) {
            let sample = samples.get(i).copied().unwrap_or(Sample::NONE);
            if !sample.present || sample.age_ms > input.max_age_ms {
                stale |= input.required;
                continue;
            }
            let peak = self.hold(i, sample.value);
            let asked = curve(input.points, peak);
            if source.is_none() || asked > demand {
                demand = asked;
                source = Some(i);
            }
        }

        let (target, health) = if stale {
            (FULL, Health::Degraded)
        } else {
            (
                demand.max(self.settings.floor).clamp(0, FULL),
                Health::Running,
            )
        };
        let commanded = self.next_duty(now_ms, dt_ms, target);
        Decision {
            duty: commanded,
            health,
            source,
        }
    }

    /// The hysteresis: rising values raise the peak; a value that has fallen
    /// past the band resets it; anything in between keeps the peak.
    fn hold(&mut self, i: usize, value: i32) -> i32 {
        let hyst = self.settings.hysteresis.max(0);
        let next = match self.peak[i] {
            None => value,
            Some(peak) if value >= peak => value,
            Some(peak) if value < peak.saturating_sub(hyst) => value,
            Some(peak) => peak,
        };
        self.peak[i] = Some(next);
        next
    }

    /// Spin-up kick, then the slew limit on the way down.
    fn next_duty(&mut self, now_ms: u64, dt_ms: u64, target: i32) -> i32 {
        if self.settings.floor == 0
            && self.duty == 0
            && target > 0
            && self.settings.kick_ms > 0
            && self.kick_until_ms.is_none()
        {
            self.kick_until_ms = Some(now_ms + u64::from(self.settings.kick_ms));
        }
        let duty = if self.kick_until_ms.is_some_and(|until| now_ms < until) {
            FULL
        } else {
            self.kick_until_ms = None;
            if target >= self.duty {
                target
            } else {
                let allowed = i64::from(self.settings.slew_per_s.max(0)) * dt_ms as i64 / 1000;
                let lowest = i64::from(self.duty) - allowed;
                i64::from(target).max(lowest) as i32
            }
        };
        self.duty = duty;
        duty
    }
}

/// The curve at `value`: piecewise linear between points, clamped at both ends.
pub fn curve(points: &[Point], value: i32) -> i32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    const CPU: [Point; 3] = [
        Point {
            value: 50_000,
            duty: 700,
        },
        Point {
            value: 65_000,
            duty: 850,
        },
        Point {
            value: 75_000,
            duty: 1000,
        },
    ];

    const SETTINGS: Settings = Settings {
        floor: 700,
        hysteresis: 3_000,
        slew_per_s: 50,
        kick_ms: 0,
    };

    fn cpu_input() -> [Input<'static>; 1] {
        [Input {
            points: &CPU,
            required: true,
            max_age_ms: 3_000,
        }]
    }

    fn sample(value: i32) -> [Sample; 1] {
        [Sample {
            value,
            age_ms: 0,
            present: true,
        }]
    }

    #[test]
    fn curves_are_checked() {
        assert_eq!(check_curve(&CPU), Ok(()));
        assert_eq!(check_curve(&[]), Err(CurveError::Length));
        let unordered = [Point { value: 2, duty: 1 }, Point { value: 1, duty: 2 }];
        assert_eq!(check_curve(&unordered), Err(CurveError::Order));
        let bad = [Point {
            value: 1,
            duty: 1001,
        }];
        assert_eq!(check_curve(&bad), Err(CurveError::Duty));
    }

    #[test]
    fn the_curve_interpolates_and_clamps() {
        assert_eq!(curve(&CPU, 0), 700);
        assert_eq!(curve(&CPU, 57_500), 775);
        assert_eq!(curve(&CPU, 90_000), 1000);
    }

    #[test]
    fn the_floor_holds_at_low_demand() {
        let inputs = cpu_input();
        let mut c = FanController::new(SETTINGS);
        assert_eq!(c.step(0, &inputs, &sample(30_000)).duty, 700);
    }

    #[test]
    fn rising_is_immediate_and_falling_is_slew_limited() {
        let inputs = cpu_input();
        let mut c = FanController::new(SETTINGS);
        assert_eq!(c.step(0, &inputs, &sample(80_000)).duty, 1000);
        // 50 per-mille per second: 1 s later the duty has fallen by at most 50.
        let d = c.step(1_000, &inputs, &sample(30_000)).duty;
        assert_eq!(d, 950);
        assert_eq!(c.step(2_000, &inputs, &sample(30_000)).duty, 900);
    }

    #[test]
    fn hysteresis_keeps_the_peak_until_it_has_fallen_the_band() {
        let inputs = cpu_input();
        let mut c = FanController::new(SETTINGS);
        c.step(0, &inputs, &sample(70_000));
        // 2 degrees below the peak is inside the 3 degree band: still evaluated at 70.
        let d = c.step(10_000, &inputs, &sample(68_000));
        assert_eq!(d.duty, curve(&CPU, 70_000));
        // 4 degrees below: the peak resets to the new value.
        let d = c.step(20_000, &inputs, &sample(66_000));
        assert_eq!(d.duty, curve(&CPU, 66_000).max(c.duty()).max(700));
    }

    #[test]
    fn a_stale_required_input_forces_full_cooling() {
        let inputs = cpu_input();
        let mut c = FanController::new(SETTINGS);
        c.step(0, &inputs, &sample(40_000));
        let stale = [Sample {
            value: 40_000,
            age_ms: 5_000,
            present: true,
        }];
        let d = c.step(1_000, &inputs, &stale);
        assert_eq!(d.duty, 1000);
        assert_eq!(d.health, Health::Degraded);
        let missing = c.step(2_000, &inputs, &[Sample::NONE]);
        assert_eq!(missing.health, Health::Degraded);
    }

    #[test]
    fn an_optional_input_missing_does_not_degrade() {
        let inputs = [Input {
            points: &CPU,
            required: false,
            max_age_ms: 3_000,
        }];
        let mut c = FanController::new(SETTINGS);
        let d = c.step(0, &inputs, &[Sample::NONE]);
        assert_eq!(d.health, Health::Running);
        assert_eq!(d.duty, 700);
        assert_eq!(d.source, None);
    }

    #[test]
    fn the_hottest_input_wins_and_is_reported() {
        let board = [
            Point { value: 0, duty: 0 },
            Point {
                value: 10_000,
                duty: 1000,
            },
        ];
        let inputs = [
            Input {
                points: &CPU,
                required: true,
                max_age_ms: 3_000,
            },
            Input {
                points: &board,
                required: false,
                max_age_ms: 3_000,
            },
        ];
        let mut c = FanController::new(SETTINGS);
        let samples = [
            Sample {
                value: 50_000,
                age_ms: 0,
                present: true,
            },
            Sample {
                value: 9_000,
                age_ms: 0,
                present: true,
            },
        ];
        let d = c.step(0, &inputs, &samples);
        assert_eq!(d.source, Some(1));
        assert_eq!(d.duty, 900);
    }

    #[test]
    fn spin_up_kicks_a_stopped_fan_when_the_floor_is_zero() {
        let inputs = cpu_input();
        let settings = Settings {
            floor: 0,
            kick_ms: 500,
            ..SETTINGS
        };
        let mut c = FanController::new(settings);
        // Demand is 700 at 50 °C; the kick runs at full for 500 ms first.
        assert_eq!(c.step(0, &inputs, &sample(50_000)).duty, 1000);
        assert_eq!(c.step(400, &inputs, &sample(50_000)).duty, 1000);
        // After the kick the duty comes down at the slew rate: 200 ms at 50 per second.
        assert_eq!(c.step(600, &inputs, &sample(50_000)).duty, 990);
        assert_eq!(c.step(1_600, &inputs, &sample(50_000)).duty, 940);
        assert_eq!(c.step(6_600, &inputs, &sample(50_000)).duty, 700);
    }
}
