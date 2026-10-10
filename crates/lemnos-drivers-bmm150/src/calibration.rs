//! The magnetometer's calibration (feature `float`): the hard- and soft-iron
//! estimator of `lemnos-fusion`, fed by every reading the driver takes, and
//! the host contract (`lemnos_device::Sensor`'s calibration methods) over it.
//!
//! The estimator has no clock of its own (the driver is `no_std`), so a
//! reading's time is the count of readings times the data-rate period.

use lemnos_device::{
    CalibrationCommand, CalibrationPart, CalibrationRoutine, CalibrationStatus, PART_MAG,
};
use lemnos_fusion::{MagCalibrator, PartStatus, Routine};

/// The applied calibration and the estimator behind it.
pub(crate) struct Calibration {
    mag: MagCalibrator,
    /// The reading clock, microseconds.
    clock_us: u64,
}

impl core::fmt::Debug for Calibration {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Calibration")
    }
}

/// A command the magnetometer cannot run (an IMU routine).
pub(crate) struct Unsupported;

impl Calibration {
    pub(crate) fn new() -> Self {
        Self {
            mag: MagCalibrator::new(),
            clock_us: 0,
        }
    }

    /// Feeds one reading (µT), taken `period_us` after the last, and returns
    /// the calibrated field (µT).
    pub(crate) fn sample(&mut self, period_us: u64, field_ut: [f32; 3]) -> [f32; 3] {
        self.clock_us = self.clock_us.saturating_add(period_us.max(1));
        let _ = self.mag.push(self.clock_us, field_ut);
        self.mag.field().apply(field_ut)
    }

    pub(crate) fn status(&self) -> CalibrationStatus {
        let mut parts = [CalibrationPart::default(); 3];
        parts[PART_MAG] = part(self.mag.status());
        CalibrationStatus {
            revision: self.mag.revision(),
            running: self.mag.running().map(|r| match r {
                Routine::MagRotate => CalibrationRoutine::MagRotate,
                Routine::AccelSix => CalibrationRoutine::AccelSix,
                Routine::GyroHold => CalibrationRoutine::GyroHold,
            }),
            progress: self.mag.progress_permille(),
            candidate: self.mag.candidate(),
            failed: self.mag.failed(),
            parts,
        }
    }

    pub(crate) fn command(&mut self, command: CalibrationCommand) -> Result<(), Unsupported> {
        match command {
            CalibrationCommand::Start(CalibrationRoutine::MagRotate) => {
                self.mag.start(Routine::MagRotate)
            }
            CalibrationCommand::Start(_) => return Err(Unsupported),
            CalibrationCommand::Stop => self.mag.stop(),
            CalibrationCommand::Apply => {
                let _ = self.mag.apply();
            }
            CalibrationCommand::Discard => self.mag.discard(),
            CalibrationCommand::Reset => self.mag.reset(),
        }
        Ok(())
    }

    pub(crate) fn words(&self, out: &mut [i32]) -> usize {
        self.mag.words(out)
    }

    /// Loads words written by [`words`](Self::words); `Err(())` for a version,
    /// kind or length the layout does not take (the state is then unchanged).
    pub(crate) fn load(&mut self, words: &[i32]) -> Result<(), ()> {
        self.mag.load(words).map_err(|_| ())
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
