//! The float rounding the builders need and `core` lacks. Exact for every finite input whose
//! integer part fits `i64`, and the identity beyond (every float that large is already an
//! integer). One code path on every target, so the host tests cover what targets run.
//!
//! The one difference from `std` is the sign of zero: a result of `-0.0` comes back as `+0.0`.
//! Callers convert the results to integers, so it is not observable.

/// Truncation toward zero: exact below 2^52 in magnitude, the identity above.
fn trunc(x: f64) -> f64 {
    const LIMIT: f64 = 4_503_599_627_370_496.0;
    if x.abs() < LIMIT {
        (x as i64) as f64
    } else {
        x
    }
}

/// Nearest integer, halves away from zero.
pub(crate) fn round(x: f64) -> f64 {
    let t = trunc(x);
    if (x - t).abs() >= 0.5 {
        t + if x < 0.0 { -1.0 } else { 1.0 }
    } else {
        t
    }
}

/// Largest integer not above `x`.
pub(crate) fn floor(x: f64) -> f64 {
    let t = trunc(x);
    if x < t { t - 1.0 } else { t }
}

/// Smallest integer not below `x`.
pub(crate) fn ceil(x: f64) -> f64 {
    let t = trunc(x);
    if x > t { t + 1.0 } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_std_on_representative_values() {
        for x in [
            0.0,
            0.25,
            0.5,
            0.75,
            1.0,
            1.5,
            2.5,
            -0.25,
            -0.5,
            -0.75,
            -1.5,
            -2.5,
            1023.5,
            1e15,
            -1e15,
            4_503_599_627_370_497.0,
        ] {
            assert_eq!(round(x), f64::round(x), "round {x}");
            assert_eq!(floor(x), f64::floor(x), "floor {x}");
            assert_eq!(ceil(x), f64::ceil(x), "ceil {x}");
        }
    }

    #[test]
    fn non_finite_values_pass_through() {
        assert!(round(f64::NAN).is_nan());
        assert_eq!(floor(f64::INFINITY), f64::INFINITY);
        assert_eq!(ceil(f64::NEG_INFINITY), f64::NEG_INFINITY);
    }
}
