//! The plant: the simulated board's physics, advanced in simulated time and
//! written into the chips' registers and the sysfs files the devices read.
//!
//! One [`Plant::step`] is one tick of the simulation: the motion and the
//! sensors' view of it, the supply load, the scripted GPIO levels and faults,
//! every chip's registers, and (every [`SLOW_MS`]) the fan's tachometer and
//! the thermal zone. A step at a given simulated time depends only on the
//! script, the seed, the previous steps and the lines and files the service
//! wrote, so tests can drive it without threads.

use super::bus::SimBus;
use super::chips::{Chip, Sample};
use super::script::{FaultKind, GpioLevel, Imu, Load, Mag, Power, Script, Thermal};
use super::world::World;
use lemnos_hal::mock::MockLine;
use std::fs;
use std::path::PathBuf;

/// Fan and thermal updates, in simulated milliseconds.
pub(crate) const SLOW_MS: f64 = 50.0;

/// A fault with the chips it applies to.
#[derive(Debug)]
struct ActiveFault {
    kind: FaultKind,
    at: f64,
    until: Option<f64>,
    rate: f64,
    chips: Vec<usize>,
    /// Whether the fault was in effect at the previous step.
    was_active: bool,
}

/// A scripted GPIO level, applied once its time has come.
#[derive(Debug)]
struct ScriptedLevel {
    level: GpioLevel,
    line: MockLine,
    applied: bool,
}

/// The rotor's first-order lag and the thermal zone's.
#[derive(Debug)]
struct Lagged {
    value: f64,
    target: f64,
}

/// What the plant writes: the hwmon fan and the thermal zone, if the board has them.
#[derive(Debug)]
pub struct Sysfs {
    pub fan: Option<PathBuf>,
    pub thermal: Option<PathBuf>,
}

/// The simulated board's state.
#[derive(Debug)]
pub(crate) struct Plant {
    speed: f64,
    world: World,
    rng: Rng,
    imu: Imu,
    mag: Mag,
    power: Power,
    thermal_params: Thermal,
    loads: Vec<Load>,
    /// Each chip with its device id, and whether a stuck fault holds it.
    chips: Vec<(String, Chip, bool)>,
    faults: Vec<ActiveFault>,
    /// The switched ports' lines: each one that is on adds to the load.
    ports: Vec<MockLine>,
    gpio: Vec<ScriptedLevel>,
    sysfs: Sysfs,
    rotor: Lagged,
    die: Lagged,
    /// The supply current and the fan duty at the last step.
    current_a: f64,
    duty: f64,
    last_slow_t: Option<f64>,
    max_rpm: f64,
    rotor_tau: f64,
    die_tau: f64,
}

impl Plant {
    /// Builds the plant from a script and what the board attached: the
    /// chips (each with its device id), the switched ports and the scripted
    /// lines. Faults name chips by device id.
    pub fn new(
        script: &Script,
        chips: Vec<(String, Chip)>,
        ports: Vec<MockLine>,
        lines: Vec<(MockLine, GpioLevel)>,
        sysfs: Sysfs,
    ) -> Result<Self, String> {
        let mut faults = Vec::new();
        for fault in &script.faults {
            let indices: Vec<usize> = chips
                .iter()
                .enumerate()
                .filter(|(_, (device, _))| *device == fault.device)
                .map(|(i, _)| i)
                .collect();
            if indices.is_empty() {
                return Err(format!(
                    "fault on {:?}: no simulated chip is that device",
                    fault.device
                ));
            }
            faults.push(ActiveFault {
                kind: fault.kind,
                at: fault.at,
                until: fault.seconds.map(|s| fault.at + s),
                rate: fault.rate.unwrap_or(0.5),
                chips: indices,
                was_active: false,
            });
        }
        let chips: Vec<(String, Chip, bool)> =
            chips.into_iter().map(|(d, c)| (d, c, false)).collect();
        Ok(Self {
            speed: script.speed,
            world: World::new(script),
            rng: Rng::new(script.seed),
            imu: script.imu.clone(),
            mag: script.mag.clone(),
            power: script.power.clone(),
            thermal_params: script.thermal.clone(),
            loads: script.loads.clone(),
            chips,
            faults,
            ports,
            gpio: lines
                .into_iter()
                .map(|(line, level)| ScriptedLevel {
                    level,
                    line,
                    applied: false,
                })
                .collect(),
            sysfs,
            rotor: Lagged {
                value: 0.0,
                target: 0.0,
            },
            die: Lagged {
                value: script.thermal.ambient_c,
                target: script.thermal.ambient_c,
            },
            current_a: 0.0,
            duty: 0.0,
            last_slow_t: None,
            max_rpm: script.fan.max_rpm,
            rotor_tau: script.fan.time_constant_s,
            die_tau: script.thermal.time_constant_s,
        })
    }

    /// Simulated seconds per real second.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// The fan's speed, rpm.
    #[cfg(test)]
    pub fn rotor_rpm(&self) -> f64 {
        self.rotor.value
    }

    /// The supply current at the last step, A.
    #[cfg(test)]
    pub fn current_a(&self) -> f64 {
        self.current_a
    }

    /// Advances the simulation to time `t` (simulated seconds from the start).
    pub fn step(&mut self, t: f64) {
        let pose = self.world.advance(t);
        let ports_on = self
            .ports
            .iter()
            .filter(|line| line.level() == Some(true))
            .count();
        let burst: f64 = self
            .loads
            .iter()
            .filter(|l| t >= l.at && t < l.at + l.seconds)
            .map(|l| l.amps)
            .sum();
        let current = self.power.base_a
            + self.power.port_a * ports_on as f64
            + burst
            + self.rng.gaussian() * self.power.noise_a;
        self.current_a = current.max(0.0);
        let bus_v = self.power.bus_v - self.power.sag_ohm * self.current_a;

        for scripted in &mut self.gpio {
            if !scripted.applied && t >= scripted.level.at {
                scripted.line.drive(scripted.level.level);
                scripted.applied = true;
            }
        }
        self.apply_faults(t);

        let sample = Sample {
            pose: super::world::Pose {
                accel: add_noise(&mut self.rng, pose.accel, self.imu.accel_noise),
                gyro: add_bias(
                    &mut self.rng,
                    pose.gyro,
                    self.imu.gyro_bias,
                    self.imu.gyro_noise,
                ),
                field_ut: add_noise(&mut self.rng, pose.field_ut, self.mag.noise_ut),
            },
            temp_c: self.die.value,
            bus_v,
            current_a: self.current_a,
        };
        for (_, chip, frozen) in &self.chips {
            if !*frozen {
                chip.update(&sample);
            }
        }

        let due = self
            .last_slow_t
            .is_none_or(|last| (t - last) * 1000.0 >= SLOW_MS);
        if due {
            let dt = self.last_slow_t.map_or(0.0, |last| t - last);
            self.last_slow_t = Some(t);
            self.update_slow(dt);
        }
    }

    /// The fan and the thermal zone: the rotor follows the duty the fan's
    /// `pwm1` asks for, lagging; the die warms with the current and the duty.
    fn update_slow(&mut self, dt: f64) {
        if let Some(dir) = &self.sysfs.fan {
            let read = |name: &str| -> Option<f64> {
                fs::read_to_string(dir.join(name))
                    .ok()?
                    .trim()
                    .parse::<f64>()
                    .ok()
            };
            // pwm-fan's mode 0 is full speed; otherwise `pwm1` (0 to 255) sets it.
            let mode = read("pwm1_enable").unwrap_or(1.0);
            let pwm = read("pwm1").unwrap_or(0.0).clamp(0.0, 255.0);
            self.duty = if mode == 0.0 { 1.0 } else { pwm / 255.0 };
            self.rotor.target = self.duty * self.max_rpm;
            let alpha = 1.0 - (-dt / self.rotor_tau).exp();
            self.rotor.value += (self.rotor.target - self.rotor.value) * alpha;
            let _ = fs::write(
                dir.join("fan1_input"),
                format!("{}\n", self.rotor.value.round() as i64),
            );
        }
        let params = &self.thermal_params;
        self.die.target =
            params.ambient_c + params.c_per_amp * self.current_a + params.c_per_duty * self.duty;
        let alpha = 1.0 - (-dt / self.die_tau).exp();
        self.die.value += (self.die.target - self.die.value) * alpha;
        if let Some(dir) = &self.sysfs.thermal {
            let mc = (self.die.value * 1000.0).round() as i64;
            let _ = fs::write(dir.join("temp"), format!("{mc}\n"));
        }
    }

    /// Makes each fault take effect at its time and lift at its end.
    fn apply_faults(&mut self, t: f64) {
        for fault in &mut self.faults {
            let active = t >= fault.at && fault.until.is_none_or(|until| t < until);
            match fault.kind {
                FaultKind::Unresponsive => {
                    if active != fault.was_active {
                        for &i in &fault.chips {
                            set_answering(&self.chips[i].1, !active);
                        }
                    }
                }
                FaultKind::BusError => {
                    if active {
                        for &i in &fault.chips {
                            let nack = self.rng.uniform() < fault.rate;
                            set_answering(&self.chips[i].1, !nack);
                        }
                    } else if fault.was_active {
                        for &i in &fault.chips {
                            set_answering(&self.chips[i].1, true);
                        }
                    }
                }
                FaultKind::Stuck => {
                    for &i in &fault.chips {
                        self.chips[i].2 = active;
                    }
                }
            }
            fault.was_active = active;
        }
    }
}

/// Makes every address of `chip` answer (`true`) or not.
fn set_answering(chip: &Chip, answer: bool) {
    let bus: &SimBus = chip.bus();
    for address in chip.addresses() {
        bus.set_dead(address, !answer);
    }
}

fn add_noise(rng: &mut Rng, v: [f64; 3], sigma: f64) -> [f64; 3] {
    v.map(|c| c + rng.gaussian() * sigma)
}

fn add_bias(rng: &mut Rng, v: [f64; 3], bias: [f64; 3], sigma: f64) -> [f64; 3] {
    [0, 1, 2].map(|i| v[i] + bias[i] + rng.gaussian() * sigma)
}

/// A deterministic generator (xorshift64*, seeded through splitmix64).
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // splitmix64: any seed (zero included) becomes a non-zero state.
        let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Self((z ^ (z >> 31)) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in [0, 1).
    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box-Muller).
    fn gaussian(&mut self) -> f64 {
        let u1 = self.uniform().max(f64::MIN_POSITIVE);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

#[cfg(test)]
mod rng_tests {
    use super::Rng;

    #[test]
    fn gaussian_has_unit_spread_and_is_repeatable() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let samples: Vec<f64> = (0..20_000).map(|_| a.gaussian()).collect();
        assert!(
            samples
                .iter()
                .zip((0..20_000).map(|_| b.gaussian()))
                .all(|(x, y)| *x == y)
        );
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let var = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / samples.len() as f64;
        assert!(mean.abs() < 0.03, "{mean}");
        assert!((var - 1.0).abs() < 0.05, "{var}");
    }
}
