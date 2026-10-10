//! Small `f32` routines that `core` does not provide: square root, sine and
//! cosine, arctangent, and the linear solves the estimators need. Everything
//! is bounded in cost and branch-free apart from range reduction.

pub(crate) use core::f32::consts::{FRAC_2_PI, FRAC_PI_2, FRAC_PI_4, PI};
/// `tan(pi/8)`: the split point for the arctangent series.
const TAN_PI_8: f32 = 0.414_213_57;

#[inline]
pub(crate) fn abs(x: f32) -> f32 {
    if x < 0.0 { -x } else { x }
}

#[inline]
pub(crate) fn min(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

#[inline]
pub(crate) fn max(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

#[inline]
pub(crate) fn clamp(x: f32, lo: f32, hi: f32) -> f32 {
    max(lo, min(x, hi))
}

/// Nearest integer, halves away from zero. Saturates at the `i32` range.
#[inline]
pub(crate) fn round_i32(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// Square root by a bit-level first guess and four Newton steps. Returns 0
/// for zero and negative input (callers only pass non-negative sums).
#[inline]
pub(crate) fn sqrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut g = f32::from_bits((x.to_bits() >> 1) + 0x1fc0_0000);
    g = 0.5 * (g + x / g);
    g = 0.5 * (g + x / g);
    g = 0.5 * (g + x / g);
    0.5 * (g + x / g)
}

/// Sine and cosine. The argument is reduced to `[-pi/4, pi/4]` by quadrant,
/// then two Taylor series of degree 11 and 12 (error below 2e-9).
/// Cube root of a positive finite `x` (Newton from above, no `libm`).
/// Returns 0 for a non-positive input.
pub(crate) fn cbrt(x: f32) -> f32 {
    if x.partial_cmp(&0.0) != Some(core::cmp::Ordering::Greater) || !x.is_finite() {
        return 0.0;
    }
    let mut y = max(x, 1.0);
    for _ in 0..100 {
        y = (2.0 * y + x / (y * y)) / 3.0;
    }
    y
}

pub(crate) fn sin_cos(x: f32) -> (f32, f32) {
    let k = round_i32(x * FRAC_2_PI);
    let r = x - k as f32 * FRAC_PI_2;
    let r2 = r * r;
    // sin(r) = r - r^3/3! + r^5/5! - ...  (Horner in r^2)
    let s = r
        * (1.0
            + r2 * (-1.0 / 6.0
                + r2 * (1.0 / 120.0 + r2 * (-1.0 / 5040.0 + r2 * (1.0 / 362_880.0)))));
    // cos(r) = 1 - r^2/2! + r^4/4! - ...
    let c = 1.0
        + r2 * (-0.5
            + r2 * (1.0 / 24.0
                + r2 * (-1.0 / 720.0 + r2 * (1.0 / 40_320.0 + r2 * (-1.0 / 3_628_800.0)))));
    match k & 3 {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

/// Arctangent series for `|t| <= tan(pi/8)`, 11 terms (error below 1e-10).
#[inline]
fn atan_core(t: f32) -> f32 {
    let t2 = t * t;
    let mut acc = 0.0;
    // Horner over the odd terms t * (1 - t^2/3 + t^4/5 - ...), highest first.
    let mut k = 21;
    while k >= 1 {
        let sign = if (k / 2) % 2 == 0 { 1.0 } else { -1.0 };
        acc = acc * t2 + sign / k as f32;
        k -= 2;
    }
    t * acc
}

/// Arctangent, accurate to about 1e-6 radians over all of `f32`.
pub(crate) fn atan(x: f32) -> f32 {
    if x < 0.0 {
        return -atan(-x);
    }
    if x > 1.0 {
        return FRAC_PI_2 - atan(1.0 / x);
    }
    if x > TAN_PI_8 {
        return FRAC_PI_4 + atan_core((x - 1.0) / (x + 1.0));
    }
    atan_core(x)
}

/// Two-argument arctangent in `(-pi, pi]`, like `f32::atan2`.
pub(crate) fn atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 {
        if y == 0.0 {
            return 0.0;
        }
        return if y > 0.0 { FRAC_PI_2 } else { -FRAC_PI_2 };
    }
    if x > 0.0 {
        atan(y / x)
    } else if y >= 0.0 {
        atan(y / x) + PI
    } else {
        atan(y / x) - PI
    }
}

/// Arcsine for `x` in `[-1, 1]` (clamped outside).
pub(crate) fn asin(x: f32) -> f32 {
    let x = clamp(x, -1.0, 1.0);
    atan2(x, sqrt(1.0 - x * x))
}

#[inline]
pub(crate) fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub(crate) fn norm(a: [f32; 3]) -> f32 {
    sqrt(dot(a, a))
}

#[inline]
pub(crate) fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub(crate) fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub(crate) fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Wraps an angle into `[-pi, pi]`.
pub(crate) fn wrap_pi(mut a: f32) -> f32 {
    // Angles in this crate are a few turns at most; bounded loops are enough.
    let mut n = 0;
    while a > PI && n < 8 {
        a -= 2.0 * PI;
        n += 1;
    }
    while a < -PI && n < 8 {
        a += 2.0 * PI;
        n += 1;
    }
    a
}

/// Solves the symmetric positive definite system `a x = b` (9x9) in place by
/// Gaussian elimination with partial pivoting. Returns `None` when a pivot is
/// not finite or is negligible against the matrix scale.
#[allow(clippy::needless_range_loop)]
pub(crate) fn solve9(a: &mut [[f32; 9]; 9], b: &mut [f32; 9], scale_ref: f32) -> Option<[f32; 9]> {
    const N: usize = 9;
    let tiny = scale_ref * 1e-9;
    for col in 0..N {
        let mut piv = col;
        for row in col + 1..N {
            if abs(a[row][col]) > abs(a[piv][col]) {
                piv = row;
            }
        }
        let pv = abs(a[piv][col]);
        if pv.is_nan() || pv <= tiny {
            return None;
        }
        if piv != col {
            a.swap(piv, col);
            b.swap(piv, col);
        }
        let p = a[col][col];
        for row in col + 1..N {
            let f = a[row][col] / p;
            if f != 0.0 {
                for k in col..N {
                    a[row][k] -= f * a[col][k];
                }
                b[row] -= f * b[col];
            }
        }
    }
    let mut x = [0.0_f32; N];
    for row in (0..N).rev() {
        let mut s = b[row];
        for k in row + 1..N {
            s -= a[row][k] * x[k];
        }
        x[row] = s / a[row][row];
    }
    if x.iter().all(|v| v.is_finite()) {
        Some(x)
    } else {
        None
    }
}

/// Solves the 3x3 system `m x = b`. `None` when `m` is singular.
pub(crate) fn solve3(m: [[f32; 3]; 3], b: [f32; 3]) -> Option<[f32; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if abs(det) <= 1e-30 || !det.is_finite() {
        return None;
    }
    let inv = 1.0 / det;
    let mut out = [0.0_f32; 3];
    for (i, o) in out.iter_mut().enumerate() {
        // Cramer's rule: replace column i with b.
        let mut t = m;
        for r in 0..3 {
            t[r][i] = b[r];
        }
        let d = t[0][0] * (t[1][1] * t[2][2] - t[1][2] * t[2][1])
            - t[0][1] * (t[1][0] * t[2][2] - t[1][2] * t[2][0])
            + t[0][2] * (t[1][0] * t[2][1] - t[1][1] * t[2][0]);
        *o = d * inv;
    }
    Some(out)
}

/// Eigendecomposition of a symmetric 3x3 matrix by cyclic Jacobi rotations.
/// Returns eigenvalues and the matrix whose columns are the eigenvectors.
#[allow(clippy::needless_range_loop)]
pub(crate) fn eig3_sym(mut a: [[f32; 3]; 3]) -> ([f32; 3], [[f32; 3]; 3]) {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..12 {
        let off = abs(a[0][1]) + abs(a[0][2]) + abs(a[1][2]);
        let diag = abs(a[0][0]) + abs(a[1][1]) + abs(a[2][2]);
        if off <= 1e-9 * diag || off == 0.0 {
            break;
        }
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            if a[p][q] == 0.0 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            // t = sign(theta) / (|theta| + sqrt(theta^2 + 1)), the smaller root.
            let t = {
                let s = if theta >= 0.0 { 1.0 } else { -1.0 };
                s / (abs(theta) + sqrt(theta * theta + 1.0))
            };
            let c = 1.0 / sqrt(t * t + 1.0);
            let s = t * c;
            let app = a[p][p];
            let aqq = a[q][q];
            let apq = a[p][q];
            a[p][p] = app - t * apq;
            a[q][q] = aqq + t * apq;
            a[p][q] = 0.0;
            a[q][p] = 0.0;
            for r in 0..3 {
                if r != p && r != q {
                    let arp = a[r][p];
                    let arq = a[r][q];
                    a[r][p] = c * arp - s * arq;
                    a[p][r] = a[r][p];
                    a[r][q] = s * arp + c * arq;
                    a[q][r] = a[r][q];
                }
            }
            for row in v.iter_mut() {
                let vp = row[p];
                let vq = row[q];
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;

    extern crate std;

    fn close(got: f32, want: f32) -> bool {
        // Absolute 1e-5, or relative for large magnitudes.
        (got - want).abs() <= 1e-5 * (1.0 + want.abs())
    }

    #[test]
    fn sqrt_matches_std() {
        let mut x = 1e-6_f32;
        while x < 1e6 {
            assert!(close(sqrt(x), x.sqrt()), "sqrt({x})");
            x *= 1.37;
        }
        assert_eq!(sqrt(0.0), 0.0);
        assert_eq!(sqrt(-4.0), 0.0);
    }

    #[test]
    fn sin_cos_match_std() {
        let mut x = -20.0_f32;
        while x < 20.0 {
            let (s, c) = sin_cos(x);
            assert!(close(s, x.sin()), "sin({x}) {s} vs {}", x.sin());
            assert!(close(c, x.cos()), "cos({x}) {c} vs {}", x.cos());
            x += 0.0137;
        }
    }

    #[test]
    fn atan2_and_asin_match_std() {
        let mut t = -50.0_f32;
        while t < 50.0 {
            assert!(close(atan(t), t.atan()), "atan({t})");
            t += 0.0331;
        }
        let angles = [-3.1, -2.0, -1.0, -0.3, 0.0, 0.3, 1.0, 2.0, 3.1];
        for &y in &angles {
            for &x in &angles {
                let (yy, xx) = (y as f32, x as f32);
                assert!(close(atan2(yy, xx), yy.atan2(xx)), "atan2({yy}, {xx})");
            }
        }
        assert_eq!(atan2(0.0, 0.0), 0.0);
        let mut s = -1.0_f32;
        while s <= 1.0 {
            assert!(close(asin(s), s.asin()), "asin({s})");
            s += 0.0123;
        }
        // Clamped outside [-1, 1].
        assert!(close(asin(1.5), core::f32::consts::FRAC_PI_2));
    }

    #[test]
    fn vector_helpers() {
        assert_eq!(cross([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]);
        assert_eq!(dot([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]), 32.0);
        assert!(close(
            wrap_pi(3.0 * core::f32::consts::PI),
            core::f32::consts::PI
        ));
    }

    #[test]
    fn solve9_and_eig3_agree_with_known_systems() {
        // Diagonal system: solution is b / d.
        let mut a = [[0.0_f32; 9]; 9];
        for (i, row) in a.iter_mut().enumerate() {
            row[i] = (i + 1) as f32;
        }
        let mut b = [1.0_f32; 9];
        let x = solve9(&mut a, &mut b, 9.0).expect("solvable");
        for (i, xi) in x.iter().enumerate() {
            assert!(close(*xi, 1.0 / (i + 1) as f32));
        }
        // Singular: no solution.
        let mut z = [[0.0_f32; 9]; 9];
        let mut zb = [0.0_f32; 9];
        assert!(solve9(&mut z, &mut zb, 1.0).is_none());

        let (lam, v) = eig3_sym([[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 5.0]]);
        let mut sorted = lam;
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(close(sorted[0], 2.0) && close(sorted[1], 3.0) && close(sorted[2], 5.0));
        // Orthonormal eigenvectors.
        for i in 0..3 {
            let n: f32 = (0..3).map(|k| v[k][i] * v[k][i]).sum();
            assert!(close(n, 1.0));
        }
    }
}
