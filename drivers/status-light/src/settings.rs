//! The board's settings for a status light, from its `config` table. Every key is optional but
//! `count`, and an unset key takes the default below. Nothing is saved: a restart reads the board
//! again.
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `count` | required | LEDs on the ring (1 to 65535). |
//! | `fade_ms` | 250 | Fade between looks, milliseconds. |
//! | `easing` | `ease-in-out` | Curve of fades and breathes: `linear`, `ease-in`, `ease-out`, `ease-in-out`, `sine`. |
//! | `status_effect` | `solid` | Effect of `ok`, `busy` and (unless `warn` has its own) `warn`: `solid`, `blink`, `breathe` or `chase`. |
//! | `error_effect` | `blink` | Effect of `error`, as above. |
//! | `breathe_period_ms`, `breathe_depth` | 2000, 0.6 | Breathe period (ms) and depth (0 to 1). |
//! | `blink_period_ms`, `blink_duty` | 1000, 0.5 | Blink period (ms) and on-fraction (0 to 1). |
//! | `look_brightness` | 0.5 | Brightness every look is scaled by (0 to 1). |
//! | `ok`, `warn`, `error`, `busy` | green, orange, red, blue | Colours as `0xRRGGBB`. |
//! | `default_look` | `status.ok` | Shown when no layer holds a look: `status.ok`, `status.warn`, `status.error`, `status.busy` or `status.off`. |
//! | `spinner_period_ms`, `spinner_tail`, `spinner_base` | 1200, 5, 0.18 | A chase's turn (ms), tail (LEDs) and floor (0 to 1). |

use lemnos_core::{Config, Result, Status};

use crate::builtin::StatusLook;
use crate::easing::Easing;
use crate::look::Rgbw;

/// What a status look's effect may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    Solid,
    Blink,
    Breathe,
    Chase,
}

impl EffectKind {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "solid" | "none" => Some(Self::Solid),
            "blink" => Some(Self::Blink),
            "breathe" | "pulse" => Some(Self::Breathe),
            "chase" | "spin" | "spinner" => Some(Self::Chase),
            _ => None,
        }
    }
}

/// The board's settings for one status light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub count: u16,
    pub fade_ms: u32,
    pub easing: Easing,
    pub status_effect: EffectKind,
    pub error_effect: EffectKind,
    pub breathe_period_ms: u32,
    /// Thousandths.
    pub breathe_depth: u16,
    pub blink_period_ms: u32,
    /// Thousandths of the period the look is on.
    pub blink_duty: u16,
    /// 0..=255, scaling every look.
    pub look_brightness: u8,
    pub ok: Rgbw,
    pub warn: Rgbw,
    pub error: Rgbw,
    pub busy: Rgbw,
    /// Shown when no layer holds a look.
    pub default_look: StatusLook,
    pub spinner_period_ms: u32,
    /// LEDs.
    pub spinner_tail: u8,
    /// Thousandths.
    pub spinner_base: u16,
}

impl Settings {
    /// Reads the settings from a device's `config`. Refused with `INVALID_ARGUMENT` when a key is
    /// missing its required value or is out of range.
    pub fn from_config(cfg: &Config<'_>) -> Result<Self> {
        let count = cfg
            .int("count")
            .and_then(|count| u16::try_from(count).ok())
            .filter(|&count| count > 0)
            .ok_or(Status::INVALID_ARGUMENT)?;
        Ok(Self {
            count,
            fade_ms: uint(cfg, "fade_ms", 250, 600_000)?,
            easing: match text(cfg, "easing")? {
                None => Easing::EaseInOut,
                Some(name) => Easing::parse(name).ok_or(Status::INVALID_ARGUMENT)?,
            },
            status_effect: effect(cfg, "status_effect", EffectKind::Solid)?,
            error_effect: effect(cfg, "error_effect", EffectKind::Blink)?,
            breathe_period_ms: period(cfg, "breathe_period_ms", 2_000)?,
            breathe_depth: fraction(cfg, "breathe_depth", 600)?,
            blink_period_ms: period(cfg, "blink_period_ms", 1_000)?,
            blink_duty: fraction(cfg, "blink_duty", 500)?,
            look_brightness: level(cfg, "look_brightness", 128)?,
            ok: colour(cfg, "ok", Rgbw::rgb(0x00ff00))?,
            warn: colour(cfg, "warn", Rgbw::rgb(0xff8000))?,
            error: colour(cfg, "error", Rgbw::rgb(0xff0000))?,
            busy: colour(cfg, "busy", Rgbw::rgb(0x0040ff))?,
            default_look: match text(cfg, "default_look")? {
                None => StatusLook::Ok,
                Some(name) => StatusLook::parse(name).ok_or(Status::INVALID_ARGUMENT)?,
            },
            spinner_period_ms: period(cfg, "spinner_period_ms", 1_200)?,
            spinner_tail: uint(cfg, "spinner_tail", 5, 64)? as u8,
            spinner_base: fraction(cfg, "spinner_base", 180)?,
        })
    }
}

/// A key's whole number, or `default` when unset. Refused when it is not a whole number up to `max`.
fn uint(cfg: &Config<'_>, key: &str, default: u32, max: u32) -> Result<u32> {
    match cfg.get(key) {
        None => Ok(default),
        Some(_) => cfg
            .int(key)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|&value| value <= max)
            .ok_or(Status::INVALID_ARGUMENT),
    }
}

/// A period in milliseconds (100 to 600000).
fn period(cfg: &Config<'_>, key: &str, default: u32) -> Result<u32> {
    let value = uint(cfg, key, default, 600_000)?;
    if value >= 100 {
        Ok(value)
    } else {
        Err(Status::INVALID_ARGUMENT)
    }
}

/// A number from 0 to 1 as thousandths, or `default` (thousandths) when unset.
fn fraction(cfg: &Config<'_>, key: &str, default: u16) -> Result<u16> {
    match cfg.get(key) {
        None => Ok(default),
        Some(_) => match cfg.float(key) {
            Some(value) if (0.0..=1.0).contains(&value) => Ok((value * 1000.0 + 0.5) as u16),
            _ => Err(Status::INVALID_ARGUMENT),
        },
    }
}

/// A number from 0 to 1 as a level (0..=255), or `default` when unset.
fn level(cfg: &Config<'_>, key: &str, default: u8) -> Result<u8> {
    match cfg.get(key) {
        None => Ok(default),
        Some(_) => match cfg.float(key) {
            Some(value) if (0.0..=1.0).contains(&value) => Ok((value * 255.0 + 0.5) as u8),
            _ => Err(Status::INVALID_ARGUMENT),
        },
    }
}

/// A colour as `0xRRGGBB`, or `default` when unset.
fn colour(cfg: &Config<'_>, key: &str, default: Rgbw) -> Result<Rgbw> {
    match cfg.get(key) {
        None => Ok(default),
        Some(_) => match cfg.int(key) {
            Some(value) if (0..=0xff_ffff).contains(&value) => Ok(Rgbw::rgb(value as u32)),
            _ => Err(Status::INVALID_ARGUMENT),
        },
    }
}

/// A string setting, or `None` when unset.
fn text<'a>(cfg: &Config<'a>, key: &str) -> Result<Option<&'a str>> {
    match cfg.get(key) {
        None => Ok(None),
        Some(_) => cfg.str(key).map(Some).ok_or(Status::INVALID_ARGUMENT),
    }
}

/// A status effect, or `default` when unset.
fn effect(cfg: &Config<'_>, key: &str, default: EffectKind) -> Result<EffectKind> {
    match text(cfg, key)? {
        None => Ok(default),
        Some(name) => EffectKind::parse(name).ok_or(Status::INVALID_ARGUMENT),
    }
}
