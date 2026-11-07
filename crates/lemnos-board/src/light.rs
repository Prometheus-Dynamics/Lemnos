//! A light's settings in the board definition: geometry (`count`, `wire`,
//! `offset`, `direction`) and how intents look (`fade_ms`, `easing`,
//! effects, colours), as `lemnos_light::Defaults`.

use crate::BoardError;
use crate::schema::{ConfigValue, DeviceSpec};
use lemnos_drivers_ws2812::{Direction, StripConfig, Wire};
use lemnos_hal::ErrorKind;
use lemnos_light::{Defaults, Easing, EffectKind, LookName, Rgbw};

/// The look directory a moved look is in (`lemnosd`'s default).
pub const LOOKS_DIR: &str = "/etc/lemnos/looks.d";

/// The ws2812 keys of the system and link looks, retired: the looks are look
/// files now (`docs/looks.md`, the example set). A board that sets one is
/// refused, and the error names the look that holds the setting now.
pub const RETIRED_LIGHT_KEYS: &[(&str, &[&str])] = &[
    ("idle", &[]),
    ("locate", &["system.locate"]),
    ("locate_effect", &["system.locate"]),
    ("updating", &["system.writing"]),
    ("verifying", &["system.verifying"]),
    ("verifying_period_ms", &["system.verifying"]),
    ("verifying_tail", &["system.verifying"]),
    ("verifying_base", &["system.verifying"]),
    ("writing", &["system.writing-unknown"]),
    ("staged", &["system.staged"]),
    ("staged_period_ms", &["system.staged"]),
    ("staged_depth", &["system.staged"]),
    ("booting", &["system.booting"]),
    ("booting_period_ms", &["system.booting"]),
    ("booting_tail", &["system.booting"]),
    ("booting_base", &["system.booting"]),
    ("rebooting", &["system.rebooting"]),
    ("failed", &["system.failed", "system.rolled-back"]),
    ("failed_period_ms", &["system.failed", "system.rolled-back"]),
    ("failed_depth", &["system.failed", "system.rolled-back"]),
    ("confirmed", &["system.confirmed"]),
];

/// The refusal for a retired key a ws2812 device sets (`None` for any other
/// key): the device, the key, and where its setting now lives.
pub(crate) fn retired_problem(driver: &str, device: &str, key: &str) -> Option<String> {
    if driver != "ws2812" {
        return None;
    }
    let (_, looks) = RETIRED_LIGHT_KEYS.iter().find(|(k, _)| *k == key)?;
    Some(match looks {
        [] => format!(
            "{device:?}: config key `{key}` is retired; the idle look is set by `default_look`"
        ),
        [look] => format!(
            "{device:?}: config key `{key}` is retired; its look is `{look}` in {LOOKS_DIR}"
        ),
        many => {
            let names: Vec<String> = many.iter().map(|l| format!("`{l}`")).collect();
            format!(
                "{device:?}: config key `{key}` is retired; its looks are {} in {LOOKS_DIR}",
                names.join(" and ")
            )
        }
    })
}

/// The `config` keys of a light. The look defaults of the `status.*` looks and
/// the progress and spinner shapes; the system and link looks are look files
/// (`docs/looks.md`, the example set), so their keys are gone.
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
    "default_look",
    "absent_look",
    "absent_priority",
    "progress",
    "progress_background",
    "spinner_period_ms",
    "spinner_tail",
    "spinner_base",
    "gravity_device",
    "gravity_plane",
    "gravity_led0_deg",
    "default_down",
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

/// The priority of a light's absent look when the board gives none: under a
/// client's default (50).
pub const ABSENT_PRIORITY: u8 = 20;

/// A light's absent look, from its `config`: `absent_look` (a look name)
/// and `absent_priority` (0-255, default [`ABSENT_PRIORITY`]). The service
/// holds it as its own application intent at that priority for as long as
/// it runs, so any client above that priority (or in a higher layer) shows
/// over it, and the ring falls back to it, not to dark or to a lower
/// client, when they go. `None` when the board sets no `absent_look`.
pub fn light_absent(spec: &DeviceSpec) -> Result<Option<(LookName, u8)>, BoardError> {
    let priority = match uint(spec, "absent_priority")? {
        None => None,
        Some(v) => {
            Some(u8::try_from(v).map_err(|_| bad(spec, "absent_priority must be 0 to 255"))?)
        }
    };
    let Some(name) = spec.config.get("absent_look") else {
        if priority.is_some() {
            return Err(bad(spec, "absent_priority needs absent_look"));
        }
        return Ok(None);
    };
    let name = name.as_str().and_then(LookName::new).ok_or_else(|| {
        bad(
            spec,
            "absent_look must be a look name (lowercase letters, digits, '.', '-' and '_')",
        )
    })?;
    Ok(Some((name, priority.unwrap_or(ABSENT_PRIORITY))))
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
    if let Some(name) = spec.config.get("default_look") {
        d.default_look = name.as_str().and_then(LookName::new).ok_or_else(|| {
            bad(
                spec,
                "default_look must be a look name (lowercase letters, digits, '.', '-' and '_')",
            )
        })?;
    }
    if let Some(v) = uint(spec, "spinner_period_ms")? {
        d.spinner_period_ms = v;
    }
    if let Some(v) = uint(spec, "spinner_tail")? {
        d.spinner_tail = u8::try_from(v).map_err(|_| bad(spec, "spinner_tail is too large"))?;
    }
    if let Some(v) = fraction(spec, "spinner_base")? {
        d.spinner_base = v;
    }
    let colors: [(&str, &mut Rgbw); 6] = [
        ("ok", &mut d.ok),
        ("warn", &mut d.warn),
        ("error", &mut d.error),
        ("busy", &mut d.busy),
        ("progress", &mut d.progress),
        ("progress_background", &mut d.progress_background),
    ];
    for (key, slot) in colors {
        if let Some(c) = color(spec, key)? {
            *slot = c;
        }
    }
    Ok(d)
}

/// A light's ring geometry from its `config`: the physical LED that is
/// logical LED 0 (the ring's "top"), and whether logical indices run
/// clockwise. `None` when the `config` is not a valid strip.
pub fn light_geometry(spec: &DeviceSpec) -> Option<(u16, bool)> {
    strip_config(spec).ok().map(|s| {
        (
            s.offset,
            s.direction == lemnos_drivers_ws2812::Direction::Cw,
        )
    })
}
