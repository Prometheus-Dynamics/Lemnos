//! Deterministic helpers for the crate's tests: a small PRNG, uniform sphere
//! directions, and body-frame synthesis of a tilting board.

use crate::Vec3;
use crate::math::{self, sin_cos, sqrt};
use crate::quat::Quat;

/// xorshift32. Enough for test data; not for anything else.
pub(crate) struct Rng(u32);

impl Rng {
    pub(crate) fn new(seed: u32) -> Self {
        Rng(if seed == 0 { 0x9e37_79b9 } else { seed })
    }

    pub(crate) fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Uniform in `[-1, 1)`.
    pub(crate) fn signed(&mut self) -> f32 {
        2.0 * self.unit() - 1.0
    }
}

/// A uniformly distributed unit vector.
pub(crate) fn random_unit(rng: &mut Rng) -> Vec3 {
    let z = rng.signed();
    let phi = rng.unit() * 2.0 * core::f32::consts::PI;
    let r = sqrt(math::max(0.0, 1.0 - z * z));
    let (s, c) = sin_cos(phi);
    [r * c, r * s, z]
}

/// Body angular velocity (rad/s) of the attitude `Ry(pitch) * Rx(roll)`, for
/// the given roll and pitch rates. Derived from `ω = ṙ x̂ + ṗ · Rx(r)ᵀ ŷ`.
pub(crate) fn body_rates(roll: f32, roll_rate: f32, pitch_rate: f32) -> Vec3 {
    let qx = Quat::from_euler_rad(roll, 0.0, 0.0);
    let ty = qx.conj().rotate([0.0, pitch_rate, 0.0]);
    [roll_rate + ty[0], ty[1], ty[2]]
}

/// Gravity and field in the sensor frame for a board with attitude `q`.
pub(crate) fn sensor_accel(q: Quat, g: f32) -> Vec3 {
    q.conj().rotate([0.0, 0.0, g])
}

pub(crate) fn sensor_field(q: Quat, world: Vec3) -> Vec3 {
    q.conj().rotate(world)
}
