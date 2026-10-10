//! Unit quaternions (Hamilton, scalar first).

use crate::Vec3;
use crate::math::{self, asin, atan2, sin_cos, sqrt};

/// A quaternion `w + xi + yj + zk`. A rotation quaternion `q` maps a vector
/// from its source frame into its destination frame: `q.rotate(v)` is `q v q*`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Quat {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Quat {
    /// No rotation.
    pub const IDENTITY: Quat = Quat {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Builds the intrinsic ZYX rotation `Rz(yaw) * Ry(pitch) * Rx(roll)`
    /// from angles in degrees.
    pub fn from_euler_deg(roll: f32, pitch: f32, yaw: f32) -> Quat {
        let k = core::f32::consts::PI / 180.0;
        Quat::from_euler_rad(roll * k, pitch * k, yaw * k)
    }

    /// Like [`Quat::from_euler_deg`], in radians.
    pub fn from_euler_rad(roll: f32, pitch: f32, yaw: f32) -> Quat {
        let (sr, cr) = sin_cos(roll * 0.5);
        let (sp, cp) = sin_cos(pitch * 0.5);
        let (sy, cy) = sin_cos(yaw * 0.5);
        Quat {
            w: cr * cp * cy + sr * sp * sy,
            x: sr * cp * cy - cr * sp * sy,
            y: cr * sp * cy + sr * cp * sy,
            z: cr * cp * sy - sr * sp * cy,
        }
    }

    /// Hamilton product `self * o` (apply `o`, then `self`).
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn mul(self, o: Quat) -> Quat {
        Quat {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }

    /// Conjugate; the inverse for a unit quaternion.
    #[inline]
    pub fn conj(self) -> Quat {
        Quat {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    /// Rotates a vector: `q v q*`.
    #[inline]
    pub fn rotate(self, v: Vec3) -> Vec3 {
        let u = [self.x, self.y, self.z];
        let t = math::scale(math::cross(u, v), 2.0);
        let ut = math::cross(u, t);
        [
            v[0] + self.w * t[0] + ut[0],
            v[1] + self.w * t[1] + ut[1],
            v[2] + self.w * t[2] + ut[2],
        ]
    }

    /// Scales to unit length. A zero quaternion becomes the identity.
    #[inline]
    pub fn normalized(self) -> Quat {
        let n2 = self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z;
        if n2 <= 1e-20 || !n2.is_finite() {
            return Quat::IDENTITY;
        }
        let inv = 1.0 / sqrt(n2);
        Quat {
            w: self.w * inv,
            x: self.x * inv,
            y: self.y * inv,
            z: self.z * inv,
        }
    }

    /// Intrinsic ZYX Euler angles `(roll, pitch, yaw)` in radians, the inverse
    /// of [`Quat::from_euler_rad`]. Pitch is in `[-pi/2, pi/2]`.
    pub fn euler(self) -> (f32, f32, f32) {
        let Quat { w, x, y, z } = self;
        let roll = atan2(2.0 * (w * x + y * z), 1.0 - 2.0 * (x * x + y * y));
        let pitch = asin(2.0 * (w * y - z * x));
        let yaw = atan2(2.0 * (w * z + x * y), 1.0 - 2.0 * (y * y + z * z));
        (roll, pitch, yaw)
    }
}

impl Default for Quat {
    fn default() -> Self {
        Quat::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn multiply_rotate_and_euler_match_known_values() {
        // 90 degrees about z maps x to y.
        let qz = Quat::from_euler_deg(0.0, 0.0, 90.0);
        let v = qz.rotate([1.0, 0.0, 0.0]);
        assert!(close(v[0], 0.0, 1e-6) && close(v[1], 1.0, 1e-6) && close(v[2], 0.0, 1e-6));

        // 90 degrees about x maps y to z.
        let qx = Quat::from_euler_deg(90.0, 0.0, 0.0);
        let v = qx.rotate([0.0, 1.0, 0.0]);
        assert!(close(v[0], 0.0, 1e-6) && close(v[1], 0.0, 1e-6) && close(v[2], 1.0, 1e-6));

        // Composition: (qx * qz) applied once equals qz then qx.
        let both = qx.mul(qz);
        let a = both.rotate([1.0, 0.0, 0.0]);
        let b = qx.rotate(qz.rotate([1.0, 0.0, 0.0]));
        for i in 0..3 {
            assert!(close(a[i], b[i], 1e-6));
        }

        // Inverse.
        let p = both.mul(both.conj()).normalized();
        assert!(close(p.w, 1.0, 1e-6) && close(p.x, 0.0, 1e-6));

        // Euler round trip, away from gimbal lock.
        let q = Quat::from_euler_deg(20.0, -10.0, 35.0);
        let (r, p, y) = q.euler();
        assert!(close(r.to_degrees(), 20.0, 1e-3));
        assert!(close(p.to_degrees(), -10.0, 1e-3));
        assert!(close(y.to_degrees(), 35.0, 1e-3));

        // Closed form: roll 30 degrees is w = cos 15, x = sin 15.
        let q = Quat::from_euler_deg(30.0, 0.0, 0.0);
        assert!(close(q.w, (15.0_f32).to_radians().cos(), 1e-6));
        assert!(close(q.x, (15.0_f32).to_radians().sin(), 1e-6));
    }

    #[test]
    fn euler_handles_gimbal_lock_without_nan() {
        let q = Quat::from_euler_deg(0.0, 90.0, 0.0);
        let (r, p, y) = q.euler();
        assert!(r.is_finite() && y.is_finite());
        // asin is ill-conditioned at +-1: the f32 input rounding near 1 is
        // amplified to about 6e-4 rad, so the bound is looser here.
        assert!(close(p, core::f32::consts::FRAC_PI_2, 2e-3));
    }
}
