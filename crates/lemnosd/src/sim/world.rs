//! The simulated board's rigid-body motion, and what its inertial sensors and
//! magnetometer see of it. The world frame is z up with x toward magnetic
//! north (the frame `lemnos-fusion` uses); the body frame is the sensor frame.

use super::script::{Script, Segment};

/// Standard gravity, m/s².
pub const GRAVITY: f64 = 9.80665;

/// A unit quaternion `w + xi + yj + zk`, the body-to-world rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub w: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Quat {
    pub const IDENTITY: Self = Self {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Intrinsic yaw (z), pitch (y), roll (x), applied in that order.
    pub fn from_euler_deg(roll: f64, pitch: f64, yaw: f64) -> Self {
        let (r, p, y) = (
            roll.to_radians() / 2.0,
            pitch.to_radians() / 2.0,
            yaw.to_radians() / 2.0,
        );
        let (sr, cr, sp, cp, sy, cy) = (r.sin(), r.cos(), p.sin(), p.cos(), y.sin(), y.cos());
        Self {
            w: cr * cp * cy + sr * sp * sy,
            x: sr * cp * cy - cr * sp * sy,
            y: cr * sp * cy + sr * cp * sy,
            z: cr * cp * sy - sr * sp * cy,
        }
    }

    pub fn mul(self, o: Self) -> Self {
        Self {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }

    pub fn conj(self) -> Self {
        Self {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    pub fn normalized(self) -> Self {
        let n = (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        Self {
            w: self.w / n,
            x: self.x / n,
            y: self.y / n,
            z: self.z / n,
        }
    }

    /// Rotates `v` by this (body-to-world) rotation.
    pub fn rotate(self, v: [f64; 3]) -> [f64; 3] {
        let u = [self.x, self.y, self.z];
        let t = cross(u, v).map(|c| 2.0 * c);
        let ut = cross(u, t);
        [
            v[0] + self.w * t[0] + ut[0],
            v[1] + self.w * t[1] + ut[1],
            v[2] + self.w * t[2] + ut[2],
        ]
    }

    /// Rotates `v` from the world frame into the body frame.
    pub fn rotate_inverse(self, v: [f64; 3]) -> [f64; 3] {
        self.conj().rotate(v)
    }

    /// Spherical linear interpolation, `t` in 0 to 1, the shorter way.
    pub fn slerp(self, to: Self, t: f64) -> Self {
        let mut dot = self.w * to.w + self.x * to.x + self.y * to.y + self.z * to.z;
        let to = if dot < 0.0 {
            dot = -dot;
            Self {
                w: -to.w,
                x: -to.x,
                y: -to.y,
                z: -to.z,
            }
        } else {
            to
        };
        if dot > 0.9995 {
            let lerp = Self {
                w: self.w + (to.w - self.w) * t,
                x: self.x + (to.x - self.x) * t,
                y: self.y + (to.y - self.y) * t,
                z: self.z + (to.z - self.z) * t,
            };
            return lerp.normalized();
        }
        let theta = dot.acos();
        let s0 = (theta * (1.0 - t)).sin() / theta.sin();
        let s1 = (theta * t).sin() / theta.sin();
        Self {
            w: self.w * s0 + to.w * s1,
            x: self.x * s0 + to.x * s1,
            y: self.y * s0 + to.y * s1,
            z: self.z * s0 + to.z * s1,
        }
    }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// What the sensors see at one instant, in the body frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pose {
    /// Specific force (what an accelerometer reads), m/s².
    pub accel: [f64; 3],
    /// Angular rate, rad/s.
    pub gyro: [f64; 3],
    /// Magnetic field, µT.
    pub field_ut: [f64; 3],
}

/// The scripted motion, evaluated at simulated times.
#[derive(Debug, Clone)]
pub struct World {
    /// The motion: each part with its start and end, in simulated seconds.
    parts: Vec<(f64, f64, Segment)>,
    total: f64,
    repeat: bool,
    field_ut: [f64; 3],
    attitude: Quat,
    /// The part the attitude was last advanced in, and the attitude it began at.
    current: Option<usize>,
    part_start_attitude: Quat,
    last_t: f64,
}

impl World {
    pub fn new(script: &Script) -> Self {
        let mut start = 0.0;
        let parts = script
            .motion
            .iter()
            .map(|segment| {
                let end = start + segment.seconds();
                let part = (start, end, segment.clone());
                start = end;
                part
            })
            .collect();
        Self {
            parts,
            total: start,
            repeat: script.repeat,
            field_ut: script.mag.field_ut,
            attitude: Quat::IDENTITY,
            current: None,
            part_start_attitude: Quat::IDENTITY,
            last_t: 0.0,
        }
    }

    /// The body-to-world attitude at the last [`advance`](Self::advance).
    #[cfg(test)]
    pub fn attitude(&self) -> Quat {
        self.attitude
    }

    /// Advances the motion to simulated time `t` and returns the sensors' view.
    /// Times must not go backwards; a repeated or earlier time is a still.
    pub fn advance(&mut self, t: f64) -> Pose {
        let dt = (t - self.last_t).max(0.0);
        self.last_t = t;
        let local = if self.repeat && self.total > 0.0 {
            t.rem_euclid(self.total)
        } else {
            t.min(self.total)
        };
        // Past the end, a final attitude part holds its target; other parts
        // stop, and the board keeps the attitude it reached.
        let index = self
            .parts
            .iter()
            .position(|(start, end, _)| local >= *start && local < *end)
            .or_else(|| match self.parts.last() {
                Some((_, _, Segment::Attitude { .. })) if local >= self.total => {
                    Some(self.parts.len() - 1)
                }
                _ => None,
            });
        let mut linear = [0.0; 3];
        let mut rate = [0.0; 3];
        match index {
            Some(i) => {
                if self.current != Some(i) {
                    self.current = Some(i);
                    self.part_start_attitude = self.attitude;
                }
                let (start, end, segment) = self.parts[i].clone();
                let into = (local - start).min(end - start);
                match segment {
                    Segment::Still { .. } => {}
                    Segment::Rotate { rate_dps, .. } => {
                        rate = rate_dps.map(f64::to_radians);
                        let w = rate;
                        let n = norm(w);
                        if n > 0.0 && dt > 0.0 {
                            // q' = q ⊗ (0, ω): one step as the rotation by |ω|·dt about ω.
                            let angle = n * dt;
                            let axis = w.map(|c| c / n);
                            let half = angle / 2.0;
                            let step = Quat {
                                w: half.cos(),
                                x: axis[0] * half.sin(),
                                y: axis[1] * half.sin(),
                                z: axis[2] * half.sin(),
                            };
                            self.attitude = self.attitude.mul(step).normalized();
                        }
                    }
                    Segment::Shake { amplitude, hz, .. } => {
                        let phase = std::f64::consts::TAU * hz * into;
                        linear[0] = amplitude * phase.sin();
                    }
                    Segment::Attitude {
                        seconds,
                        roll_deg,
                        pitch_deg,
                        yaw_deg,
                    } => {
                        let target = Quat::from_euler_deg(roll_deg, pitch_deg, yaw_deg);
                        let fraction = (into / seconds).clamp(0.0, 1.0);
                        let before = self.attitude;
                        self.attitude = self.part_start_attitude.slerp(target, fraction);
                        if dt > 0.0 {
                            rate = body_rate(before, self.attitude, dt);
                        }
                    }
                }
            }
            None => {
                self.current = None;
                self.part_start_attitude = self.attitude;
            }
        }
        // Specific force: the body accelerates by `linear` and reads it minus
        // gravity, so a level board at rest reads +g on z.
        let world_force = [linear[0], linear[1], linear[2] + GRAVITY];
        Pose {
            accel: self.attitude.rotate_inverse(world_force),
            gyro: rate,
            field_ut: self.attitude.rotate_inverse(self.field_ut),
        }
    }
}

/// The body rate that turns `from` into `to` in `dt` seconds.
fn body_rate(from: Quat, to: Quat, dt: f64) -> [f64; 3] {
    let d = from.conj().mul(to);
    let sign = if d.w < 0.0 { -1.0 } else { 1.0 };
    let v = [d.x * sign, d.y * sign, d.z * sign];
    v.map(|c| 2.0 * c / dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3], tol: f64) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() <= tol, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn euler_rotates_vectors_as_expected() {
        // A 90° yaw turns body x to world y.
        let q = Quat::from_euler_deg(0.0, 0.0, 90.0);
        close(q.rotate([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0], 1e-12);
        // A 90° roll turns body y to world z.
        let q = Quat::from_euler_deg(90.0, 0.0, 0.0);
        close(q.rotate([0.0, 1.0, 0.0]), [0.0, 0.0, 1.0], 1e-12);
        // Inverse rotation undoes it.
        close(q.rotate_inverse([0.0, 0.0, 1.0]), [0.0, 1.0, 0.0], 1e-12);
    }

    #[test]
    fn still_board_reads_gravity_on_z() {
        let script = Script::from_toml_str(
            "format = \"lemnos.sim\"\nschema_version = 1\n[[motion]]\nkind = \"still\"\nseconds = 1.0\n",
        )
        .unwrap();
        let mut world = World::new(&script);
        let pose = world.advance(0.5);
        close(pose.accel, [0.0, 0.0, GRAVITY], 1e-9);
        close(pose.gyro, [0.0; 3], 1e-12);
        close(pose.field_ut, [20.0, 0.0, -40.0], 1e-9);
    }

    #[test]
    fn rotation_integrates_the_commanded_rate() {
        let script = Script::from_toml_str(
            "format = \"lemnos.sim\"\nschema_version = 1\n[[motion]]\nkind = \"rotate\"\nseconds = 4.0\nrate_dps = [0.0, 0.0, 90.0]\n",
        )
        .unwrap();
        let mut world = World::new(&script);
        let mut t = 0.0;
        while t < 1.0 {
            t += 0.001;
            world.advance(t);
        }
        // One second at 90°/s: a quarter turn about z, so body x points at world y.
        close(
            world.attitude().rotate([1.0, 0.0, 0.0]),
            [0.0, 1.0, 0.0],
            1e-3,
        );
        let pose = world.advance(1.001);
        close(pose.gyro, [0.0, 0.0, std::f64::consts::FRAC_PI_2], 1e-9);
    }

    #[test]
    fn attitude_slerps_to_its_target_and_holds() {
        let script = Script::from_toml_str(
            "format = \"lemnos.sim\"\nschema_version = 1\n[[motion]]\nkind = \"attitude\"\nseconds = 2.0\nroll_deg = 30.0\npitch_deg = -10.0\nyaw_deg = 45.0\n",
        )
        .unwrap();
        let mut world = World::new(&script);
        world.advance(0.0);
        world.advance(3.0);
        let target = Quat::from_euler_deg(30.0, -10.0, 45.0);
        let q = world.attitude();
        assert!(
            (q.w * target.w + q.x * target.x + q.y * target.y + q.z * target.z).abs() > 1.0 - 1e-9
        );
    }

    #[test]
    fn repeat_wraps_the_motion() {
        let script = Script::from_toml_str(
            "format = \"lemnos.sim\"\nschema_version = 1\nrepeat = true\n[[motion]]\nkind = \"shake\"\nseconds = 1.0\namplitude = 2.0\nhz = 1.0\n",
        )
        .unwrap();
        let mut world = World::new(&script);
        let a = world.advance(0.25).accel[0];
        let b = world.advance(1.25).accel[0];
        assert!((a - b).abs() < 1e-9, "{a} {b}");
    }
}
