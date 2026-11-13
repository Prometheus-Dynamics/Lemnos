//! A fan driven straight from a PWM channel, served as `fan/1`. Duty is per-mille, 0 to 1000.
//!
//! This is the driver for a fan wired to a PWM output. Nothing sits between it and the channel:
//! no kernel `pwm-fan` driver, no hwmon device, no thermal governor. The kernel's `pwm-fan` is
//! a different thing, and this driver never binds to it. For a fan the kernel exposes through
//! hwmon, see `hwmon-fan`.
//!
//! The board names the channel, and optionally a tachometer line:
//!
//! ```toml
//! [[devices]]
//! id = "fan"
//! driver = "pwm-channel-fan"
//! uses = { pwm = "pwmchip0:1", tach = "gpiochip0:20" }
//! config = { frequency_hz = 25000, min_duty = 200, kick_ms = 500, pulses_per_rev = 2 }
//! ```
//!
//! # Configuration
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `frequency_hz` | 25000 | The PWM frequency, 1 to 1000000. |
//! | `inverted` | false | The PWM polarity: the output is active low. The duty is still the fan's duty. |
//! | `min_duty` | 0 | The lowest duty the hardware gets for any non-zero request, per-mille. Zero stays zero, so the fan can stop. |
//! | `kick_ms` | 0 | From a stopped fan, full duty this long before the requested duty. |
//! | `pulses_per_rev` | 2 | Rising edges of the tachometer per revolution, 1 to 64. Used only with `tach`. |
//!
//! # Duty
//!
//! `set_duty(d)` sets the requested duty. The hardware gets `d`, raised to `min_duty` when `d` is
//! not zero, and the PWM's high time is that share of the period. Full duty, the state the fan is
//! in until something asks for less, is 1000.
//!
//! # Tachometer
//!
//! With `tach`, the line is configured at bind as an input that reports rising edges. The driver
//! reads the edges at each tick (up to 64 per tick, a tick every 100 ms) and takes the revolutions
//! from the time between the last two rising edges, using the edges' own timestamps. Edges come
//! from the kernel's queue; an edge lost there is seen as a gap in the sequence, and the interval
//! across the gap is not used. `rpm` is zero when the pulses have stopped (for longer than twice
//! the last interval, or one second) or before two pulses have been seen.
//!
//! # Time
//!
//! The driver steps from `tick`, on the host's clock. The host ticks it after every method call, so
//! a new duty is written at once, in that tick. It writes again when a kick ends and when a failed
//! write is due for retry. Methods only change what is wanted. Nothing blocks and no thread sleeps.
//!
//! # Safe state
//!
//! The driver starts at full duty and writes it at its first tick. `release` and dropping the
//! driver both set full duty. Nothing hands the fan to anything else: a stopped driver leaves the
//! fan at full. A controller that wants the fan stopped must ask for it.
//!
//! What a temperature failure does is the controller's to decide. `fan-control` runs the fan at
//! full duty when an input is missing or stale, and reports `Degraded`.
//!
//! # Failure
//!
//! A failed PWM write fails the call that made it, and the driver retries from its tick: after 1
//! s, then 2, 4 and 8 s, and at 8 s for good. It never gives up. While writes fail it reports
//! `Degraded`, and `Ok` after the next good write. A failed tachometer read is not a fan failure:
//! its pulses are simply not counted.

#[cfg(test)]
mod tests;

pub mod math;

use lemnos_contract_fan::Fan;
use lemnos_core::{
    Config, ConfigValue, Description, Health, Item, Lifecycle, Line, MethodId, Pwm, Result, Status,
    Unit, driver, status_of,
};
use lemnos_hal::raw::{EdgeDetect, LineConfig, Polarity, PwmConfig};
use math::{FULL, Tach, backoff_ns, duty_ns, hardware_duty, period_ns};

/// Method numbers in the `fan/1` contract.
const SET_DUTY: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const DUTY: MethodId = MethodId {
    contract: 0,
    method: 2,
};
const RPM: MethodId = MethodId {
    contract: 0,
    method: 3,
};
const RELEASE: MethodId = MethodId {
    contract: 0,
    method: 4,
};

/// How often a driver with a tachometer reads its edges, in nanoseconds.
const TACH_POLL_NS: u64 = 100_000_000;
/// The most edges read in one tick. A tick that finds more leaves the rest for the next one.
const MAX_EDGES_PER_TICK: usize = 64;
/// The longest kick the configuration allows.
const MAX_KICK_MS: i64 = 10_000;

/// The health reason after a failed PWM write.
const WRITE_FAILED: &str = "the PWM write failed: retried with backoff, full duty is the goal";

/// What the configuration sets, checked once at bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    period_ns: u64,
    polarity: Polarity,
    min_duty: u16,
    kick_ns: u64,
    pulses_per_rev: u32,
}

#[driver("pwm-channel-fan", provides = [Fan])]
pub struct PwmChannelFan {
    #[needs]
    pwm: Pwm,
    #[needs(optional)]
    tach: Option<Line>,
    shape: Shape,
    /// The requested duty, per-mille. Full until something asks for less.
    target: u16,
    /// The duty the hardware last took. `None` until the first write works.
    applied: Option<u16>,
    /// Whether the kick has started for the current stop.
    kicked: bool,
    /// Full duty until this time (ns), while a kick runs.
    kick_until_ns: Option<u64>,
    /// The tick's time (ns). Method calls use it, since they have no time of their own.
    now_ns: u64,
    /// When a failed write may be tried again (ns).
    retry_at_ns: Option<u64>,
    /// Writes that have failed in a row.
    attempts: u32,
    /// The tachometer's pulses, if a line is wired to one.
    meter: Tach,
}

impl Lifecycle for PwmChannelFan {
    type Needs = PwmChannelFanNeeds;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    const TICKS: bool = true;

    fn start(needs: PwmChannelFanNeeds, cfg: &Config<'_>) -> Result<Self> {
        let shape = shape(cfg)?;
        let mut tach = needs.tach;
        if let Some(line) = tach.as_mut() {
            let config = LineConfig::input().with_edge(EdgeDetect::Rising);
            line.configure(&config).map_err(status_of)?;
        }
        Ok(Self {
            pwm: needs.pwm,
            tach,
            shape,
            target: FULL,
            applied: None,
            kicked: false,
            kick_until_ns: None,
            now_ns: 0,
            retry_at_ns: None,
            attempts: 0,
            meter: Tach::new(shape.pulses_per_rev),
        })
    }

    fn describe(&self, out: &mut Description) {
        if self.tach.is_some() {
            out.add(Item::value("rpm", RPM).unit(Unit::RevolutionPerMinute, 0));
        }
        // Duty is per mille: from 0 to FULL, one step is one per mille.
        out.add(
            Item::value("duty", DUTY)
                .unit(Unit::One, -3)
                .range(0, i64::from(FULL))
                .step(1),
        );
        out.add(
            Item::control("duty", SET_DUTY)
                .unit(Unit::One, -3)
                .range(0, i64::from(FULL))
                .step(1),
        );
        out.add(Item::action("release", RELEASE));
    }

    fn health(&self) -> Health {
        if self.attempts > 0 {
            Health::Degraded(WRITE_FAILED)
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.now_ns = now_ns;
        self.read_edges();
        // A failed write waits for its retry time. Everything else is due now.
        if self.retry_at_ns.is_none_or(|at| now_ns >= at) {
            let _ = self.apply(now_ns);
        }
        self.next_due(now_ns)
    }
}

impl PwmChannelFan {
    /// Reads the tachometer's pending edges, up to [`MAX_EDGES_PER_TICK`].
    fn read_edges(&mut self) {
        let Some(line) = self.tach.as_mut() else {
            return;
        };
        for _ in 0..MAX_EDGES_PER_TICK {
            match line.read_edge() {
                Ok(Some(edge)) => self.meter.edge(edge),
                _ => break,
            }
        }
    }

    /// The hardware duty wanted at `now_ns`, starting a kick if one is due. A kick runs from a
    /// fan that is stopped: full duty for `kick_ns`, then the requested duty.
    fn desired(&mut self, now_ns: u64) -> u16 {
        let hardware = hardware_duty(self.target, self.shape.min_duty);
        if hardware == 0 {
            self.kicked = false;
            self.kick_until_ns = None;
            return 0;
        }
        let stopped = self.applied.is_none_or(|duty| duty == 0);
        if stopped && !self.kicked && self.shape.kick_ns > 0 {
            self.kicked = true;
            self.kick_until_ns = Some(now_ns.saturating_add(self.shape.kick_ns));
        }
        match self.kick_until_ns {
            Some(until) if now_ns < until => FULL,
            Some(_) => {
                self.kick_until_ns = None;
                hardware
            }
            None => hardware,
        }
    }

    /// Writes the duty the driver wants, if it differs from the one the hardware has. A failure
    /// schedules the next try.
    fn apply(&mut self, now_ns: u64) -> Result<()> {
        let want = self.desired(now_ns);
        if self.applied == Some(want) {
            self.attempts = 0;
            self.retry_at_ns = None;
            return Ok(());
        }
        let config = PwmConfig {
            period_ns: self.shape.period_ns,
            duty_ns: duty_ns(self.shape.period_ns, want),
            polarity: self.shape.polarity,
            enabled: true,
        };
        match self.pwm.configure(&config).map_err(status_of) {
            Ok(()) => {
                self.applied = Some(want);
                self.attempts = 0;
                self.retry_at_ns = None;
                Ok(())
            }
            Err(status) => {
                self.attempts = self.attempts.saturating_add(1);
                self.retry_at_ns = Some(now_ns.saturating_add(backoff_ns(self.attempts)));
                Err(status)
            }
        }
    }

    /// When the driver next needs a tick: a retry, the end of a kick, or the next tachometer poll.
    fn next_due(&self, now_ns: u64) -> Option<u64> {
        let mut due: Option<u64> = None;
        let mut want = |at: u64| {
            if at > now_ns {
                due = Some(due.map_or(at, |d| d.min(at)));
            }
        };
        if let Some(at) = self.retry_at_ns {
            want(at);
        }
        if let Some(at) = self.kick_until_ns {
            want(at);
        }
        if self.tach.is_some() {
            want(now_ns.saturating_add(TACH_POLL_NS));
        }
        due
    }
}

impl Fan for PwmChannelFan {
    fn set_duty(&mut self, per_mille: u16) -> Result<()> {
        if per_mille > FULL {
            return Err(Status::INVALID_ARGUMENT);
        }
        // The write is made by the tick the host runs after this call, at the host's time.
        self.target = per_mille;
        Ok(())
    }

    fn duty(&mut self) -> Result<u16> {
        Ok(self.target)
    }

    fn rpm(&mut self) -> Result<Option<u32>> {
        if self.tach.is_none() {
            return Ok(None);
        }
        Ok(Some(self.meter.rpm(self.now_ns)))
    }

    fn release(&mut self) -> Result<()> {
        self.target = FULL;
        Ok(())
    }
}

impl Drop for PwmChannelFan {
    fn drop(&mut self) {
        // Nothing can report an error from here. The fan is left at full duty if the write works.
        self.target = FULL;
        let _ = self.apply(self.now_ns);
    }
}

/// The settings, from the device's `config`, with their defaults.
fn shape(cfg: &Config<'_>) -> Result<Shape> {
    let frequency = int(cfg, "frequency_hz", 1, 1_000_000, 25_000)? as u32;
    let polarity = if flag(cfg, "inverted", false)? {
        Polarity::Inversed
    } else {
        Polarity::Normal
    };
    let min_duty = int(cfg, "min_duty", 0, i64::from(FULL), 0)? as u16;
    let kick_ms = int(cfg, "kick_ms", 0, MAX_KICK_MS, 0)? as u64;
    let pulses_per_rev = int(cfg, "pulses_per_rev", 1, 64, 2)? as u32;
    Ok(Shape {
        period_ns: period_ns(frequency),
        polarity,
        min_duty,
        kick_ns: kick_ms * 1_000_000,
        pulses_per_rev,
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

/// A boolean setting, or `default` when it is not set.
fn flag(cfg: &Config<'_>, key: &str, default: bool) -> Result<bool> {
    match cfg.get(key) {
        None => Ok(default),
        Some(ConfigValue::Bool(value)) => Ok(value),
        Some(_) => Err(Status::INVALID_ARGUMENT),
    }
}
