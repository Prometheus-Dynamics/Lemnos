//! Which way is down, for a falling sparkle: the ring's bottom LED from one
//! reading of the IMU's acceleration. The ring lies in a plane of the IMU
//! (`gravity_plane`, two axes with signs, such as `["-y", "x"]`); the angle
//! of the in-plane gravity, measured from the ring's first physical LED
//! (`gravity_led0_deg`, the angle of the LED at index 0 on the wire), gives
//! the physical LED at the bottom, which the strip's `offset` and `direction`
//! turn into a logical LED. The accelerometer reads the support force, so
//! the down direction is the negated reading.
//!
//! Angles go round the plane from the first axis toward the second, and the
//! physical LEDs go round the same way, one step per `360 / count` degrees.
//! A board lying flat (the in-plane part under [`FLAT_FRACTION`] of the
//! total) has no useful down; the look then uses `default_down`.

use crate::BoardError;
use crate::light::strip_config;
use crate::schema::{ConfigValue, DeviceSpec};
use lemnos_drivers_ws2812::{Direction, StripConfig};
use lemnos_hal::ErrorKind;

/// The `config` keys that set the gravity of a light.
pub const GRAVITY_KEYS: &[&str] = &[
    "gravity_device",
    "gravity_plane",
    "gravity_led0_deg",
    "default_down",
];

/// Below this share of the reading in the ring's plane the board is treated
/// as flat, and the default down is used.
pub const FLAT_FRACTION: f64 = 0.35;

/// A light's gravity settings, and its strip.
#[derive(Debug, Clone, PartialEq)]
pub struct Gravity {
    /// The IMU to read once when a falling sparkle starts.
    pub device: Option<String>,
    /// The two axes spanning the ring's plane: (axis 0..3 for x, y, z, sign).
    pub plane: Option<[(usize, f64); 2]>,
    /// The angle of the first physical LED, in degrees in the plane.
    pub led0_deg: f64,
    /// The logical LED used when there is no gravity.
    pub default_down: u16,
    pub strip: StripConfig,
}

/// The bottom of the ring for one reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bottom {
    /// The logical LED, in thousandths (`0..count * 1000`).
    pub led_milli: u32,
    /// The down direction in the plane, in degrees.
    pub angle_deg: f64,
    /// The in-plane share of the reading (1 is fully in the plane).
    pub in_plane: f64,
}

impl Gravity {
    /// The default bottom, in thousandths of an LED.
    pub fn default_milli(&self) -> u32 {
        u32::from(self.default_down) * 1_000
    }

    /// The bottom for an accelerometer reading `accel` (any unit): `None`
    /// when no plane is set, the reading is zero, or the board is flat.
    pub fn bottom(&self, accel: [f64; 3]) -> Option<Bottom> {
        let plane = self.plane?;
        let down = [-accel[0], -accel[1], -accel[2]];
        let total = down.iter().map(|c| c * c).sum::<f64>().sqrt();
        if total.is_nan() || total <= 1e-9 {
            return None;
        }
        let u = plane[0].1 * down[plane[0].0];
        let v = plane[1].1 * down[plane[1].0];
        let in_plane = (u * u + v * v).sqrt() / total;
        if in_plane < FLAT_FRACTION {
            return None;
        }
        let angle_deg = v.atan2(u).to_degrees().rem_euclid(360.0);
        Some(Bottom {
            led_milli: self.led_at(angle_deg),
            angle_deg,
            in_plane,
        })
    }

    /// The logical LED (thousandths) at a plane angle, honouring the strip's
    /// offset and direction.
    pub fn led_at(&self, angle_deg: f64) -> u32 {
        let count = u32::from(self.strip.count);
        if count == 0 {
            return 0;
        }
        let n = f64::from(count);
        let step = 360.0 / n;
        // The physical LED, fractional, from the first one.
        let physical = ((angle_deg - self.led0_deg) / step).rem_euclid(n);
        let steps = (physical - f64::from(self.strip.offset)).rem_euclid(n);
        let logical = match self.strip.direction {
            Direction::Cw => steps,
            Direction::Ccw => (n - steps).rem_euclid(n),
        };
        ((logical * 1_000.0).round() as u32) % (count * 1_000)
    }

    /// The plane angle (degrees) at the centre of physical LED `index`, for
    /// the calibration tool.
    pub fn angle_of_physical(&self, index: u16) -> f64 {
        let n = f64::from(self.strip.count.max(1));
        (self.led0_deg + f64::from(index) * 360.0 / n).rem_euclid(360.0)
    }
}

/// One axis, `x`, `-x`, `+y`, ...: its index (0 for x) and sign.
pub fn parse_axis(text: &str) -> Option<(usize, f64)> {
    let (sign, name) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text.strip_prefix('+').unwrap_or(text)),
    };
    let index = match name {
        "x" => 0,
        "y" => 1,
        "z" => 2,
        _ => return None,
    };
    Some((index, sign))
}

fn bad(spec: &DeviceSpec, reason: impl Into<String>) -> BoardError {
    BoardError::device(&spec.id, ErrorKind::Configuration, reason)
}

/// The gravity settings of a `ws2812` light.
pub fn gravity(spec: &DeviceSpec) -> Result<Gravity, BoardError> {
    let strip = strip_config(spec)?;
    let device = match spec.config.get("gravity_device") {
        None => None,
        Some(ConfigValue::String(name)) if !name.is_empty() => Some(name.clone()),
        Some(_) => return Err(bad(spec, "gravity_device must be the IMU's device id")),
    };
    let plane = match spec.config.get("gravity_plane") {
        None => None,
        Some(ConfigValue::List(items)) => {
            let axes: Option<Vec<(usize, f64)>> = items
                .iter()
                .map(|v| v.as_str().and_then(parse_axis))
                .collect();
            match axes.as_deref() {
                Some([a, b]) if a.0 != b.0 => Some([*a, *b]),
                _ => {
                    return Err(bad(
                        spec,
                        "gravity_plane must be two different axes, such as [\"-y\", \"x\"]",
                    ));
                }
            }
        }
        Some(_) => return Err(bad(spec, "gravity_plane must be a list of two axes")),
    };
    if device.is_some() != plane.is_some() {
        return Err(bad(
            spec,
            "gravity_device and gravity_plane go together (set both, or neither)",
        ));
    }
    let led0_deg = match spec.config.get("gravity_led0_deg") {
        None => 0.0,
        Some(ConfigValue::Integer(v)) => *v as f64,
        Some(ConfigValue::Float(v)) if v.is_finite() => *v,
        Some(_) => return Err(bad(spec, "gravity_led0_deg must be a number of degrees")),
    };
    let default_down = match spec.config.get("default_down") {
        None => strip.count / 2,
        Some(ConfigValue::Integer(v)) if (0..i64::from(strip.count)).contains(v) => *v as u16,
        Some(_) => return Err(bad(spec, "default_down must be an LED below count")),
    };
    Ok(Gravity {
        device,
        plane,
        led0_deg,
        default_down,
        strip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_drivers_ws2812::Wire;

    fn gravity_with(count: u16, offset: u16, direction: Direction) -> Gravity {
        Gravity {
            device: Some("imu".into()),
            plane: Some([(0, 1.0), (1, 1.0)]),
            led0_deg: 0.0,
            default_down: count / 2,
            strip: StripConfig::new(count, Wire::Rgb)
                .with_offset(offset)
                .with_direction(direction),
        }
    }

    #[test]
    fn every_logical_led_is_found_from_its_own_angle_with_offset_and_direction() {
        for direction in [Direction::Cw, Direction::Ccw] {
            for offset in [0u16, 3, 11] {
                let g = gravity_with(16, offset, direction);
                for i in 0..16u16 {
                    // The angle of the physical LED that shows logical LED i.
                    let angle = g.angle_of_physical(g.strip.physical(i));
                    let led = g.led_at(angle);
                    let want = u32::from(i) * 1_000;
                    let diff = (i64::from(led) - i64::from(want)).abs();
                    assert!(
                        diff <= 1 || diff >= 16_000 - 1,
                        "{direction:?} offset {offset} LED {i}: got {led}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_first_physical_led_angle_moves_the_bottom() {
        let mut g = gravity_with(16, 0, Direction::Cw);
        g.led0_deg = 90.0;
        // Angle 90 is physical LED 0 now, so logical LED 0 is the bottom.
        assert_eq!(g.led_at(90.0), 0);
        // Angle 180 is physical LED 4 (22.5 degrees each).
        assert_eq!(g.led_at(180.0), 4_000);
    }

    #[test]
    fn a_flat_board_has_no_bottom_and_falls_back_to_the_default() {
        let g = gravity_with(16, 0, Direction::Cw);
        // Lying flat: the support force is all along z.
        assert_eq!(g.bottom([0.0, 0.0, 9.8]), None);
        assert_eq!(g.default_milli(), 8_000);
        // Tilted: the down direction is 90 degrees (the in-plane y), from
        // an accelerometer reading up (-y).
        let b = g.bottom([0.0, -6.9, 6.9]).expect("tilted");
        assert!((b.angle_deg - 90.0).abs() < 0.5, "{}", b.angle_deg);
        assert_eq!(b.led_milli, 4_000);
        assert!(b.in_plane > 0.6);
        // A zero reading has no direction either.
        assert_eq!(g.bottom([0.0, 0.0, 0.0]), None);
    }

    #[test]
    fn plane_signs_turn_the_angle() {
        let mut g = gravity_with(16, 0, Direction::Cw);
        // Plane (-y, x): the in-plane down is (u, v) = (-down_y, down_x).
        g.plane = Some([(1, -1.0), (0, 1.0)]);
        // Reading up along +y, so down is -y: u = +8.8, v = 0, angle 0.
        let b = g.bottom([0.0, 9.8 * 0.9, 0.0]).expect("in plane");
        assert!(
            b.angle_deg.min(360.0 - b.angle_deg) < 0.5,
            "{}",
            b.angle_deg
        );
        assert_eq!(b.led_milli, 0);
    }

    #[test]
    fn config_keys_parse_and_check_their_values() {
        let mut spec = DeviceSpec::new("ring", "ws2812");
        spec.config.insert("count".into(), ConfigValue::Integer(16));
        spec.config
            .insert("gravity_device".into(), ConfigValue::String("imu".into()));
        spec.config.insert(
            "gravity_plane".into(),
            ConfigValue::List(vec![
                ConfigValue::String("-y".into()),
                ConfigValue::String("x".into()),
            ]),
        );
        spec.config
            .insert("gravity_led0_deg".into(), ConfigValue::Float(22.5));
        let g = gravity(&spec).expect("parses");
        assert_eq!(g.device.as_deref(), Some("imu"));
        assert_eq!(g.plane, Some([(1, -1.0), (0, 1.0)]));
        assert_eq!(g.led0_deg, 22.5);
        assert_eq!(g.default_down, 8);

        let mut bad_plane = spec.clone();
        bad_plane.config.insert(
            "gravity_plane".into(),
            ConfigValue::List(vec![
                ConfigValue::String("x".into()),
                ConfigValue::String("x".into()),
            ]),
        );
        assert!(gravity(&bad_plane).is_err());

        let mut half = spec.clone();
        half.config.remove("gravity_plane");
        assert!(gravity(&half).is_err(), "a device needs a plane");

        let mut down = spec.clone();
        down.config
            .insert("default_down".into(), ConfigValue::Integer(16));
        assert!(gravity(&down).is_err());
    }
}
