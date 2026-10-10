//! Ellipsoid fit for accelerometer and magnetometer calibration.
//!
//! A sensor's samples on a sphere of radius `r̄`, distorted by offset,
//! per-axis scale and cross-axis (soft iron), lie on the ellipsoid
//! `(x - c)ᵀ Q (x - c) = 1`. The correction `r̄ · Q^½ (x - c)` maps them back
//! onto that sphere. [`EllipsoidFit`] estimates `c` and `Q` by a linear least
//! squares fit of the quadratic `xᵀAx + bᵀx = 1`.

use crate::Vec3;
use crate::math::{self, eig3_sym, solve3, solve9, sqrt};

/// Number of recent samples kept for the residual check.
pub const RESIDUAL_SAMPLES: usize = 64;

/// Number of direction cells (the 3x3x3 cube's cells other than the centre).
pub const CELLS: usize = 26;

/// Component threshold for the direction cells: a unit-direction component
/// beyond `±CELL_THRESHOLD` counts as that side of the cell.
const CELL_THRESHOLD: f32 = 0.4;

/// Ridge term, relative to the mean diagonal of the normal matrix.
const RIDGE: f32 = 1e-5;

/// Smallest accepted ratio of the smallest to the largest axis eigenvalue.
const MIN_AXIS_RATIO: f32 = 1e-4;

/// A calibration ellipsoid: `corrected = radius · matrix · (raw - center)`.
///
/// `matrix` is `Q^½` (in the raw units of the sensor), so `corrected` lies on
/// a sphere of radius `radius` in the same units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ellipsoid {
    pub center: Vec3,
    pub matrix: [[f32; 3]; 3],
    pub radius: f32,
}

impl Ellipsoid {
    /// No correction: `apply(x) == x`.
    pub const IDENTITY: Ellipsoid = Ellipsoid {
        center: [0.0; 3],
        matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        radius: 1.0,
    };

    /// Applies the correction to a raw sample.
    #[inline]
    pub fn apply(&self, raw: Vec3) -> Vec3 {
        let d = math::sub(raw, self.center);
        let m = self.matrix;
        let r = self.radius;
        [
            r * (m[0][0] * d[0] + m[0][1] * d[1] + m[0][2] * d[2]),
            r * (m[1][0] * d[0] + m[1][1] * d[1] + m[1][2] * d[2]),
            r * (m[2][0] * d[0] + m[2][1] * d[1] + m[2][2] * d[2]),
        ]
    }
}

/// The result of a successful fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub ellipsoid: Ellipsoid,
    /// RMS of `|corrected| - radius` over the recent samples, relative to the radius.
    pub residual: f32,
    /// Share of the 26 direction cells holding at least `min_per_cell` samples.
    pub coverage: f32,
    /// Samples the fit was accumulated from.
    pub samples: u32,
}

/// Online ellipsoid fit with a forgetting factor.
///
/// Samples are normalised by `reference` for conditioning, so the fit works
/// the same for m/s² and µT. The fitted sphere radius is `reference`.
#[derive(Clone, Debug)]
pub struct EllipsoidFit {
    reference: f32,
    forgetting: f32,
    min_per_cell: u16,
    samples: u32,
    normal: [[f32; 9]; 9],
    rhs: [f32; 9],
    cells: [f32; CELLS + 1],
    mean: Vec3,
    centre: Vec3,
    centre_set: bool,
    ring: [Vec3; RESIDUAL_SAMPLES],
    ring_len: usize,
    ring_next: usize,
}

impl EllipsoidFit {
    /// `reference` is the sphere radius (and normalisation scale) in sensor
    /// units. `forgetting` in `(0, 1]` decays old samples per update (1 keeps
    /// all). A direction cell counts as covered at `min_per_cell` samples.
    pub fn new(reference: f32, forgetting: f32, min_per_cell: u16) -> Self {
        EllipsoidFit {
            reference: if reference > 1e-9 { reference } else { 1.0 },
            forgetting: if forgetting > 0.0 && forgetting <= 1.0 {
                forgetting
            } else {
                1.0
            },
            min_per_cell,
            samples: 0,
            normal: [[0.0; 9]; 9],
            rhs: [0.0; 9],
            cells: [0.0; CELLS + 1],
            mean: [0.0; 3],
            centre: [0.0; 3],
            centre_set: false,
            ring: [[0.0; 3]; RESIDUAL_SAMPLES],
            ring_len: 0,
            ring_next: 0,
        }
    }

    /// Adds one raw sample (sensor units).
    pub fn add(&mut self, sample: Vec3) {
        let inv = 1.0 / self.reference;
        let u = math::scale(sample, inv);
        let phi = [
            u[0] * u[0],
            u[1] * u[1],
            u[2] * u[2],
            u[0] * u[1],
            u[1] * u[2],
            u[0] * u[2],
            u[0],
            u[1],
            u[2],
        ];

        let lam = self.forgetting;
        if lam < 1.0 {
            for row in self.normal.iter_mut() {
                for v in row.iter_mut() {
                    *v *= lam;
                }
            }
            for v in self.rhs.iter_mut() {
                *v *= lam;
            }
            for v in self.cells.iter_mut() {
                *v *= lam;
            }
        }
        for (i, row) in self.normal.iter_mut().enumerate() {
            let pi = phi[i];
            for (j, v) in row.iter_mut().enumerate() {
                *v += pi * phi[j];
            }
            self.rhs[i] += pi;
        }

        // Running mean: the centre estimate until a fit provides one.
        let n = core::cmp::min(self.samples + 1, 1000) as f32;
        for (m, x) in self.mean.iter_mut().zip(sample.iter()) {
            *m += (*x - *m) / n;
        }

        let reference_centre = if self.centre_set {
            self.centre
        } else {
            self.mean
        };
        let d = math::sub(sample, reference_centre);
        let len = math::norm(d);
        if len > 1e-9 {
            let idx = cell_index(math::scale(d, 1.0 / len));
            if idx != 13 {
                self.cells[idx] += 1.0;
            }
        }

        self.ring[self.ring_next] = sample;
        self.ring_next = (self.ring_next + 1) % RESIDUAL_SAMPLES;
        if self.ring_len < RESIDUAL_SAMPLES {
            self.ring_len += 1;
        }
        self.samples = self.samples.saturating_add(1);
    }

    /// Samples added since the last [`EllipsoidFit::clear`].
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// The sphere radius (and normalisation scale) this fit was built with.
    pub fn reference(&self) -> f32 {
        self.reference
    }

    /// Share of the 26 direction cells holding at least `min_per_cell` samples.
    pub fn coverage(&self) -> f32 {
        let thr = self.min_per_cell as f32;
        let mut n = 0u32;
        for (i, c) in self.cells.iter().enumerate() {
            if i != 13 && *c >= thr && *c > 0.0 {
                n += 1;
            }
        }
        n as f32 / CELLS as f32
    }

    /// Uses `centre` for the direction cells from now on (the caller passes
    /// the centre of its latest fit).
    pub fn set_centre(&mut self, centre: Vec3) {
        self.centre = centre;
        self.centre_set = true;
    }

    /// Forgets every sample.
    pub fn clear(&mut self) {
        let reference = self.reference;
        let forgetting = self.forgetting;
        let min_per_cell = self.min_per_cell;
        *self = EllipsoidFit::new(reference, forgetting, min_per_cell);
    }

    /// Solves the fit. `None` when there are too few samples, the system is
    /// degenerate (for example all samples on a plane), or the result is not
    /// an ellipsoid.
    pub fn fit(&self) -> Option<Fit> {
        if self.samples < 9 {
            return None;
        }
        let mut a = self.normal;
        let trace: f32 = (0..9).map(|i| a[i][i]).sum();
        if trace <= 0.0 || !trace.is_finite() {
            return None;
        }
        let ridge = RIDGE * trace / 9.0;
        for (i, row) in a.iter_mut().enumerate() {
            row[i] += ridge;
        }
        let mut b = self.rhs;
        let t = solve9(&mut a, &mut b, trace)?;

        // Quadratic and linear parts, in normalised units.
        let au = [
            [t[0], 0.5 * t[3], 0.5 * t[5]],
            [0.5 * t[3], t[1], 0.5 * t[4]],
            [0.5 * t[5], 0.5 * t[4], t[2]],
        ];
        let bu = [t[6], t[7], t[8]];
        // Centre: Au c = -b / 2.
        let c_u = solve3(au, [-0.5 * bu[0], -0.5 * bu[1], -0.5 * bu[2]])?;
        let auc = mat_vec(au, c_u);
        let k = 1.0 + math::dot(c_u, auc);
        if k <= 1e-12 || !k.is_finite() {
            return None;
        }
        let inv_k = 1.0 / k;
        let q = [
            [au[0][0] * inv_k, au[0][1] * inv_k, au[0][2] * inv_k],
            [au[1][0] * inv_k, au[1][1] * inv_k, au[1][2] * inv_k],
            [au[2][0] * inv_k, au[2][1] * inv_k, au[2][2] * inv_k],
        ];
        let (lam, v) = eig3_sym(q);
        let lmax = math::max(math::max(lam[0], lam[1]), lam[2]);
        let lmin = math::min(math::min(lam[0], lam[1]), lam[2]);
        if !lmin.is_finite() || !lmax.is_finite() || lmin <= 0.0 || lmin / lmax < MIN_AXIS_RATIO {
            return None;
        }
        let s = [sqrt(lam[0]), sqrt(lam[1]), sqrt(lam[2])];
        // M = V diag(s) Vᵀ / reference  (Q_raw^½ with Q_raw = Q_u / reference²).
        let inv_ref = 1.0 / self.reference;
        let mut matrix = [[0.0_f32; 3]; 3];
        for (i, row) in matrix.iter_mut().enumerate() {
            for (j, m) in row.iter_mut().enumerate() {
                let mut acc = 0.0;
                for (kk, sk) in s.iter().enumerate() {
                    acc += v[i][kk] * sk * v[j][kk];
                }
                *m = acc * inv_ref;
            }
        }
        // The sphere's radius in the sensor's own units: the matrix maps a
        // sample on that sphere to a unit vector, so for a sphere of radius B
        // it is I / B and det = B^-3. The radius is fitted, not pinned: the
        // magnetometer's calibrated magnitude is the local field.
        let det = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
            - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
            + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
        if det.partial_cmp(&0.0) != Some(core::cmp::Ordering::Greater) || !det.is_finite() {
            return None;
        }
        let radius = math::cbrt(1.0 / det);
        let ellipsoid = Ellipsoid {
            center: math::scale(c_u, self.reference),
            matrix,
            radius,
        };
        if !ellipsoid.center.iter().all(|x| x.is_finite())
            || !matrix.iter().flatten().all(|x| x.is_finite())
        {
            return None;
        }

        let mut sq = 0.0_f32;
        for x in self.ring.iter().take(self.ring_len) {
            let e = math::norm(ellipsoid.apply(*x)) - radius;
            sq += e * e;
        }
        let residual = sqrt(sq / self.ring_len as f32) / radius;
        Some(Fit {
            ellipsoid,
            residual,
            coverage: self.coverage(),
            samples: self.samples,
        })
    }
}

/// Maps a unit direction to one of the 26 cells (index 13 is the centre,
/// never reached by a unit vector with this threshold).
#[inline]
fn cell_index(d: Vec3) -> usize {
    let part = |v: f32| {
        if v > CELL_THRESHOLD {
            2
        } else if v < -CELL_THRESHOLD {
            0
        } else {
            1
        }
    };
    part(d[0]) + 3 * part(d[1]) + 9 * part(d[2])
}

fn mat_vec(m: [[f32; 3]; 3], v: Vec3) -> Vec3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Rng, random_unit};

    extern crate std;

    /// Hard iron (10, -5, 20), soft iron diag (1.1, 0.95, 1.05) with 0.05
    /// off-diagonal: `raw = S·(B) + offset` with `|B| = 50`.
    fn mag_raw(b: Vec3) -> Vec3 {
        // Unit determinant (the volume-preserving convention the fitted radius
        // assumes): the raw matrix's determinant is 1.08975, cube root 1.02914.
        let k = 1.0 / 1.02914;
        let s = [
            [1.1 * k, 0.05 * k, 0.05 * k],
            [0.05 * k, 0.95 * k, 0.05 * k],
            [0.05 * k, 0.05 * k, 1.05 * k],
        ];
        let mut out = [0.0; 3];
        for i in 0..3 {
            out[i] = s[i][0] * b[0] + s[i][1] * b[1] + s[i][2] * b[2];
        }
        [out[0] + 10.0, out[1] - 5.0, out[2] + 20.0]
    }

    #[test]
    fn cell_index_reaches_all_26_cells() {
        let mut rng = Rng::new(7);
        let mut seen = [false; 27];
        for _ in 0..20_000 {
            let d = random_unit(&mut rng);
            seen[cell_index(d)] = true;
        }
        assert!(!seen[13], "the centre cell must be unreachable");
        assert_eq!(seen.iter().filter(|s| **s).count(), 26);
    }

    #[test]
    fn mag_partial_cone_has_low_coverage_and_full_sphere_recovers() {
        let mut fit = EllipsoidFit::new(50.0, 0.999, 4);
        let mut rng = Rng::new(11);
        // Cone: within 60 degrees of +Z.
        // The calibrator refreshes the cell centre from its fits; do the same.
        for k in 0..1000 {
            let d = loop {
                let d = random_unit(&mut rng);
                if d[2] >= 0.5 {
                    break d;
                }
            };
            fit.add(mag_raw(math::scale(d, 50.0)));
            if k % 64 == 63
                && let Some(f) = fit.fit()
            {
                fit.set_centre(f.ellipsoid.center);
            }
        }
        let cone_cov = fit.coverage();
        assert!(cone_cov < 0.6, "cone coverage {cone_cov}");

        for _ in 0..3000 {
            let d = random_unit(&mut rng);
            fit.add(mag_raw(math::scale(d, 50.0)));
        }
        let full = fit.coverage();
        assert!(full > 0.9, "full coverage {full}");
        let f = fit.fit().expect("full sphere fits");
        // Offset within 2 % of the field magnitude.
        let c = f.ellipsoid.center;
        let err = math::norm(math::sub(c, [10.0, -5.0, 20.0]));
        assert!(err < 0.02 * 50.0, "centre error {err} uT");
        // Corrected magnitude within 2 % of 50 uT for fresh samples.
        for _ in 0..50 {
            let d = random_unit(&mut rng);
            let corrected = f.ellipsoid.apply(mag_raw(math::scale(d, 50.0)));
            let m = math::norm(corrected);
            assert!((m - 50.0).abs() < 0.02 * 50.0, "magnitude {m}");
        }
        assert!(f.residual < 0.005, "residual {}", f.residual);
    }

    #[test]
    fn accel_sphere_recovers_offset_and_scale_from_faces_and_tumbling() {
        let g = crate::STANDARD_GRAVITY;
        let off = [0.3, -0.2, 0.1];
        let scale = [1.02, 0.98, 1.01];
        let raw = |a: Vec3| {
            [
                scale[0] * a[0] + off[0],
                scale[1] * a[1] + off[1],
                scale[2] * a[2] + off[2],
            ]
        };
        let mut fit = EllipsoidFit::new(g, 0.9995, 4);
        // Six faces, then tumbling.
        for axis in 0..3 {
            for sign in [-1.0_f32, 1.0] {
                let mut a = [0.0; 3];
                a[axis] = sign * g;
                for _ in 0..300 {
                    fit.add(raw(a));
                }
            }
        }
        let mut rng = Rng::new(3);
        for _ in 0..2000 {
            let d = random_unit(&mut rng);
            fit.add(raw(math::scale(d, g)));
        }
        let f = fit.fit().expect("accel fit");
        assert!(f.coverage > 0.6, "coverage {}", f.coverage);
        for _ in 0..50 {
            let d = random_unit(&mut rng);
            let m = math::norm(f.ellipsoid.apply(raw(math::scale(d, g))));
            assert!((m - g).abs() < 0.01 * g, "|a| {m}");
        }
    }

    #[test]
    fn planar_samples_return_none() {
        let mut fit = EllipsoidFit::new(50.0, 1.0, 2);
        let mut rng = Rng::new(5);
        for _ in 0..500 {
            let d = random_unit(&mut rng);
            fit.add([d[0] * 40.0, d[1] * 40.0, 0.0]);
        }
        assert!(fit.fit().is_none());
    }

    #[test]
    fn too_few_samples_return_none() {
        let mut fit = EllipsoidFit::new(50.0, 1.0, 2);
        for i in 0..5 {
            fit.add([i as f32, 1.0, 2.0]);
        }
        assert!(fit.fit().is_none());
    }

    #[test]
    fn identity_apply_is_identity() {
        let x = [1.0, -2.0, 3.0];
        assert_eq!(Ellipsoid::IDENTITY.apply(x), x);
    }
}
