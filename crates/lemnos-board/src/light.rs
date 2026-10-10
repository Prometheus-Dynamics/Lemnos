//! A light's settings in the board definition: geometry (`count`, `wire`,
//! `offset`, `direction`) and how intents look (`fade_ms`, `easing`,
//! effects, colours), as `lemnos_light::Defaults`.

use crate::BoardError;
use crate::schema::{ConfigValue, DeviceSpec};
use lemnos_drivers_ws2812::{Direction, StripConfig, Wire};
use lemnos_hal::ErrorKind;
use lemnos_light::{Defaults, Easing, EffectKind, Rgbw};

/// The `config` keys of a light.
pub const LIGHT_KEYS: &[&str] = &[
    "count",
    "wire",
    "offset",
    "direction",
    "brightness",
    "look_brightness",
    "gpio",
    "fade_ms",
    "easing",
    "status_effect",
    "error_effect",
    "breathe_period_ms",
    "breathe_depth",
    "blink_period_ms",
    "blink_duty",
    "ok",
    "warn",
    "error",
    "busy",
    "locate",
    "locate_effect",
    "idle",
    "progress",
    "progress_background",
    "spinner_period_ms",
    "spinner_tail",
    "updating",
    "verifying",
    "verifying_period_ms",
    "verifying_tail",
    "verifying_base",
    "writing",
    "staged",
    "staged_period_ms",
    "staged_depth",
    "booting",
    "booting_period_ms",
    "booting_tail",
    "booting_base",
    "rebooting",
    "failed",
    "failed_period_ms",
    "failed_depth",
    "confirmed",
];

fn bad(spec: &DeviceSpec, reason: impl Into<String>) -> BoardError {
    BoardError::device(&spec.id, ErrorKind::Configuration, reason)
}

fn uint(spec: &DeviceSpec, key: &str) -> Result<Option<u32>, BoardError> {
    match spec.config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_i64()
            .and_then(|v| u32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| bad(spec, format!("{key} must be a non-negative integer"))),
    }
}

/// A fraction 0..1 as thousandths.
fn fraction(spec: &DeviceSpec, key: &str) -> Result<Option<u16>, BoardError> {
    match spec.config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|v| (0.0..=1.0).contains(v))
            .map(|v| Some((v * 1000.0).round() as u16))
            .ok_or_else(|| bad(spec, format!("{key} must be a number from 0 to 1"))),
    }
}

/// A colour: `0xRRGGBB` (white in a fourth byte, `0xWWRRGGBB`), or a string
/// `"#rrggbb"`.
fn color(spec: &DeviceSpec, key: &str) -> Result<Option<Rgbw>, BoardError> {
    let value = match spec.config.get(key) {
        None => return Ok(None),
        Some(ConfigValue::Integer(v)) => u32::try_from(*v).ok(),
        Some(ConfigValue::String(s)) => {
            u32::from_str_radix(s.trim_start_matches('#').trim_start_matches("0x"), 16).ok()
        }
        Some(_) => None,
    }
    .ok_or_else(|| {
        bad(
            spec,
            format!("{key} must be a colour (0xRRGGBB or \"#rrggbb\")"),
        )
    })?;
    Ok(Some(Rgbw::new(
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    )))
}

/// LEDs as thousandths: a number from 0 to 64 (`7`, `4.5`).
fn leds(spec: &DeviceSpec, key: &str) -> Result<Option<u16>, BoardError> {
    match spec.config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|v| (0.0..=64.0).contains(v))
            .map(|v| Some((v * 1000.0).round() as u16))
            .ok_or_else(|| bad(spec, format!("{key} must be a number of LEDs, 0 to 64"))),
    }
}

fn effect(spec: &DeviceSpec, key: &str) -> Result<Option<EffectKind>, BoardError> {
    match spec.config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .and_then(EffectKind::parse)
            .map(Some)
            .ok_or_else(|| {
                bad(
                    spec,
                    format!("{key} must be solid, blink, breathe or chase"),
                )
            }),
    }
}

/// The strip geometry and wire format of a `ws2812` device.
pub fn strip_config(spec: &DeviceSpec) -> Result<StripConfig, BoardError> {
    let count = uint(spec, "count")?.ok_or_else(|| bad(spec, "needs config.count (LEDs)"))?;
    let count = u16::try_from(count).map_err(|_| bad(spec, "count is too large"))?;
    let wire = match spec.config.get("wire").and_then(ConfigValue::as_str) {
        None => Wire::Rgb,
        Some(name) => Wire::from_name(name).ok_or_else(|| bad(spec, "wire must be rgb or rgbw"))?,
    };
    let direction = match spec.config.get("direction").and_then(ConfigValue::as_str) {
        None => Direction::Cw,
        Some(name) => {
            Direction::from_name(name).ok_or_else(|| bad(spec, "direction must be cw or ccw"))?
        }
    };
    let offset = uint(spec, "offset")?.unwrap_or(0);
    if count > 0 && offset >= u32::from(count) {
        return Err(bad(spec, "offset must be below count"));
    }
    let brightness = fraction(spec, "brightness")?.unwrap_or(1000);
    Ok(StripConfig::new(count, wire)
        .with_offset(offset as u16)
        .with_direction(direction)
        .with_brightness(((u32::from(brightness) * 255 + 500) / 1000) as u8))
}

/// How a light's intents look, from its `config` (unset keys keep
/// `Defaults::default()`).
pub fn light_defaults(spec: &DeviceSpec) -> Result<Defaults, BoardError> {
    let mut d = Defaults::default();
    if let Some(v) = uint(spec, "fade_ms")? {
        d.fade_ms = v;
    }
    if let Some(v) = fraction(spec, "look_brightness")? {
        d.look_brightness = ((u32::from(v) * 255 + 500) / 1000) as u8;
    }
    if let Some(v) = spec.config.get("easing") {
        d.easing = v
            .as_str()
            .and_then(Easing::parse)
            .ok_or_else(|| bad(spec, "easing must be linear, ease-in, ease-out, ease-in-out, sine or cubic-bezier(x1, y1, x2, y2)"))?;
    }
    if let Some(v) = effect(spec, "status_effect")? {
        d.status_effect = v;
    }
    if let Some(v) = effect(spec, "error_effect")? {
        d.error_effect = v;
    }
    if let Some(v) = effect(spec, "locate_effect")? {
        d.locate_effect = v;
    }
    if let Some(v) = uint(spec, "breathe_period_ms")? {
        d.breathe_period_ms = v;
    }
    if let Some(v) = fraction(spec, "breathe_depth")? {
        d.breathe_depth = v;
    }
    if let Some(v) = uint(spec, "blink_period_ms")? {
        d.blink_period_ms = v;
    }
    if let Some(v) = fraction(spec, "blink_duty")? {
        d.blink_duty = v;
    }
    if let Some(v) = uint(spec, "spinner_period_ms")? {
        d.spinner_period_ms = v;
    }
    if let Some(v) = uint(spec, "spinner_tail")? {
        d.spinner_tail = u8::try_from(v).map_err(|_| bad(spec, "spinner_tail is too large"))?;
    }
    if let Some(v) = uint(spec, "verifying_period_ms")? {
        d.verifying_period_ms = v;
    }
    if let Some(v) = leds(spec, "verifying_tail")? {
        d.verifying_tail = v;
    }
    if let Some(v) = fraction(spec, "verifying_base")? {
        d.verifying_base = v;
    }
    if let Some(v) = uint(spec, "booting_period_ms")? {
        d.booting_period_ms = v;
    }
    if let Some(v) = leds(spec, "booting_tail")? {
        d.booting_tail = v;
    }
    if let Some(v) = fraction(spec, "booting_base")? {
        d.booting_base = v;
    }
    if let Some(v) = uint(spec, "staged_period_ms")? {
        d.staged_period_ms = v;
    }
    if let Some(v) = fraction(spec, "staged_depth")? {
        d.staged_depth = v;
    }
    if let Some(v) = uint(spec, "failed_period_ms")? {
        d.failed_period_ms = v;
    }
    if let Some(v) = fraction(spec, "failed_depth")? {
        d.failed_depth = v;
    }
    let colors: [(&str, &mut Rgbw); 16] = [
        ("ok", &mut d.ok),
        ("warn", &mut d.warn),
        ("error", &mut d.error),
        ("busy", &mut d.busy),
        ("locate", &mut d.locate),
        ("idle", &mut d.idle),
        ("progress", &mut d.progress),
        ("progress_background", &mut d.progress_background),
        ("updating", &mut d.updating),
        ("verifying", &mut d.verifying),
        ("writing", &mut d.writing),
        ("staged", &mut d.staged),
        ("booting", &mut d.booting),
        ("rebooting", &mut d.rebooting),
        ("failed", &mut d.failed),
        ("confirmed", &mut d.confirmed),
    ];
    for (key, slot) in colors {
        if let Some(c) = color(spec, key)? {
            *slot = c;
        }
    }
    Ok(d)
}
