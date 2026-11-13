//! A fan controller: it reads temperatures and drives one fan, as `fan-control/1`. Duty is
//! per-mille, 0 to 1000. Curves, hysteresis, floor and slew are in [`law`].
//!
//! # Wiring
//!
//! The board names the devices it uses, by slot:
//!
//! - `fan` (required): a `fan/1` device, such as `hwmon-fan`.
//! - `t0` (required): a `temperature/1` device.
//! - `t1`, `t2`, `t3` (optional): more temperature devices. Each one named is an input.
//!
//! ```toml
//! [[devices]]
//! id = "fan-control"
//! driver = "fan-control"
//! uses = { fan = "fan", t0 = "cpu-thermal", t1 = "board-thermal" }
//! config = { curve_t0 = "30000:300,60000:600,75000:1000", floor = 300, slew_per_s = 50 }
//! ```
//!
//! # Configuration
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `floor` | 0 | Lowest duty while running (per-mille, 0 to 1000). |
//! | `hysteresis` | 0 | How far an input falls below its peak before its curve lets go (its unit). |
//! | `slew_per_s` | 50 | Most the duty falls per second (per-mille). Rising is immediate. |
//! | `kick_ms` | 0 | From a stopped fan with floor 0: full duty this long first. |
//! | `max_age_ms` | 3000 | How old a reading may be and still count. |
//! | `curve_tN` | 30000:300,60000:600,75000:1000 | Input `tN`'s curve: `value:duty` pairs, values in millidegrees Celsius, ascending. |
//!
//! # Time
//!
//! The driver has no clock of its own. It steps once a second on the host's time, from `tick`:
//! it is called once it starts, when a step or a hold ends, and after every method call, and it
//! returns when it next needs to run. A tick that comes early does nothing. Nothing blocks and no
//! thread sleeps.
//!
//! # Failure
//!
//! - A temperature read that fails keeps the last good reading for `max_age_ms`, then the input is
//!   stale and the fan runs at full duty and the driver reports `Degraded`. A reading comes back
//!   and the curves take over again.
//! - A failed fan write releases the fan (`release`, which puts it at full duty), and the next
//!   attempt waits 1 s, 2 s, 4 s, then 8 s, and stays at 8 s. The driver reports `Degraded`
//!   meanwhile. It never gives up.
//! - A disabled controller releases the fan and makes no writes until it is enabled. A failed
//!   release is tried again each second.
//! - Dropping the driver (its device stops, is rebound, or a call into it panics) releases the fan.
//!
//! The fan's safe state is full duty, and it is the fan driver's to keep: `pwm-channel-fan` starts
//! there and goes there on release. This controller only decides when it asks for less. Nothing
//! outside the daemon is relied on to cool the board when the daemon's view fails.
//!
//! # State
//!
//! The tuning lives in memory only. A method can change it (`set_settings`, `set_curve`, `hold`,
//! `set_enabled`), and a restart starts again from the board's configuration.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod law;

#[cfg(test)]
mod tests;

use law::{Control, Curve, FULL, SLOTS, parse_curve};
use lemnos_contract_fan::Fan;
use lemnos_contract_fan_control::{FanControl, MAX_POINTS, Point, Settings, State};
use lemnos_contract_temperature::Temperature;
use lemnos_core::{
    Config, ConfigValue, Description, Dev, Health, Item, Lifecycle, MethodId, Result, Status, Unit,
    driver,
};

/// The method that reads `State`, for a description.
const STATE: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// The configuration key of each input's curve.
const CURVE_KEYS: [&str; SLOTS] = ["curve_t0", "curve_t1", "curve_t2", "curve_t3"];

/// The curve an input gets without its `curve_tN`: 30 °C at 300, 60 °C at 600, 75 °C at full.
const DEFAULT_CURVE: [Point; 3] = [
    Point {
        value: 30_000,
        duty: 300,
    },
    Point {
        value: 60_000,
        duty: 600,
    },
    Point {
        value: 75_000,
        duty: 1000,
    },
];

#[driver("fan-control", provides = [FanControl])]
pub struct FanController {
    #[needs]
    fan: Dev<dyn Fan>,
    #[needs]
    t0: Dev<dyn Temperature>,
    #[needs(optional)]
    t1: Option<Dev<dyn Temperature>>,
    #[needs(optional)]
    t2: Option<Dev<dyn Temperature>>,
    #[needs(optional)]
    t3: Option<Dev<dyn Temperature>>,
    control: Control,
}

impl Lifecycle for FanController {
    type Needs = FanControllerNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    const TICKS: bool = true;

    fn start(needs: FanControllerNeeds, cfg: &Config<'_>) -> Result<Self> {
        let (settings, max_age_ms) = tuning(cfg)?;
        let wired = [
            true,
            needs.t1.is_some(),
            needs.t2.is_some(),
            needs.t3.is_some(),
        ];
        let mut curves = [None; SLOTS];
        for (slot, is_wired) in wired.into_iter().enumerate() {
            if is_wired {
                curves[slot] = Some(curve(cfg, slot)?);
            }
        }
        let control = Control::new(settings, max_age_ms, curves)?;
        Ok(Self {
            fan: needs.fan,
            t0: needs.t0,
            t1: needs.t1,
            t2: needs.t2,
            t3: needs.t3,
            control,
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("duty", STATE).unit(Unit::One, -3).field("duty"));
        out.add(
            Item::value("demand", STATE)
                .unit(Unit::One, -3)
                .field("demand"),
        );
    }

    fn health(&self) -> Health {
        self.control.health()
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.control.begin(now_ns);
        if self.control.release_due(now_ns) {
            self.release_fan(now_ns);
        }
        if self.control.step_due(now_ns) {
            for slot in 0..SLOTS {
                if self.control.present(slot) {
                    let reading = self.read(slot).and_then(millidegrees);
                    self.control.observe(slot, reading, now_ns);
                }
            }
            if let Some(duty) = self.control.step(now_ns) {
                let written = u16::try_from(duty)
                    .map_err(|_| Status::INVALID_ARGUMENT)
                    .and_then(|per_mille| self.fan.set_duty(per_mille));
                match written {
                    Ok(()) => self.control.wrote(),
                    Err(_) => {
                        self.release_fan(now_ns);
                        self.control.write_failed(now_ns);
                    }
                }
            }
        }
        self.control.next_due()
    }
}

impl FanController {
    /// Reads input `slot`, if a device is wired to it.
    fn read(&mut self, slot: usize) -> Option<f32> {
        match slot {
            0 => self.t0.read().ok(),
            1 => self.t1.as_mut()?.read().ok(),
            2 => self.t2.as_mut()?.read().ok(),
            3 => self.t3.as_mut()?.read().ok(),
            _ => None,
        }
    }

    /// Releases the fan (its safe state). If that fails, the control law tries again in a second.
    fn release_fan(&mut self, now_ns: u64) {
        match self.fan.release() {
            Ok(()) => self.control.released(),
            Err(_) => self.control.release_failed(now_ns),
        }
    }
}

impl Drop for FanController {
    fn drop(&mut self) {
        // Nothing can report an error from here. A release is the fan's safe state, so it is
        // harmless on a fan this controller never drove.
        let _ = self.fan.release();
    }
}

impl FanControl for FanController {
    fn state(&mut self) -> Result<State> {
        Ok(self.control.state())
    }

    fn settings(&mut self) -> Result<Settings> {
        Ok(self.control.settings())
    }

    fn set_settings(&mut self, settings: Settings) -> Result<()> {
        self.control.set_settings(settings)
    }

    fn curve(&mut self, input: u8, into: &mut [u8]) -> Result<usize> {
        let curve = self.control.curve(input)?;
        Point::write_all(curve.points(), into)
    }

    fn set_curve(&mut self, input: u8, points: &[u8]) -> Result<()> {
        let mut buf = [Point::default(); MAX_POINTS];
        let mut count = 0;
        for point in Point::read_all(points)? {
            let slot = buf.get_mut(count).ok_or(Status::INVALID_ARGUMENT)?;
            *slot = point;
            count += 1;
        }
        self.control.set_curve(input, &buf[..count])
    }

    fn hold(&mut self, duty: i32, ms: u32) -> Result<()> {
        self.control.hold(duty, ms)
    }

    fn release_hold(&mut self) -> Result<()> {
        self.control.release_hold();
        Ok(())
    }

    fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        self.control.set_enabled(enabled);
        Ok(())
    }
}

/// Degrees Celsius as millidegrees, rounded. A value that is not a finite number in a sane
/// range is no reading at all.
fn millidegrees(celsius: f32) -> Option<i32> {
    let milli = celsius * 1000.0;
    if !milli.is_finite() || !(-1.0e9..=1.0e9).contains(&milli) {
        return None;
    }
    Some(if milli >= 0.0 {
        (milli + 0.5) as i32
    } else {
        (milli - 0.5) as i32
    })
}

/// A whole-number setting in `low..=high`, or `default` when it is not set.
fn int(cfg: &Config<'_>, key: &str, low: i64, high: i64, default: i64) -> Result<i64> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Int(value)) if (low..=high).contains(&value) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}

/// The tuning and the staleness limit, from the configuration.
fn tuning(cfg: &Config<'_>) -> Result<(Settings, u32)> {
    let settings = Settings {
        floor: int(cfg, "floor", 0, i64::from(FULL), 0)? as i32,
        hysteresis: int(cfg, "hysteresis", 0, i64::from(i32::MAX), 0)? as i32,
        slew_per_s: int(cfg, "slew_per_s", 0, i64::from(FULL), 50)? as i32,
        kick_ms: int(cfg, "kick_ms", 0, i64::from(u32::MAX), 0)? as u32,
    };
    let max_age_ms = int(cfg, "max_age_ms", 1, i64::from(u32::MAX), 3000)? as u32;
    Ok((settings, max_age_ms))
}

/// Input `slot`'s curve from its `curve_tN`, or the default.
fn curve(cfg: &Config<'_>, slot: usize) -> Result<Curve> {
    match cfg.get(CURVE_KEYS[slot]) {
        None => Curve::new(&DEFAULT_CURVE),
        Some(ConfigValue::Str(text)) => parse_curve(text),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}
