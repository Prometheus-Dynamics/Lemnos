//! Curve mechanics for the back end's table blocks: a caller's curve into the gamma block's
//! packed LUT. The curves themselves are tuning, and stay with the caller.
//!
//! Ported from libpisp `src/libpisp/backend/backend_default_config.cpp` (BSD-2-Clause,
//! Copyright (C) 2021 - 2023, Raspberry Pi Ltd).

use crate::uapi::*;

/// Evaluates a piecewise-linear curve given as sorted `(x, y)` points (truncating, as
/// libpisp's `Pwl::Eval` result is assigned to an int). A curve of fewer than two points is flat
/// at its only point, or 0 when empty.
pub fn pwl_eval(points: &[(u32, u32)], x: u32) -> u32 {
    if points.len() < 2 {
        return points.first().map_or(0, |p| p.1);
    }
    let last = points.len() - 2;
    let mut span = 0;
    while span < last && x >= points[span + 1].0 {
        span += 1;
    }
    let (x0, y0) = (f64::from(points[span].0), f64::from(points[span].1));
    let (x1, y1) = (f64::from(points[span + 1].0), f64::from(points[span + 1].1));
    if x1 == x0 {
        return y1 as u32;
    }
    (y0 + (f64::from(x) - x0) * (y1 - y0) / (x1 - x0)) as u32
}

/// Builds the gamma block from a curve (`(x, y)` points, 16-bit scales): 64 entries at the
/// hardware's knots, each `slope << 16 | y`.
pub fn gamma_from_curve(points: &[(u32, u32)]) -> BeGammaConfig {
    const SLOPE_BITS: u32 = 14;
    let mut lut = [0u32; BE_GAMMA_LUT_SIZE];
    let mut last_y = 0u32;
    for i in 0..BE_GAMMA_LUT_SIZE as u32 {
        let x = if i < 32 {
            i * 512
        } else if i < 48 {
            (i - 32) * 1024 + 16384
        } else {
            ((i - 48) * 2048 + 32768).min(65535)
        };
        let mut y = pwl_eval(points, x).max(last_y);
        if i > 0 {
            let mut slope = y - last_y;
            if slope >= 1 << SLOPE_BITS {
                slope = (1 << SLOPE_BITS) - 1;
                y = last_y + slope;
            }
            lut[i as usize - 1] |= slope << 16;
        }
        lut[i as usize] = y;
        last_y = y;
    }
    BeGammaConfig { lut }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_straight_line_packs_values_and_slopes_at_the_hardware_knots() {
        let g = gamma_from_curve(&[(0, 0), (65535, 65535)]);
        assert_eq!(g.lut[1] & 0xffff, 512);
        assert_eq!(g.lut[32] & 0xffff, 16384);
        assert_eq!(g.lut[48] & 0xffff, 32768);
        // The last knot is x = 63488.
        assert_eq!(g.lut[63] & 0xffff, 63488);
        for i in 1..BE_GAMMA_LUT_SIZE {
            let (y0, y1) = (g.lut[i - 1] & 0xffff, g.lut[i] & 0xffff);
            assert!(y1 >= y0);
            assert_eq!(g.lut[i - 1] >> 16, y1 - y0, "slope at knot {}", i - 1);
        }
    }

    #[test]
    fn slopes_saturate_at_fourteen_bits() {
        let g = gamma_from_curve(&[(0, 0), (512, 65535), (65535, 65535)]);
        assert_eq!(g.lut[0] >> 16, (1 << 14) - 1);
        assert_eq!(g.lut[1] & 0xffff, (1 << 14) - 1);
    }

    #[test]
    fn short_curves_are_flat_and_do_not_panic() {
        assert_eq!(pwl_eval(&[], 100), 0);
        assert_eq!(pwl_eval(&[(10, 77)], 100), 77);
        assert_eq!(gamma_from_curve(&[]).lut, [0; BE_GAMMA_LUT_SIZE]);
    }
}
