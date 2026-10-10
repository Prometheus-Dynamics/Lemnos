//! Automatic and forced calibration of an accelerometer, a gyroscope and a
//! magnetometer.
//!
//! [`ImuCalibrator`] runs on every IMU sample: a gyro zero-rate estimate from
//! still windows, and an accelerometer ellipsoid fit from quasi-static samples.
//! [`MagCalibrator`] runs an ellipsoid fit on every magnetometer sample. Both
//! apply their estimates automatically, rate-limited, and both can run a
//! [`Routine`] that collects a *candidate* which only [`ImuCalibrator::apply`]
//! or [`MagCalibrator::apply`] promotes to the applied state.
//!
//! All timestamps are `t_us` (microseconds on any monotonic clock). Routine
//! timeouts are measured from the first sample pushed after the routine starts.

use crate::STANDARD_GRAVITY;
use crate::Vec3;
use crate::ellipsoid::{Ellipsoid, EllipsoidFit, Fit};
use crate::math::{self, clamp, sqrt};
use crate::words::{self, WordsError};

/// `true` when a `push` changed the applied calibration.
pub type Changed = bool;

/// A forced guided procedure. It produces a candidate, which is applied only
/// by `apply()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Routine {
    /// Hold still on each of the six faces (ImuCalibrator).
    AccelSix,
    /// Rotate through as many orientations as possible (MagCalibrator).
    MagRotate,
    /// Hold still for five seconds (ImuCalibrator).
    GyroHold,
}

/// The status of one calibrated part, as reported to clients.
///
/// `samples` and `coverage` are live (what the estimator has seen so far).
/// `confidence` and `residual` describe the applied calibration. All ratios
/// are permille, and `active` is `confidence >= 500`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PartStatus {
    pub samples: u32,
    pub confidence: u16,
    pub coverage: u16,
    pub residual: u16,
    pub active: bool,
}

const ACTIVE_PERMILLE: u16 = 500;

/// Accelerometer sphere radius: the calibrated magnitude is `g`.
const ACCEL_REF: f32 = STANDARD_GRAVITY;
/// Magnetometer sphere radius (µT): the nominal Earth field.
const MAG_REF: f32 = 50.0;
/// Raw field magnitudes outside this range (µT) are not an Earth field.
const MAG_MIN_RAW: f32 = 15.0;
const MAG_MAX_RAW: f32 = 80.0;
const MAG_FORGET: f32 = 0.999;
const ACCEL_FORGET: f32 = 0.9995;
const CELL_MIN_SAMPLES: u16 = 4;

/// Quasi-static test: `|ω|` below this (rad/s) and `|a|` within
/// `QS_ACCEL_TOL` of gravity.
const QS_GYRO: f32 = 0.05;
/// Widened from the design's 5 %: a raw offset of 0.3 m/s² and a 2 % scale
/// already put a face's |a| 5 % off gravity, and the gyro is the motion test.
const QS_ACCEL_TOL: f32 = 0.10;
/// A quasi-static run must last this long (µs) before its samples are used.
const HOLD_US: u64 = 1_000_000;
/// Gyro still window length, in samples.
const GYRO_WINDOW: u32 = 100;
/// Still window: gyro standard deviation below 0.5 °/s (in rad/s).
const GYRO_STD_LIMIT: f32 = 0.008_726_646;
/// Still window: mean `|a|` within this fraction of gravity.
const GYRO_ACCEL_TOL: f32 = 0.03;
/// Blend weight cap for a window's mean, and the sample count that reaches it.
const GYRO_BLEND_MAX: f32 = 0.2;
const GYRO_BLEND_SAMPLES: f32 = 500.0;
/// Still time that gives full gyro confidence.
const GYRO_FULL_STILL_US: u64 = 60_000_000;
/// Fit attempts happen every this many gated samples.
const FIT_CADENCE: u32 = 64;
/// Rate limit: each accepted automatic fit moves the applied calibration this far.
const APPLY_STEP: f32 = 0.25;
/// A fit whose centre moves more than this (times the reference) is an outlier.
const MAX_CENTRE_MOVE: f32 = 0.5;

const ACCEL_MIN_COVERAGE: f32 = 0.6;
const ACCEL_MAX_RESIDUAL: f32 = 0.02;
const ACCEL_MIN_SAMPLES: u32 = 300;
const MAG_MIN_COVERAGE: f32 = 0.6;
const MAG_MAX_RESIDUAL: f32 = 0.02;
const MAG_MIN_SAMPLES: u32 = 300;

/// AccelSix: 100 samples per face, faces within 20° of an axis.
const FACE_SAMPLES: u32 = 100;
const FACE_COS: f32 = 0.939_692_6;
const ACCEL_ROUTINE_TIMEOUT_US: u64 = 180_000_000;
/// GyroHold: five seconds of still windows, timeout 30 s.
const GYRO_HOLD_US: u64 = 5_000_000;
const GYRO_ROUTINE_TIMEOUT_US: u64 = 30_000_000;
/// MagRotate: coverage, residual and samples to finish, timeout 180 s.
const MAG_ROTATE_COVERAGE: f32 = 0.9;
const MAG_ROTATE_MAX_RESIDUAL: f32 = 0.03;
const MAG_ROTATE_MIN_SAMPLES: u32 = 500;
const MAG_ROTATE_TIMEOUT_US: u64 = 180_000_000;

/// Statistics of an applied or candidate fit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Stats {
    samples: u32,
    confidence: u16,
    coverage: u16,
    residual: u16,
}

fn permille(x: f32) -> u16 {
    let v = clamp(x, 0.0, 1.0) * 1000.0;
    math::round_i32(v).clamp(0, 1000) as u16
}

fn fit_stats(fit: &Fit, cov_thr: f32, res_thr: f32, min_samples: u32) -> Stats {
    let cov = clamp(fit.coverage / cov_thr, 0.0, 1.0);
    let res = clamp((2.0 * res_thr - fit.residual) / res_thr, 0.0, 1.0);
    let samp = clamp(fit.samples as f32 / min_samples as f32, 0.0, 1.0);
    Stats {
        samples: fit.samples,
        confidence: permille(math::min(cov, math::min(res, samp))),
        coverage: permille(fit.coverage),
        residual: permille(fit.residual),
    }
}

/// The identity correction for a sphere of `reference` radius.
fn factory_ellipsoid(reference: f32) -> Ellipsoid {
    let k = 1.0 / reference;
    Ellipsoid {
        center: [0.0; 3],
        matrix: [[k, 0.0, 0.0], [0.0, k, 0.0], [0.0, 0.0, k]],
        radius: reference,
    }
}

/// Moves `cur` toward `target` by `step` (1 = replace), blending the centre,
/// the matrix and the radius. The radius follows the target: a fitted one for
/// the magnetometer (the local field), the reference for the accelerometer
/// (the gravity magnitude is pinned, see [`accel_target`]).
fn blend(cur: Ellipsoid, target: Ellipsoid, step: f32) -> Ellipsoid {
    let mut center = cur.center;
    for (c, t) in center.iter_mut().zip(target.center.iter()) {
        *c += step * (*t - *c);
    }
    let mut matrix = cur.matrix;
    for (row, trow) in matrix.iter_mut().zip(target.matrix.iter()) {
        for (m, t) in row.iter_mut().zip(trow.iter()) {
            *m += step * (*t - *m);
        }
    }
    Ellipsoid {
        center,
        matrix,
        radius: cur.radius + step * (target.radius - cur.radius),
    }
}

/// The accelerometer's target: the same shape, scaled to the gravity sphere.
fn accel_target(e: Ellipsoid) -> Ellipsoid {
    Ellipsoid {
        radius: ACCEL_REF,
        ..e
    }
}

/// Live status of a fit-based part, with the applied stats for confidence.
fn fit_part_status(live: &EllipsoidFit, applied: Stats) -> PartStatus {
    PartStatus {
        samples: live.samples().max(applied.samples),
        confidence: applied.confidence,
        coverage: permille(live.coverage()),
        residual: applied.residual,
        active: applied.confidence >= ACTIVE_PERMILLE,
    }
}

/// A forced routine's result, waiting for `apply()`.
#[derive(Clone, Copy, Debug)]
enum Candidate {
    Accel { ellipsoid: Ellipsoid, stats: Stats },
    Gyro { bias: Vec3, stats: Stats },
    Mag { ellipsoid: Ellipsoid, stats: Stats },
}

/// One finished still window.
struct WindowDone {
    accepted: bool,
    mean: Vec3,
    samples: u32,
    duration_us: u64,
}

/// Accelerometer and gyroscope calibration.
///
/// Push every raw sample (SI units) with its timestamp. The applied state is
/// read with [`ImuCalibrator::accel`] and [`ImuCalibrator::gyro_bias`].
#[derive(Clone, Debug)]
pub struct ImuCalibrator {
    last_t: Option<u64>,
    // Accelerometer.
    accel_fit: EllipsoidFit,
    accel_applied: Ellipsoid,
    accel_stats: Stats,
    accel_gate_start: Option<u64>,
    accel_gated: u32,
    // Gyroscope.
    gyro_bias: Vec3,
    gyro_still_us: u64,
    gyro_samples: u32,
    win_n: u32,
    win_t0: u64,
    win_t1: u64,
    win_shift: Vec3,
    win_sum: Vec3,
    win_sumsq: Vec3,
    win_accel_sum: f32,
    // Routines.
    running: Option<Routine>,
    started_us: Option<u64>,
    routine_fit: EllipsoidFit,
    faces: [u32; 6],
    gyro_routine_us: u64,
    gyro_routine_sum: Vec3,
    gyro_routine_n: u32,
    candidate: Option<Candidate>,
    failed: bool,
    revision: u32,
}

impl Default for ImuCalibrator {
    fn default() -> Self {
        Self::new()
    }
}

impl ImuCalibrator {
    /// Factory state: no correction, no bias, no calibration active.
    pub fn new() -> Self {
        ImuCalibrator {
            last_t: None,
            accel_fit: EllipsoidFit::new(ACCEL_REF, ACCEL_FORGET, CELL_MIN_SAMPLES),
            accel_applied: factory_ellipsoid(ACCEL_REF),
            accel_stats: Stats::default(),
            accel_gate_start: None,
            accel_gated: 0,
            gyro_bias: [0.0; 3],
            gyro_still_us: 0,
            gyro_samples: 0,
            win_n: 0,
            win_t0: 0,
            win_t1: 0,
            win_shift: [0.0; 3],
            win_sum: [0.0; 3],
            win_sumsq: [0.0; 3],
            win_accel_sum: 0.0,
            running: None,
            started_us: None,
            routine_fit: EllipsoidFit::new(ACCEL_REF, 1.0, CELL_MIN_SAMPLES),
            faces: [0; 6],
            gyro_routine_us: 0,
            gyro_routine_sum: [0.0; 3],
            gyro_routine_n: 0,
            candidate: None,
            failed: false,
            revision: 0,
        }
    }

    /// Processes one sample: raw accelerometer (m/s²) and gyroscope (rad/s)
    /// at time `t_us`. Returns `true` when the applied calibration changed.
    pub fn push(&mut self, t_us: u64, accel: Vec3, gyro: Vec3) -> Changed {
        self.last_t = Some(t_us);
        if self.running.is_some() && self.started_us.is_none() {
            self.started_us = Some(t_us);
        }

        let mut changed = false;
        let a_mag = math::norm(accel);
        let accel_ok = a_mag > 1e-6 && math::abs(a_mag / STANDARD_GRAVITY - 1.0) <= QS_ACCEL_TOL;
        let quasi_static = accel_ok && math::norm(gyro) < QS_GYRO;

        if quasi_static {
            if self.accel_gate_start.is_none() {
                self.accel_gate_start = Some(t_us);
            }
        } else {
            self.accel_gate_start = None;
        }
        let gated = quasi_static
            && self
                .accel_gate_start
                .is_some_and(|t0| t_us.saturating_sub(t0) >= HOLD_US);

        // Gyro still windows.
        if let Some(done) = self.window_push(t_us, a_mag, gyro)
            && done.accepted
        {
            changed |= self.apply_gyro_window(&done);
        }

        // Accelerometer, continuous.
        if gated {
            self.accel_fit.add(accel);
            self.accel_gated += 1;
            if self.accel_gated >= FIT_CADENCE {
                self.accel_gated = 0;
                changed |= self.try_accel_fit();
            }
        }

        // Routines.
        if let Some(routine) = self.running {
            match routine {
                Routine::AccelSix => {
                    if gated {
                        self.accel_routine_sample(accel);
                    }
                }
                Routine::GyroHold => {}
                Routine::MagRotate => {}
            }
        }
        self.check_timeout(t_us);
        changed
    }

    /// The applied accelerometer correction.
    pub fn accel(&self) -> Ellipsoid {
        self.accel_applied
    }

    /// The applied gyroscope zero-rate offset (rad/s, sensor frame).
    pub fn gyro_bias(&self) -> Vec3 {
        self.gyro_bias
    }

    /// Status of the accelerometer and the gyroscope, in that order.
    pub fn status(&self) -> (PartStatus, PartStatus) {
        let accel = fit_part_status(&self.accel_fit, self.accel_stats);
        let gyro_conf = permille(self.gyro_still_us as f32 / GYRO_FULL_STILL_US as f32);
        let gyro = PartStatus {
            samples: self.gyro_samples,
            confidence: gyro_conf,
            coverage: 0,
            residual: 0,
            active: gyro_conf >= ACTIVE_PERMILLE,
        };
        (accel, gyro)
    }

    /// Counts applied calibration changes (lemnosd persists it).
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// Returns to factory state. The revision advances.
    pub fn reset(&mut self) {
        let revision = self.revision.wrapping_add(1);
        *self = ImuCalibrator::new();
        self.revision = revision;
    }

    /// Starts a routine, ending any running one. A previous candidate and the
    /// failed flag are cleared.
    pub fn start(&mut self, routine: Routine) {
        if !matches!(routine, Routine::AccelSix | Routine::GyroHold) {
            return;
        }
        self.running = Some(routine);
        self.started_us = None;
        self.routine_fit = EllipsoidFit::new(ACCEL_REF, 1.0, CELL_MIN_SAMPLES);
        self.faces = [0; 6];
        self.gyro_routine_us = 0;
        self.gyro_routine_sum = [0.0; 3];
        self.gyro_routine_n = 0;
        self.candidate = None;
        self.failed = false;
    }

    /// Ends the running routine. A finished candidate is kept; a partial one
    /// is dropped.
    pub fn stop(&mut self) {
        self.running = None;
        self.started_us = None;
    }

    /// Promotes the candidate to the applied state. Returns `false` when
    /// there is no candidate.
    pub fn apply(&mut self) -> bool {
        let Some(candidate) = self.candidate.take() else {
            return false;
        };
        match candidate {
            Candidate::Accel { ellipsoid, stats } => {
                self.accel_applied = blend(self.accel_applied, accel_target(ellipsoid), 1.0);
                self.accel_stats = stats;
            }
            Candidate::Gyro { bias, stats } => {
                self.gyro_bias = bias;
                self.gyro_still_us = GYRO_FULL_STILL_US * stats.confidence as u64 / 1000;
                self.gyro_samples = self.gyro_samples.max(stats.samples);
            }
            other => {
                // A magnetometer candidate never sits in the IMU calibrator.
                self.candidate = Some(other);
                return false;
            }
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Drops the candidate and the failed flag.
    pub fn discard(&mut self) {
        self.candidate = None;
        self.failed = false;
    }

    /// The running routine, if any.
    pub fn running(&self) -> Option<Routine> {
        self.running
    }

    /// Progress of the running routine, 0 to 1000 permille.
    pub fn progress_permille(&self) -> u16 {
        match self.running {
            Some(Routine::AccelSix) => {
                let done: u32 = self.faces.iter().map(|f| (*f).min(FACE_SAMPLES)).sum();
                permille(done as f32 / (6 * FACE_SAMPLES) as f32)
            }
            Some(Routine::GyroHold) => permille(self.gyro_routine_us as f32 / GYRO_HOLD_US as f32),
            _ => 0,
        }
    }

    /// `true` when a finished candidate awaits `apply()`.
    pub fn candidate(&self) -> bool {
        self.candidate.is_some()
    }

    /// `true` when the last routine timed out or missed its thresholds.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Writes the applied state as words. Returns the number written, or 0
    /// when `out` is shorter than [`words::IMU_LEN`].
    pub fn words(&self, out: &mut [i32]) -> usize {
        if out.len() < words::IMU_LEN {
            return 0;
        }
        let w = &mut out[..words::IMU_LEN];
        w[0] = words::VERSION;
        w[1] = words::KIND_IMU;
        words::put_vec(&mut w[2..5], self.accel_applied.center, words::MICRO);
        words::put_matrix(&mut w[5..14], self.accel_applied.matrix, words::MICRO);
        words::put_vec(&mut w[14..17], self.gyro_bias, words::MICRO);
        w[17] = self.accel_stats.confidence as i32;
        w[18] = self.accel_stats.coverage as i32;
        w[19] = self.accel_stats.residual as i32;
        w[20] = self.accel_fit.samples().max(self.accel_stats.samples) as i32;
        let gyro_conf = permille(self.gyro_still_us as f32 / GYRO_FULL_STILL_US as f32);
        w[21] = gyro_conf as i32;
        w[22] = self.gyro_samples as i32;
        w[23] = self.revision as i32;
        words::IMU_LEN
    }

    /// Loads words written by [`ImuCalibrator::words`]. A wrong version or
    /// kind, or too few words, is refused and the state is left unchanged.
    pub fn load(&mut self, w: &[i32]) -> Result<(), WordsError> {
        words::check(w, words::KIND_IMU, words::IMU_LEN)?;
        let center = words::get_vec(&w[2..5], words::MICRO);
        let matrix = words::get_matrix(&w[5..14], words::MICRO);
        self.accel_applied = Ellipsoid {
            center,
            matrix,
            radius: ACCEL_REF,
        };
        self.accel_stats = Stats {
            samples: w[20].max(0) as u32,
            confidence: w[17].clamp(0, 1000) as u16,
            coverage: w[18].clamp(0, 1000) as u16,
            residual: w[19].clamp(0, 1000) as u16,
        };
        self.gyro_bias = words::get_vec(&w[14..17], words::MICRO);
        self.gyro_still_us = GYRO_FULL_STILL_US * w[21].clamp(0, 1000) as u64 / 1000;
        self.gyro_samples = w[22].max(0) as u32;
        self.revision = w[23].max(0) as u32;
        Ok(())
    }

    fn window_push(&mut self, t_us: u64, accel_mag: f32, gyro: Vec3) -> Option<WindowDone> {
        if self.win_n == 0 {
            self.win_t0 = t_us;
            self.win_shift = gyro;
            self.win_sum = [0.0; 3];
            self.win_sumsq = [0.0; 3];
            self.win_accel_sum = 0.0;
        }
        for (k, g) in gyro.iter().enumerate() {
            let d = g - self.win_shift[k];
            self.win_sum[k] += d;
            self.win_sumsq[k] += d * d;
        }
        self.win_accel_sum += accel_mag;
        self.win_n += 1;
        self.win_t1 = t_us;
        if self.win_n < GYRO_WINDOW {
            return None;
        }

        let n = self.win_n as f32;
        let mut var_sum = 0.0;
        let mut mean = [0.0_f32; 3];
        for (k, mk) in mean.iter_mut().enumerate() {
            let m = self.win_sum[k] / n;
            *mk = self.win_shift[k] + m;
            var_sum += math::max(0.0, self.win_sumsq[k] / n - m * m);
        }
        let std = sqrt(var_sum);
        let accel_mean = self.win_accel_sum / n;
        let accepted = std < GYRO_STD_LIMIT
            && math::abs(accel_mean / STANDARD_GRAVITY - 1.0) <= GYRO_ACCEL_TOL;
        let done = WindowDone {
            accepted,
            mean,
            samples: self.win_n,
            duration_us: self.win_t1.saturating_sub(self.win_t0),
        };
        self.win_n = 0;
        Some(done)
    }

    fn apply_gyro_window(&mut self, done: &WindowDone) -> Changed {
        let w = math::min(GYRO_BLEND_MAX, done.samples as f32 / GYRO_BLEND_SAMPLES);
        for i in 0..3 {
            self.gyro_bias[i] += w * (done.mean[i] - self.gyro_bias[i]);
        }
        self.gyro_samples = self.gyro_samples.saturating_add(done.samples);
        self.gyro_still_us = self
            .gyro_still_us
            .saturating_add(done.duration_us.min(2_000_000));

        if let Some(Routine::GyroHold) = self.running {
            self.gyro_routine_us = self.gyro_routine_us.saturating_add(done.duration_us);
            for i in 0..3 {
                self.gyro_routine_sum[i] += done.mean[i] * done.samples as f32;
            }
            self.gyro_routine_n = self.gyro_routine_n.saturating_add(done.samples);
            if self.gyro_routine_us >= GYRO_HOLD_US && self.gyro_routine_n > 0 {
                let inv = 1.0 / self.gyro_routine_n as f32;
                let bias = math::scale(self.gyro_routine_sum, inv);
                let stats = Stats {
                    samples: self.gyro_routine_n,
                    confidence: 1000,
                    coverage: 0,
                    residual: 0,
                };
                self.candidate = Some(Candidate::Gyro { bias, stats });
                self.running = None;
                self.started_us = None;
            }
        }
        true
    }

    fn try_accel_fit(&mut self) -> Changed {
        let Some(f) = self.accel_fit.fit() else {
            return false;
        };
        self.accel_fit.set_centre(f.ellipsoid.center);
        if f.coverage < ACCEL_MIN_COVERAGE
            || f.residual > ACCEL_MAX_RESIDUAL
            || f.samples < ACCEL_MIN_SAMPLES
        {
            return false;
        }
        let moved = math::norm(math::sub(f.ellipsoid.center, self.accel_applied.center));
        if moved > MAX_CENTRE_MOVE * ACCEL_REF {
            return false;
        }
        self.accel_applied = blend(self.accel_applied, accel_target(f.ellipsoid), APPLY_STEP);
        self.accel_stats = fit_stats(
            &f,
            ACCEL_MIN_COVERAGE,
            ACCEL_MAX_RESIDUAL,
            ACCEL_MIN_SAMPLES,
        );
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn accel_routine_sample(&mut self, accel: Vec3) {
        let norm = math::norm(accel);
        if norm <= 1e-6 {
            return;
        }
        let mut best = 0;
        for i in 1..3 {
            if math::abs(accel[i]) > math::abs(accel[best]) {
                best = i;
            }
        }
        if math::abs(accel[best]) / norm < FACE_COS {
            return;
        }
        let face = best * 2 + usize::from(accel[best] > 0.0);
        if self.faces[face] >= FACE_SAMPLES {
            return;
        }
        self.faces[face] += 1;
        self.routine_fit.add(accel);
        if self.faces.iter().all(|f| *f >= FACE_SAMPLES) {
            self.running = None;
            self.started_us = None;
            match self.routine_fit.fit() {
                Some(f) if f.residual <= ACCEL_MAX_RESIDUAL && f.samples >= ACCEL_MIN_SAMPLES => {
                    // Six faces are the coverage here: the cell coverage of six
                    // directions is structurally low.
                    let mut stats = fit_stats(
                        &f,
                        ACCEL_MIN_COVERAGE,
                        ACCEL_MAX_RESIDUAL,
                        ACCEL_MIN_SAMPLES,
                    );
                    stats.coverage = 1000;
                    self.candidate = Some(Candidate::Accel {
                        ellipsoid: f.ellipsoid,
                        stats,
                    });
                }
                _ => self.failed = true,
            }
        }
    }

    fn check_timeout(&mut self, t_us: u64) {
        let Some(routine) = self.running else {
            return;
        };
        let Some(start) = self.started_us else {
            return;
        };
        let limit = match routine {
            Routine::AccelSix => ACCEL_ROUTINE_TIMEOUT_US,
            Routine::GyroHold => GYRO_ROUTINE_TIMEOUT_US,
            Routine::MagRotate => MAG_ROTATE_TIMEOUT_US,
        };
        if t_us.saturating_sub(start) > limit {
            self.running = None;
            self.started_us = None;
            self.failed = true;
        }
    }
}

/// Magnetometer calibration. The field is in µT.
#[derive(Clone, Debug)]
pub struct MagCalibrator {
    last_t: Option<u64>,
    fit: EllipsoidFit,
    applied: Ellipsoid,
    stats: Stats,
    gated: u32,
    running: bool,
    started_us: Option<u64>,
    routine_fit: EllipsoidFit,
    candidate: Option<Candidate>,
    failed: bool,
    revision: u32,
}

impl Default for MagCalibrator {
    fn default() -> Self {
        Self::new()
    }
}

impl MagCalibrator {
    /// Factory state: no correction, not active.
    pub fn new() -> Self {
        MagCalibrator {
            last_t: None,
            fit: EllipsoidFit::new(MAG_REF, MAG_FORGET, CELL_MIN_SAMPLES),
            applied: factory_ellipsoid(MAG_REF),
            stats: Stats::default(),
            gated: 0,
            running: false,
            started_us: None,
            routine_fit: EllipsoidFit::new(MAG_REF, MAG_FORGET, CELL_MIN_SAMPLES),
            candidate: None,
            failed: false,
            revision: 0,
        }
    }

    /// Processes one calibrated-units field sample (raw, µT) at `t_us`.
    /// Returns `true` when the applied calibration changed.
    pub fn push(&mut self, t_us: u64, field_ut: Vec3) -> Changed {
        self.last_t = Some(t_us);
        if self.running && self.started_us.is_none() {
            self.started_us = Some(t_us);
        }
        let mag = math::norm(field_ut);
        let in_range = (MAG_MIN_RAW..=MAG_MAX_RAW).contains(&mag);
        let mut changed = false;
        if in_range {
            self.fit.add(field_ut);
            self.gated += 1;
            if self.gated >= FIT_CADENCE {
                self.gated = 0;
                changed |= self.try_fit();
            }
            if self.running {
                self.routine_fit.add(field_ut);
                if self.routine_fit.samples().is_multiple_of(FIT_CADENCE)
                    && self.routine_fit.samples() >= MAG_ROTATE_MIN_SAMPLES
                {
                    self.try_routine_done();
                }
            }
        }
        if let (true, Some(start)) = (self.running, self.started_us)
            && t_us.saturating_sub(start) > MAG_ROTATE_TIMEOUT_US
        {
            self.running = false;
            self.started_us = None;
            self.failed = true;
        }
        changed
    }

    /// The applied correction.
    pub fn field(&self) -> Ellipsoid {
        self.applied
    }

    /// Status of the magnetometer calibration.
    pub fn status(&self) -> PartStatus {
        fit_part_status(&self.fit, self.stats)
    }

    /// Counts applied calibration changes.
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// Returns to factory state. The revision advances.
    pub fn reset(&mut self) {
        let revision = self.revision.wrapping_add(1);
        *self = MagCalibrator::new();
        self.revision = revision;
    }

    /// Starts `MagRotate`, ending any running one. Other routines are not
    /// offered by the magnetometer and are ignored.
    pub fn start(&mut self, routine: Routine) {
        if routine != Routine::MagRotate {
            return;
        }
        self.running = true;
        self.started_us = None;
        self.routine_fit = EllipsoidFit::new(MAG_REF, MAG_FORGET, CELL_MIN_SAMPLES);
        self.candidate = None;
        self.failed = false;
    }

    /// Ends the running routine, keeping a finished candidate.
    pub fn stop(&mut self) {
        self.running = false;
        self.started_us = None;
    }

    /// Promotes the candidate. `false` when there is none.
    pub fn apply(&mut self) -> bool {
        let Some(Candidate::Mag { ellipsoid, stats }) = self.candidate.take() else {
            return false;
        };
        self.applied = blend(self.applied, ellipsoid, 1.0);
        self.stats = stats;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Drops the candidate and the failed flag.
    pub fn discard(&mut self) {
        self.candidate = None;
        self.failed = false;
    }

    /// The running routine, if any.
    pub fn running(&self) -> Option<Routine> {
        self.running.then_some(Routine::MagRotate)
    }

    /// Progress of `MagRotate`: the slower of coverage and samples, 0 to 1000.
    pub fn progress_permille(&self) -> u16 {
        if !self.running {
            return 0;
        }
        let cov = self.routine_fit.coverage() / MAG_ROTATE_COVERAGE;
        let samp = self.routine_fit.samples() as f32 / MAG_ROTATE_MIN_SAMPLES as f32;
        permille(math::min(cov, samp))
    }

    /// `true` when a finished candidate awaits `apply()`.
    pub fn candidate(&self) -> bool {
        self.candidate.is_some()
    }

    /// `true` when the routine timed out.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Writes the applied state as words. Returns the number written, or 0
    /// when `out` is shorter than [`words::MAG_LEN`].
    pub fn words(&self, out: &mut [i32]) -> usize {
        if out.len() < words::MAG_LEN {
            return 0;
        }
        let w = &mut out[..words::MAG_LEN];
        w[0] = words::VERSION;
        w[1] = words::KIND_MAG;
        words::put_vec(&mut w[2..5], self.applied.center, words::MILLI);
        words::put_matrix(&mut w[5..14], self.applied.matrix, words::MICRO);
        w[14] = self.stats.confidence as i32;
        w[15] = self.stats.coverage as i32;
        w[16] = self.stats.residual as i32;
        w[17] = self.fit.samples().max(self.stats.samples) as i32;
        w[18] = self.revision as i32;
        w[19] = (self.applied.radius * 1000.0).clamp(0.0, 2.0e9) as i32;
        words::MAG_LEN
    }

    /// Loads words written by [`MagCalibrator::words`].
    pub fn load(&mut self, w: &[i32]) -> Result<(), WordsError> {
        words::check(w, words::KIND_MAG, words::MAG_LEN)?;
        self.applied = Ellipsoid {
            center: words::get_vec(&w[2..5], words::MILLI),
            matrix: words::get_matrix(&w[5..14], words::MICRO),
            radius: w[19] as f32 / 1000.0,
        };
        self.stats = Stats {
            samples: w[17].max(0) as u32,
            confidence: w[14].clamp(0, 1000) as u16,
            coverage: w[15].clamp(0, 1000) as u16,
            residual: w[16].clamp(0, 1000) as u16,
        };
        self.revision = w[18].max(0) as u32;
        Ok(())
    }

    fn try_fit(&mut self) -> Changed {
        let Some(f) = self.fit.fit() else {
            return false;
        };
        self.fit.set_centre(f.ellipsoid.center);
        if f.coverage < MAG_MIN_COVERAGE
            || f.residual > MAG_MAX_RESIDUAL
            || f.samples < MAG_MIN_SAMPLES
        {
            return false;
        }
        let moved = math::norm(math::sub(f.ellipsoid.center, self.applied.center));
        if moved > MAX_CENTRE_MOVE * MAG_REF {
            return false;
        }
        self.applied = blend(self.applied, f.ellipsoid, APPLY_STEP);
        self.stats = fit_stats(&f, MAG_MIN_COVERAGE, MAG_MAX_RESIDUAL, MAG_MIN_SAMPLES);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn try_routine_done(&mut self) {
        let Some(f) = self.routine_fit.fit() else {
            return;
        };
        if f.coverage >= MAG_ROTATE_COVERAGE
            && f.residual <= MAG_ROTATE_MAX_RESIDUAL
            && f.samples >= MAG_ROTATE_MIN_SAMPLES
        {
            let stats = fit_stats(
                &f,
                MAG_ROTATE_COVERAGE,
                MAG_ROTATE_MAX_RESIDUAL,
                MAG_ROTATE_MIN_SAMPLES,
            );
            self.candidate = Some(Candidate::Mag {
                ellipsoid: f.ellipsoid,
                stats,
            });
            self.running = false;
            self.started_us = None;
        }
    }
}

#[cfg(test)]
mod tests;
