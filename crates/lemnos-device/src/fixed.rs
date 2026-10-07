//! Fixed-point helpers: values are integers times a power of ten.

use crate::NO_VALUE;

const POW10: [i64; 19] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];

/// Converts `value × 10^from` to the nearest `x × 10^to`, saturating to the
/// `i32` range without producing [`NO_VALUE`].
///
/// Drivers use it to move a reading from the unit they compute in (nA, µW) to
/// the channel's exponent.
pub fn rescale(value: i64, from: i8, to: i8) -> i32 {
    let shift = i32::from(from) - i32::from(to);
    let scaled = if shift >= 0 {
        match POW10.get(shift as usize) {
            Some(p) => value.saturating_mul(*p),
            None if value == 0 => 0,
            None => value.signum() * i64::MAX,
        }
    } else {
        match POW10.get(shift.unsigned_abs() as usize) {
            Some(p) => {
                // Round half away from zero.
                let half = p / 2;
                if value >= 0 {
                    (value + half) / p
                } else {
                    (value - half) / p
                }
            }
            None => 0,
        }
    };
    scaled.clamp(i64::from(NO_VALUE) + 1, i64::from(i32::MAX)) as i32
}

/// `raw × 10^exponent` as `f32`.
#[cfg(feature = "float")]
pub fn to_f32(raw: i32, exponent: i8) -> f32 {
    // Divide by an exact power of ten rather than multiplying by 0.1
    // repeatedly, which accumulates rounding.
    let scale = pow10_f32(exponent.unsigned_abs());
    if exponent < 0 {
        raw as f32 / scale
    } else {
        raw as f32 * scale
    }
}

/// `value` in units as the nearest raw count at `exponent`, or `None` if it is
/// not finite or does not fit.
#[cfg(feature = "float")]
pub fn from_f32(value: f32, exponent: i8) -> Option<i32> {
    let scale = pow10_f32(exponent.unsigned_abs());
    let raw = if exponent < 0 {
        value * scale
    } else {
        value / scale
    };
    let rounded = if raw >= 0.0 { raw + 0.5 } else { raw - 0.5 };
    (rounded.is_finite() && rounded > NO_VALUE as f32 && rounded < i32::MAX as f32)
        .then_some(rounded as i32)
}

#[cfg(feature = "float")]
fn pow10_f32(exponent: u8) -> f32 {
    let mut value = 1.0f32;
    for _ in 0..exponent {
        value *= 10.0;
    }
    value
}
