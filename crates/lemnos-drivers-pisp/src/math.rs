//! The float rounding the configuration builders need and `core` lacks: `round`, `floor` and
//! `ceil`. Compiled only without `std`, where the callers' `.round()` and friends resolve to
//! these; with `std` they are the inherent methods. The versions here need no `libm`: exact for
//! every finite input whose integer part fits `i64`, and the identity beyond (every float that
//! large is already an integer).
//!
//! The one difference is the sign of zero: a result of `-0.0` from `round`/`floor`/`ceil`
//! comes back as `+0.0`. Callers convert the results to integers, so it is not observable.

/// Rounding methods on `f32` and `f64`, the same names as the inherent ones.
pub(crate) trait Float: Copy {
    /// Nearest integer, halves away from zero.
    fn round(self) -> Self;
    /// Largest integer not above `self`.
    fn floor(self) -> Self;
    /// Smallest integer not below `self`.
    fn ceil(self) -> Self;
}

/// The `no_std` versions for `$t`, with its truncation helper `$trunc`.
macro_rules! no_std_float {
    ($t:ident, $trunc:ident) => {
        impl Float for $t {
            fn round(self) -> Self {
                let t = $trunc(self);
                let half_or_more = (self - t).abs() >= 0.5;
                if half_or_more {
                    t + if self < 0.0 { -1.0 } else { 1.0 }
                } else {
                    t
                }
            }

            fn floor(self) -> Self {
                let t = $trunc(self);
                if self < t { t - 1.0 } else { t }
            }

            fn ceil(self) -> Self {
                let t = $trunc(self);
                if self > t { t + 1.0 } else { t }
            }
        }

        /// Truncation toward zero: exact below 2^52 in magnitude, the identity above (every
        /// float that large is an integer already).
        #[inline]
        fn $trunc(x: $t) -> $t {
            const LIMIT: $t = 4_503_599_627_370_496.0;
            if x.abs() < LIMIT { (x as i64) as $t } else { x }
        }
    };
}

no_std_float!(f32, trunc_f32);
no_std_float!(f64, trunc_f64);
