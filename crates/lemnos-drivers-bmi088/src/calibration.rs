//! The IMU's calibration (feature `float`): the automatic estimators of
//! `lemnos-fusion` fed by every full sample the driver reads, and the host
//! contract (`lemnos_device::Sensor`'s calibration methods) over them.
//!
//! The estimators run on sample time. The host gives each read its boot-clock
//! time (`Sensor::set_clock_us`), so a sample's time is that time less the
//! samples newer than it, and every timer (still time, routine timeouts, the
//! fit's dwell) is wall time, whatever the read rate. Without a host clock (a
//! standalone read), each sample advances the clock by the accelerometer's
//! period. A read that does not take every axis of both dies (a
//! channel-selective read) adds no sample.

use lemnos_device::{
    CalibrationCommand, CalibrationPart, CalibrationRoutine, CalibrationStatus, PART_ACCEL,
    PART_GYRO,
};
use lemnos_fusion::{ImuCalibrator, PartStatus, Routine};

impl core::fmt::Debug for Calibration {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Calibration")
    }
}

/// The applied calibration and the estimators behind it.
pub(crate) struct Calibration {
    imu: ImuCalibrator,
    /// The sample clock without a host clock, microseconds.
    clock_us: u64,
    /// The host's boot-clock time of the newest sample of the read in progress.
    read_us: Option<u64>,
}

/// A command the IMU cannot run (a magnetometer routine).
pub(crate) struct Unsupported;

impl Calibration {
    pub(crate) fn new() -> Self {
        Self {
            imu: ImuCalibrator::new(),
            clock_us: 0,
            read_us: None,
        }
    }

    /// The host's time of the read about to run.
    pub(crate) fn set_read_us(&mut self, t_us: u64) {
        self.read_us = Some(t_us);
    }

    /// Feeds one full sample (SI units) and returns the calibrated acceleration
    /// (m/s²) and angular rate (rad/s). The sample is `back_us` older than the
    /// newest of its read; `period_us` is the sample spacing (used only without
    /// a host clock).
    pub(crate) fn sample(
        &mut self,
        back_us: u64,
        period_us: u64,
        accel: [f32; 3],
        gyro: [f32; 3],
    ) -> ([f32; 3], [f32; 3]) {
        let t = match self.read_us {
            Some(read) => read.saturating_sub(back_us),
            None => {
                self.clock_us = self.clock_us.saturating_add(period_us.max(1));
                self.clock_us
            }
        };
        let _ = self.imu.push(t, accel, gyro);
        let a = self.imu.accel().apply(accel);
        let bias = self.imu.gyro_bias();
        let g = [gyro[0] - bias[0], gyro[1] - bias[1], gyro[2] - bias[2]];
        (a, g)
    }

    pub(crate) fn status(&self) -> CalibrationStatus {
        let (accel, gyro) = self.imu.status();
        let mut parts = [CalibrationPart::default(); 3];
        parts[PART_ACCEL] = part(accel);
        parts[PART_GYRO] = part(gyro);
        CalibrationStatus {
            revision: self.imu.revision(),
            running: self.imu.running().map(|r| match r {
                Routine::AccelSix => CalibrationRoutine::AccelSix,
                Routine::GyroHold => CalibrationRoutine::GyroHold,
                Routine::MagRotate => CalibrationRoutine::MagRotate,
            }),
            progress: self.imu.progress_permille(),
            candidate: self.imu.candidate(),
            failed: self.imu.failed(),
            parts,
        }
    }

    pub(crate) fn command(&mut self, command: CalibrationCommand) -> Result<(), Unsupported> {
        match command {
            CalibrationCommand::Start(CalibrationRoutine::AccelSix) => {
                self.imu.start(Routine::AccelSix)
            }
            CalibrationCommand::Start(CalibrationRoutine::GyroHold) => {
                self.imu.start(Routine::GyroHold)
            }
            CalibrationCommand::Start(CalibrationRoutine::MagRotate) => return Err(Unsupported),
            CalibrationCommand::Stop => self.imu.stop(),
            CalibrationCommand::Apply => {
                let _ = self.imu.apply();
            }
            CalibrationCommand::Discard => self.imu.discard(),
            CalibrationCommand::Reset => self.imu.reset(),
        }
        Ok(())
    }

    pub(crate) fn words(&self, out: &mut [i32]) -> usize {
        self.imu.words(out)
    }

    /// Loads words written by [`words`](Self::words); `Err(())` for a version,
    /// kind or length the layout does not take (the state is then unchanged).
    pub(crate) fn load(&mut self, words: &[i32]) -> Result<(), ()> {
        self.imu.load(words).map_err(|_| ())
    }
}

fn part(status: PartStatus) -> CalibrationPart {
    CalibrationPart {
        samples: status.samples,
        confidence: status.confidence,
        coverage: status.coverage,
        residual: status.residual,
        active: status.active,
    }
}
