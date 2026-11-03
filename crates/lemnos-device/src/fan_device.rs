//! The fan controller as a device: [`FanDevice`] is a [`Composite`] with
//! channels (what it computed) and controls (its tuning). The host builds its
//! [`DeviceInfo`] from the board (the input roles, their units), so the layout
//! is fixed here and the host lists it in this order:
//!
//! - channels: `demand`, `duty`, `source` (the input whose curve asked for
//!   the most, or none), then `input.<role>` for each input;
//! - controls: `floor`, `hysteresis`, `slew`, `enabled`, then for each input in
//!   order its curve's points as `value` and `duty` pairs.
//!
//! The control law is [`FanController`]; this module adds the device's
//! inputs, curves and outputs. Nothing here allocates.

use crate::composite::{Composite, InputSpec, OutputSpec, Outputs, Sample};
use crate::fan_control::{
    CurveError, FanController, Health, Input, MAX_INPUTS, MAX_POINTS, Point, Settings, check_curve,
};
use crate::{
    Control, Device, DeviceError, DeviceInfo, NO_VALUE, Sensor, check_buffer, check_control,
};
use lemnos_hal::ErrorKind;

/// How often the host steps the controller (ms).
pub const PERIOD_MS: u32 = 1_000;

/// Channel index of the demand (the curves' result, per-mille).
pub const CH_DEMAND: usize = 0;
/// Channel index of the commanded duty (per-mille).
pub const CH_DUTY: usize = 1;
/// Channel index of the source: the input whose curve asked for the most.
pub const CH_SOURCE: usize = 2;
/// Channel index of input 0's value; input `i` is at `CH_INPUT + i`.
pub const CH_INPUT: usize = 3;

/// Control index of the floor (per-mille).
pub const CTL_FLOOR: usize = 0;
/// Control index of the hysteresis (in the inputs' unit).
pub const CTL_HYSTERESIS: usize = 1;
/// Control index of the slew limit (per-mille per second).
pub const CTL_SLEW: usize = 2;
/// Control index of the enable flag (0 hands the fan back and stops).
pub const CTL_ENABLED: usize = 3;
/// Control index of input 0's first point's value; the points follow as
/// `value`, `duty` pairs, input after input.
pub const CTL_CURVE: usize = 4;

static OUTPUTS: [OutputSpec; 1] = [OutputSpec { per_mille: true }];

/// Where a control index points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Floor,
    Hysteresis,
    Slew,
    Enabled,
    Value(usize, usize),
    Duty(usize, usize),
}

/// A fan controller device: its inputs, curves and tuning, and the control law.
#[derive(Debug, Clone)]
pub struct FanDevice {
    info: &'static DeviceInfo,
    controller: FanController,
    enabled: bool,
    count: usize,
    inputs: [InputSpec; MAX_INPUTS],
    curves: [[Point; MAX_POINTS]; MAX_INPUTS],
    lens: [usize; MAX_INPUTS],
    /// The last step's result and the input values it used (`NO_VALUE` when
    /// an input was absent).
    health: Health,
    demand: i32,
    duty: i32,
    source: Option<usize>,
    values: [i32; MAX_INPUTS],
}

impl FanDevice {
    /// A controller with `settings`, one curve per input (`inputs[i]` is
    /// input `i`'s spec and curve). Fails on a count outside
    /// `1..=MAX_INPUTS` or a curve [`check_curve`] refuses.
    pub fn new(
        info: &'static DeviceInfo,
        settings: Settings,
        inputs: &[(InputSpec, &[Point])],
    ) -> Result<Self, CurveError> {
        if inputs.is_empty() || inputs.len() > MAX_INPUTS {
            return Err(CurveError::Length);
        }
        let mut device = Self {
            info,
            controller: FanController::new(settings),
            enabled: true,
            count: inputs.len(),
            inputs: [InputSpec {
                required: false,
                max_age_ms: 0,
            }; MAX_INPUTS],
            curves: [[Point { value: 0, duty: 0 }; MAX_POINTS]; MAX_INPUTS],
            lens: [0; MAX_INPUTS],
            health: Health::Running,
            demand: 0,
            duty: 0,
            source: None,
            values: [NO_VALUE; MAX_INPUTS],
        };
        for (i, (spec, points)) in inputs.iter().enumerate() {
            device.set_curve(i, points)?;
            device.inputs[i] = *spec;
        }
        Ok(device)
    }

    /// Whether the controller drives the fan.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The tuning in force.
    pub fn settings(&self) -> Settings {
        self.controller.settings()
    }

    /// Input `input`'s curve.
    pub fn curve(&self, input: usize) -> &[Point] {
        &self.curves[input][..self.lens[input]]
    }

    /// Replaces input `input`'s curve. It keeps its point count (the board
    /// fixes it) and must pass [`check_curve`].
    pub fn set_curve(&mut self, input: usize, points: &[Point]) -> Result<(), CurveError> {
        if input >= self.count || (self.lens[input] != 0 && points.len() != self.lens[input]) {
            return Err(CurveError::Length);
        }
        check_curve(points)?;
        self.curves[input][..points.len()].copy_from_slice(points);
        self.lens[input] = points.len();
        Ok(())
    }

    /// Whether the last step had a required input missing or stale.
    pub fn health(&self) -> Health {
        self.health
    }

    fn target(&self, index: usize) -> Option<Target> {
        match index {
            CTL_FLOOR => Some(Target::Floor),
            CTL_HYSTERESIS => Some(Target::Hysteresis),
            CTL_SLEW => Some(Target::Slew),
            CTL_ENABLED => Some(Target::Enabled),
            _ => {
                let mut k = index.checked_sub(CTL_CURVE)?;
                for input in 0..self.count {
                    let n = self.lens[input] * 2;
                    if k < n {
                        let point = k / 2;
                        return Some(if k % 2 == 0 {
                            Target::Value(input, point)
                        } else {
                            Target::Duty(input, point)
                        });
                    }
                    k -= n;
                }
                None
            }
        }
    }
}

impl Device for FanDevice {
    type Error = ErrorKind;

    fn info(&self) -> &'static DeviceInfo {
        self.info
    }
}

impl Sensor for FanDevice {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<ErrorKind>> {
        check_buffer(self.info, out)?;
        out[CH_DEMAND] = self.demand;
        out[CH_DUTY] = self.duty;
        out[CH_SOURCE] = self.source.map_or(NO_VALUE, |s| s as i32);
        out[CH_INPUT..CH_INPUT + self.count].copy_from_slice(&self.values[..self.count]);
        Ok(())
    }
}

impl Control for FanDevice {
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<ErrorKind>> {
        check_control(self.info, index, value)?;
        match self.target(index).ok_or(DeviceError::Unsupported)? {
            Target::Floor | Target::Hysteresis | Target::Slew => {
                let mut settings = self.controller.settings();
                match index {
                    CTL_FLOOR => settings.floor = value,
                    CTL_HYSTERESIS => settings.hysteresis = value,
                    _ => settings.slew_per_s = value,
                }
                self.controller.set_settings(settings);
            }
            Target::Enabled => self.enabled = value != 0,
            Target::Value(input, point) => {
                // Moving one point must keep the curve ascending.
                let mut points = self.curves[input];
                points[point].value = value;
                check_curve(&points[..self.lens[input]]).map_err(|_| DeviceError::OutOfRange)?;
                self.curves[input] = points;
            }
            Target::Duty(input, point) => self.curves[input][point].duty = value,
        }
        Ok(value)
    }

    fn get(&mut self, index: usize) -> Result<i32, DeviceError<ErrorKind>> {
        Ok(match self.target(index).ok_or(DeviceError::Unsupported)? {
            Target::Floor => self.controller.settings().floor,
            Target::Hysteresis => self.controller.settings().hysteresis,
            Target::Slew => self.controller.settings().slew_per_s,
            Target::Enabled => i32::from(self.enabled),
            Target::Value(input, point) => self.curves[input][point].value,
            Target::Duty(input, point) => self.curves[input][point].duty,
        })
    }
}

impl Composite for FanDevice {
    fn inputs(&self) -> &[InputSpec] {
        &self.inputs[..self.count]
    }

    fn outputs(&self) -> &[OutputSpec] {
        &OUTPUTS
    }

    fn period_ms(&self) -> u32 {
        PERIOD_MS
    }

    fn step(
        &mut self,
        now_ms: u64,
        samples: &[Sample],
        out: &mut dyn Outputs,
    ) -> Result<(), DeviceError<ErrorKind>> {
        if !self.enabled {
            return Ok(());
        }
        let mut inputs = [Input {
            points: &[],
            required: false,
            max_age_ms: 0,
        }; MAX_INPUTS];
        for (i, slot) in inputs.iter_mut().enumerate().take(self.count) {
            *slot = Input {
                points: &self.curves[i][..self.lens[i]],
                required: self.inputs[i].required,
                max_age_ms: self.inputs[i].max_age_ms,
            };
        }
        let count = self.count;
        let decision = self.controller.step(now_ms, &inputs[..count], samples);
        for (i, value) in self.values.iter_mut().enumerate().take(count) {
            *value = match samples.get(i) {
                Some(s) if s.present => s.value,
                _ => NO_VALUE,
            };
        }
        self.health = decision.health;
        self.demand = decision.demand;
        self.duty = decision.duty;
        self.source = decision.source;
        out.set(0, decision.duty).map_err(DeviceError::Driver)
    }

    fn release(&mut self, out: &mut dyn Outputs) {
        out.release(0);
        // The next start begins from a stopped fan.
        self.controller = FanController::new(self.controller.settings());
        self.duty = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, ControlInfo, DeviceClass, Quantity};

    static CHANNELS: [Channel; 4] = [
        Channel::new("demand", Quantity::Ratio, -3),
        Channel::new("duty", Quantity::Ratio, -3),
        Channel::new("source", Quantity::Mode, 0),
        Channel::new("input.cpu", Quantity::Temperature, -3),
    ];
    static CONTROLS: [ControlInfo; 8] = [
        ControlInfo::new("floor", Quantity::Ratio, -3, 0, 1000),
        ControlInfo::new("hysteresis", Quantity::Temperature, -3, 0, i32::MAX),
        ControlInfo::new("slew", Quantity::Ratio, -3, 0, 1000),
        ControlInfo::new("enabled", Quantity::Level, 0, 0, 1),
        ControlInfo::new(
            "curve.cpu.0.value",
            Quantity::Temperature,
            -3,
            i32::MIN,
            i32::MAX,
        ),
        ControlInfo::new("curve.cpu.0.duty", Quantity::Ratio, -3, 0, 1000),
        ControlInfo::new(
            "curve.cpu.1.value",
            Quantity::Temperature,
            -3,
            i32::MIN,
            i32::MAX,
        ),
        ControlInfo::new("curve.cpu.1.duty", Quantity::Ratio, -3, 0, 1000),
    ];
    static INFO: DeviceInfo =
        DeviceInfo::new(DeviceClass::Fan, "fan-controller", &CHANNELS, &CONTROLS);

    const CPU: [Point; 2] = [
        Point {
            value: 50_000,
            duty: 700,
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

    #[derive(Default)]
    struct Recorder {
        duty: Option<i32>,
        fail: bool,
        released: bool,
    }

    impl Outputs for Recorder {
        fn set(&mut self, _output: usize, value: i32) -> Result<(), ErrorKind> {
            if self.fail {
                return Err(ErrorKind::Busy);
            }
            self.duty = Some(value);
            Ok(())
        }
        fn release(&mut self, _output: usize) {
            self.released = true;
        }
    }

    fn device() -> FanDevice {
        let spec = InputSpec {
            required: true,
            max_age_ms: 3_000,
        };
        FanDevice::new(&INFO, SETTINGS, &[(spec, &CPU)]).unwrap()
    }

    #[test]
    fn a_step_writes_the_duty_and_reads_back() {
        let mut d = device();
        let mut out = Recorder::default();
        let s = [Sample {
            value: 60_000,
            age_ms: 0,
            present: true,
        }];
        d.step(0, &s, &mut out).unwrap();
        assert_eq!(out.duty, Some(820));
        let mut ch = [0; 4];
        d.read(&mut ch).unwrap();
        assert_eq!(ch, [820, 820, 0, 60_000]);
        assert_eq!(d.health(), Health::Running);
    }

    #[test]
    fn a_failed_write_is_an_error() {
        let mut d = device();
        let mut out = Recorder {
            fail: true,
            ..Recorder::default()
        };
        let s = [Sample {
            value: 60_000,
            age_ms: 0,
            present: true,
        }];
        assert_eq!(
            d.step(0, &s, &mut out),
            Err(DeviceError::Driver(ErrorKind::Busy))
        );
    }

    #[test]
    fn controls_map_to_floor_hysteresis_and_points() {
        let mut d = device();
        assert_eq!(d.set(CTL_FLOOR, 800), Ok(800));
        assert_eq!(d.controller.settings().floor, 800);
        assert_eq!(d.get(CTL_CURVE + 3), Ok(1000));
        assert_eq!(d.set(CTL_CURVE + 3, 900), Ok(900));
        assert_eq!(d.curve(0)[1].duty, 900);
        assert_eq!(d.set(CTL_ENABLED, 0), Ok(0));
        assert!(!d.enabled());
    }

    #[test]
    fn a_point_must_keep_the_curve_ascending() {
        let mut d = device();
        assert_eq!(d.set(CTL_CURVE, 80_000), Err(DeviceError::OutOfRange));
        assert_eq!(d.set(CTL_CURVE + 2, 80_000), Ok(80_000));
    }

    #[test]
    fn a_disabled_device_does_not_drive_the_fan() {
        let mut d = device();
        d.set(CTL_ENABLED, 0).unwrap();
        let mut out = Recorder::default();
        d.step(0, &[Sample::NONE], &mut out).unwrap();
        assert_eq!(out.duty, None);
        d.release(&mut out);
        assert!(out.released);
    }

    #[test]
    fn set_curve_keeps_the_count_and_checks_the_order() {
        let mut d = device();
        assert_eq!(d.set_curve(0, &CPU[..1]), Err(CurveError::Length));
        let bad = [
            Point {
                value: 80_000,
                duty: 700,
            },
            Point {
                value: 70_000,
                duty: 900,
            },
        ];
        assert_eq!(d.set_curve(0, &bad), Err(CurveError::Order));
    }
}
