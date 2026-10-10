//! Persisted calibration words.
//!
//! An applied calibration is a short run of `i32` words: word 0 is the layout
//! version ([`VERSION`]), word 1 the part kind ([`KIND_IMU`] or [`KIND_MAG`]).
//! Values are fixed-point: offsets in µm/s², µrad/s (IMU) or nT (magnetometer),
//! matrix entries × 10⁶, confidences and coverage and residual in permille,
//! sample counts and revisions exact. A word set of another version or kind is
//! refused, so an IMU's words never load into the magnetometer.

use crate::Vec3;
use crate::math::round_i32;

/// Layout version of the words.
pub const VERSION: i32 = 1;
/// Kind word of an IMU (accelerometer and gyroscope) calibration.
pub const KIND_IMU: i32 = 1;
/// Kind word of a magnetometer calibration.
pub const KIND_MAG: i32 = 2;
/// The most words a calibration writes.
pub const MAX_CALIBRATION_WORDS: usize = 64;
/// Words in an IMU calibration.
pub const IMU_LEN: usize = 24;
/// Words in a magnetometer calibration.
pub const MAG_LEN: usize = 20;

/// Fixed-point scale of SI micro-units (m/s² and rad/s to µm/s² and µrad/s).
pub(crate) const MICRO: f32 = 1_000_000.0;
/// Fixed-point scale of nanotesla (µT to nT).
pub(crate) const MILLI: f32 = 1_000.0;

/// Refusal to load words: a wrong version or kind, or too few words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordsError;

impl core::fmt::Display for WordsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("calibration words: wrong version, kind, or length")
    }
}

pub(crate) fn check(w: &[i32], kind: i32, len: usize) -> Result<(), WordsError> {
    if w.len() < len || w[0] != VERSION || w[1] != kind {
        return Err(WordsError);
    }
    Ok(())
}

pub(crate) fn put_vec(dst: &mut [i32], v: Vec3, scale: f32) {
    for (d, x) in dst.iter_mut().zip(v.iter()) {
        *d = round_i32(*x * scale);
    }
}

pub(crate) fn get_vec(src: &[i32], scale: f32) -> Vec3 {
    [
        src[0] as f32 / scale,
        src[1] as f32 / scale,
        src[2] as f32 / scale,
    ]
}

pub(crate) fn put_matrix(dst: &mut [i32], m: [[f32; 3]; 3], scale: f32) {
    for (i, d) in dst.iter_mut().enumerate() {
        *d = round_i32(m[i / 3][i % 3] * scale);
    }
}

pub(crate) fn get_matrix(src: &[i32], scale: f32) -> [[f32; 3]; 3] {
    let mut m = [[0.0_f32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = src[i * 3 + j] as f32 / scale;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use crate::calibrate::{ImuCalibrator, MagCalibrator};
    use crate::words::{self, WordsError};

    #[test]
    fn imu_words_round_trip() {
        let mut a = ImuCalibrator::new();
        // Put a known applied state in place through the public path: a
        // loaded word set is the only way to set it from outside the fit.
        let mut w = [0i32; 64];
        let n = a.words(&mut w);
        assert_eq!(n, words::IMU_LEN);
        assert!(n <= words::MAX_CALIBRATION_WORDS);
        assert_eq!(w[0], words::VERSION);
        assert_eq!(w[1], words::KIND_IMU);

        // Hand-built words with non-trivial values.
        let mut src = [0i32; words::IMU_LEN];
        src[0] = words::VERSION;
        src[1] = words::KIND_IMU;
        src[2] = 300_000; // 0.3 m/s²
        src[3] = -200_000;
        src[4] = 100_000;
        src[5] = 1_020_000; // matrix diagonal, scaled by 1e6
        src[9] = 980_000;
        src[13] = 1_010_000;
        src[6] = 50_000; // off-diagonal
        src[14] = 20_000; // gyro bias 0.02 rad/s
        src[15] = -10_000;
        src[16] = 5_000;
        src[17] = 900;
        src[18] = 850;
        src[19] = 12;
        src[20] = 4_321;
        src[21] = 750;
        src[22] = 6_000;
        src[23] = 42;
        a.load(&src).expect("load");
        assert_eq!(a.revision(), 42);
        let (acc, gyr) = a.status();
        assert!(acc.active && gyr.active);
        assert_eq!(acc.confidence, 900);
        assert_eq!(acc.samples, 4_321);
        assert_eq!(gyr.samples, 6_000);

        let mut out = [0i32; 64];
        let n = a.words(&mut out);
        assert_eq!(n, words::IMU_LEN);
        for i in 0..words::IMU_LEN {
            if i == 20 {
                // Samples: the live fit count (0 here) or the loaded count.
                assert_eq!(out[i], 4_321);
            } else if i == 22 {
                assert_eq!(out[i], 6_000);
            } else {
                assert_eq!(out[i], src[i], "word {i}");
            }
        }
        // Exact values of the decoded applied state.
        let e = a.accel();
        assert!((e.center[0] - 0.3).abs() < 1e-6);
        assert!((e.matrix[0][0] - 1.02).abs() < 1e-6);
        assert!((a.gyro_bias()[0] - 0.02).abs() < 1e-6);
    }

    #[test]
    fn mag_words_round_trip() {
        let mut m = MagCalibrator::new();
        let mut src = [0i32; words::MAG_LEN];
        src[0] = words::VERSION;
        src[1] = words::KIND_MAG;
        src[2] = 10_000; // 10 µT offset, in nT
        src[3] = -5_000;
        src[4] = 20_000;
        src[5] = 20_000; // 0.02 per µT
        src[9] = 19_000;
        src[13] = 21_000;
        src[14] = 800;
        src[15] = 700;
        src[16] = 15;
        src[17] = 999;
        src[18] = 7;
        src[19] = 50_000; // radius 50 µT, in milli-µT
        m.load(&src).expect("load");
        assert!(m.status().active);
        let e = m.field();
        assert!((e.center[0] - 10.0).abs() < 1e-6);
        assert!((e.radius - 50.0).abs() < 1e-6);
        let mut out = [0i32; 64];
        let n = m.words(&mut out);
        assert_eq!(n, words::MAG_LEN);
        for i in 0..words::MAG_LEN {
            if i == 17 {
                assert_eq!(out[i], 999);
            } else {
                assert_eq!(out[i], src[i], "word {i}");
            }
        }
    }

    #[test]
    fn version_and_kind_are_refused() {
        let mut m = MagCalibrator::new();
        let mut imu_words = [0i32; words::IMU_LEN];
        let mut a = ImuCalibrator::new();
        a.words(&mut imu_words);

        // An IMU word set does not load into the magnetometer.
        assert_eq!(m.load(&imu_words), Err(WordsError));

        // A wrong version is refused.
        imu_words[0] = 2;
        assert_eq!(a.load(&imu_words), Err(WordsError));

        // Too few words is refused.
        let mut mag_words = [0i32; words::MAG_LEN];
        let n = m.words(&mut mag_words);
        assert_eq!(n, words::MAG_LEN);
        assert_eq!(a.load(&mag_words), Err(WordsError));
        assert_eq!(m.load(&mag_words[..5]), Err(WordsError));

        // A short output buffer writes nothing.
        let mut short = [0i32; 4];
        assert_eq!(a.words(&mut short), 0);
    }
}
