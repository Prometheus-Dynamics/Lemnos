//! Look definitions as text: the `[looks.<name>]` tables of a board
//! definition and of look files, the bare look body of `led show --spec`
//! and `--file`, and the JSON form Atlas sends. One validator for all of
//! them, with the file or key named in every error. [`to_toml`] writes a
//! look back in the file form.
//!
//! A look body:
//!
//! ```toml
//! envelope = { kind = "breathe", period_ms = 4000, depth = 0.18, easing = "ease-in-out" }
//! brightness = 0.7            # of the whole look (0..1, default 1)
//! min_brightness = 0.06       # never dimmer than this (0..1, default 0)
//! layers = [
//!   { block = "fill", color = "2f7bff", brightness = 1.0, mode = "max" },
//! ]
//! ```

use lemnos_light::{
    Block, Easing, Effect, Fraction, LayerSpec, LookSpec, MAX_FRAME_LEDS, MAX_LAYERS, Mode, Rgbw,
    valid_look_name,
};
use std::fmt::Write as _;

/// One problem in a look: where it is, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookError {
    /// The file, or where the look came from (`board.toml`, `--spec`).
    pub source: String,
    /// The look and key, `looks.<name>.layers[1].heads`, or empty for the
    /// whole file.
    pub key: String,
    pub reason: String,
}

impl std::fmt::Display for LookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.key.is_empty() {
            write!(f, "{}: {}", self.source, self.reason)
        } else {
            write!(f, "{}: {}: {}", self.source, self.key, self.reason)
        }
    }
}

type Errors = Vec<LookError>;

/// Parses a look file: `[looks.<name>]` tables and nothing else. Every
/// look is checked; one bad look makes the whole file fail (the caller keeps
/// what it had).
pub fn parse_file(source: &str, text: &str) -> Result<Vec<(String, LookSpec)>, Errors> {
    let table: toml::Table = toml::from_str(text).map_err(|e| {
        vec![LookError {
            source: source.into(),
            key: String::new(),
            reason: e.to_string(),
        }]
    })?;
    let mut errors = Vec::new();
    let mut looks = Vec::new();
    for (key, value) in &table {
        if key.as_str() != "looks" {
            errors.push(LookError {
                source: source.into(),
                key: key.clone(),
                reason: "unknown key (a look file has only [looks.<name>] tables)".into(),
            });
            continue;
        }
        let Some(named) = value.as_table() else {
            errors.push(LookError {
                source: source.into(),
                key: "looks".into(),
                reason: "must be a table of looks".into(),
            });
            continue;
        };
        for (name, body) in named {
            match from_value(source, name, body) {
                Ok(spec) => looks.push((name.clone(), spec)),
                Err(mut e) => errors.append(&mut e),
            }
        }
    }
    if errors.is_empty() {
        Ok(looks)
    } else {
        Err(errors)
    }
}

/// Parses a look body in TOML (`--spec`, a look's own text).
pub fn from_toml(source: &str, text: &str) -> Result<LookSpec, Errors> {
    let table: toml::Table = toml::from_str(text).map_err(|e| {
        vec![LookError {
            source: source.into(),
            key: String::new(),
            reason: e.to_string(),
        }]
    })?;
    body(source, "", &table)
}

/// Parses a look body in JSON (what Atlas sends).
pub fn from_json(source: &str, text: &str) -> Result<LookSpec, Errors> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        vec![LookError {
            source: source.into(),
            key: String::new(),
            reason: e.to_string(),
        }]
    })?;
    let table = <toml::Table as serde::Deserialize>::deserialize(value).map_err(|e| {
        vec![LookError {
            source: source.into(),
            key: String::new(),
            reason: e.to_string(),
        }]
    })?;
    body(source, "", &table)
}

/// Parses one named look's table.
pub fn from_value(source: &str, name: &str, value: &toml::Value) -> Result<LookSpec, Errors> {
    let prefix = format!("looks.{name}");
    let mut errors = Vec::new();
    if !valid_look_name(name) {
        errors.push(LookError {
            source: source.into(),
            key: prefix.clone(),
            reason: "a look name is 1 to 40 of a-z, 0-9, '.', '-' or '_'".into(),
        });
    }
    let Some(table) = value.as_table() else {
        errors.push(LookError {
            source: source.into(),
            key: prefix,
            reason: "must be a table".into(),
        });
        return Err(errors);
    };
    match body(source, &prefix, table) {
        Ok(spec) if errors.is_empty() => Ok(spec),
        Ok(_) => Err(errors),
        Err(mut e) => {
            errors.append(&mut e);
            Err(errors)
        }
    }
}

fn err(source: &str, key: &str, reason: impl Into<String>) -> LookError {
    LookError {
        source: source.into(),
        key: key.into(),
        reason: reason.into(),
    }
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Keys a table may have; anything else is rejected with the list.
fn only_keys(
    source: &str,
    prefix: &str,
    table: &toml::Table,
    allowed: &[&str],
    errors: &mut Errors,
) {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.push(err(
                source,
                &join(prefix, key),
                format!("unknown key (allowed: {})", allowed.join(", ")),
            ));
        }
    }
}

/// A fraction 0..1 as thousandths.
fn fraction(value: &toml::Value) -> Option<u16> {
    let v = number(value)?;
    (0.0..=1.0)
        .contains(&v)
        .then(|| (v * 1000.0).round() as u16)
}

/// A fraction 0..1 as a byte (0..=255).
fn brightness(value: &toml::Value) -> Option<u8> {
    let v = number(value)?;
    (0.0..=1.0).contains(&v).then(|| (v * 255.0).round() as u8)
}

fn number(value: &toml::Value) -> Option<f64> {
    match value {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) => f.is_finite().then_some(*f),
        _ => None,
    }
}

fn whole(value: &toml::Value, min: i64, max: i64) -> Option<i64> {
    value.as_integer().filter(|v| (min..=max).contains(v))
}

/// A colour: `"rrggbb"` or `"#rrggbb"` (white 0), `"wwrrggbb"`, or an
/// integer `0xWWRRGGBB`.
fn color(value: &toml::Value) -> Option<Rgbw> {
    let v = match value {
        toml::Value::Integer(i) => u32::try_from(*i).ok()?,
        toml::Value::String(s) => {
            let hex = s.strip_prefix('#').unwrap_or(s);
            if !(hex.len() == 6 || hex.len() == 8) {
                return None;
            }
            u32::from_str_radix(hex, 16).ok()?
        }
        _ => return None,
    };
    Some(Rgbw::new(
        (v >> 16) as u8,
        (v >> 8) as u8,
        v as u8,
        (v >> 24) as u8,
    ))
}

fn color_text(c: Rgbw) -> String {
    if c.w == 0 {
        format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        format!("{:02x}{:02x}{:02x}{:02x}", c.w, c.r, c.g, c.b)
    }
}

const BODY_KEYS: &[&str] = &["layers", "envelope", "brightness", "min_brightness"];

/// A look's body: its layers, envelope, brightness and floor.
fn body(source: &str, prefix: &str, table: &toml::Table) -> Result<LookSpec, Errors> {
    let mut errors = Vec::new();
    only_keys(source, prefix, table, BODY_KEYS, &mut errors);
    let mut spec = LookSpec::EMPTY;

    match table.get("layers").map(|v| (v, v.as_array())) {
        None => errors.push(err(
            source,
            &join(prefix, "layers"),
            "needs layers (1 to 4)",
        )),
        Some((_, None)) => errors.push(err(
            source,
            &join(prefix, "layers"),
            "must be an array of layer tables",
        )),
        Some((_, Some(layers))) if layers.is_empty() || layers.len() > MAX_LAYERS => {
            errors.push(err(
                source,
                &join(prefix, "layers"),
                format!("has {} layers; a look has 1 to {MAX_LAYERS}", layers.len()),
            ));
        }
        Some((_, Some(layers))) => {
            for (index, layer) in layers.iter().enumerate() {
                let key = format!("{}[{index}]", join(prefix, "layers"));
                match layer.as_table() {
                    Some(t) => match layer_spec(source, &key, t) {
                        Ok(l) => {
                            spec.push(l);
                        }
                        Err(mut e) => errors.append(&mut e),
                    },
                    None => errors.push(err(source, &key, "must be a table")),
                }
            }
        }
    }

    if let Some(value) = table.get("envelope") {
        match envelope(source, &join(prefix, "envelope"), value) {
            Ok(e) => spec.envelope = e,
            Err(mut e) => errors.append(&mut e),
        }
    }
    if let Some(value) = table.get("brightness") {
        match brightness(value) {
            Some(b) => spec.brightness = b,
            None => errors.push(err(
                source,
                &join(prefix, "brightness"),
                "must be a number from 0 to 1",
            )),
        }
    }
    if let Some(value) = table.get("min_brightness") {
        match brightness(value) {
            Some(b) => spec.floor = b,
            None => errors.push(err(
                source,
                &join(prefix, "min_brightness"),
                "must be a number from 0 to 1",
            )),
        }
    }

    if errors.is_empty()
        && let Err(reason) = spec.validate()
    {
        errors.push(err(source, &join(prefix, "layers"), reason));
    }
    if errors.is_empty() {
        Ok(spec)
    } else {
        Err(errors)
    }
}

fn envelope(source: &str, key: &str, value: &toml::Value) -> Result<Effect, Errors> {
    // A bare name is that envelope with its defaults.
    let table = match value {
        toml::Value::String(name) => {
            let mut t = toml::Table::new();
            t.insert("kind".into(), toml::Value::String(name.clone()));
            t
        }
        toml::Value::Table(t) => t.clone(),
        _ => {
            return Err(vec![err(
                source,
                key,
                "must be a table, or solid, blink or breathe",
            )]);
        }
    };
    let mut errors = Vec::new();
    let kind = table
        .get("kind")
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    let effect = match kind {
        "solid" => {
            only_keys(source, key, &table, &["kind"], &mut errors);
            Some(Effect::Solid)
        }
        "blink" => {
            only_keys(
                source,
                key,
                &table,
                &["kind", "period_ms", "duty"],
                &mut errors,
            );
            let period = period(source, key, &table, 1000, &mut errors);
            let duty = table.get("duty").map_or(Some(500), fraction);
            if duty.is_none() {
                errors.push(err(
                    source,
                    &join(key, "duty"),
                    "must be a number from 0 to 1",
                ));
            }
            period
                .zip(duty)
                .map(|(period_ms, duty)| Effect::Blink { period_ms, duty })
        }
        "breathe" => {
            only_keys(
                source,
                key,
                &table,
                &["kind", "period_ms", "depth", "easing"],
                &mut errors,
            );
            let period = period(source, key, &table, 2000, &mut errors);
            let depth = table.get("depth").map_or(Some(600), fraction);
            if depth.is_none() {
                errors.push(err(
                    source,
                    &join(key, "depth"),
                    "must be a number from 0 to 1",
                ));
            }
            let easing = match table.get("easing") {
                None => Some(Easing::EaseInOut),
                Some(v) => v.as_str().and_then(Easing::parse),
            };
            if easing.is_none() {
                errors.push(err(
                    source,
                    &join(key, "easing"),
                    "must be linear, ease-in, ease-out, ease-in-out, sine or cubic-bezier(x1, y1, x2, y2)",
                ));
            }
            match (period, depth, easing) {
                (Some(period_ms), Some(depth), Some(easing)) => Some(Effect::Breathe {
                    period_ms,
                    depth,
                    easing,
                }),
                _ => None,
            }
        }
        _ => {
            errors.push(err(
                source,
                &join(key, "kind"),
                "must be solid, blink or breathe",
            ));
            None
        }
    };
    match (effect, errors.is_empty()) {
        (Some(effect), true) => Ok(effect),
        _ => Err(errors),
    }
}

/// A period in milliseconds (the default when unset), within the range a look
/// can run at.
fn period(
    source: &str,
    key: &str,
    table: &toml::Table,
    default: i64,
    errors: &mut Errors,
) -> Option<u32> {
    let key_name = join(key, "period_ms");
    match table.get("period_ms") {
        None => Some(default as u32),
        Some(value) => match whole(value, 100, 600_000) {
            Some(v) => Some(v as u32),
            None => {
                errors.push(err(
                    source,
                    &key_name,
                    "must be a whole number of milliseconds from 100 to 600000",
                ));
                None
            }
        },
    }
}

fn layer_spec(source: &str, key: &str, table: &toml::Table) -> Result<LayerSpec, Errors> {
    let mut errors = Vec::new();
    let Some(block_name) = table.get("block").and_then(toml::Value::as_str) else {
        return Err(vec![err(
            source,
            &join(key, "block"),
            "needs block: fill, comet, arc, ripple or frame",
        )]);
    };
    let allowed: &[&str] = match block_name {
        "fill" => &["block", "color", "brightness", "mode"],
        "comet" => &[
            "block",
            "color",
            "period_ms",
            "tail",
            "heads",
            "base",
            "reverse",
            "brightness",
            "mode",
        ],
        "arc" => &[
            "block",
            "fraction",
            "color",
            "track",
            "head",
            "sheen",
            "brightness",
            "mode",
        ],
        "ripple" => &[
            "block",
            "color",
            "origin",
            "speed",
            "width",
            "settle_ms",
            "glow",
            "brightness",
            "mode",
        ],
        "frame" => &["block", "pixels", "brightness", "mode"],
        other => {
            return Err(vec![err(
                source,
                &join(key, "block"),
                format!("unknown block {other:?} (fill, comet, arc, ripple or frame)"),
            )]);
        }
    };
    // Unknown keys first: they are the likeliest mistake.
    only_keys(source, key, table, allowed, &mut errors);
    let block = match block_name {
        "fill" => fill(source, key, table, &mut errors),
        "comet" => comet(source, key, table, &mut errors),
        "arc" => arc(source, key, table, &mut errors),
        "ripple" => ripple(source, key, table, &mut errors),
        _ => frame(source, key, table, &mut errors),
    };
    let layer_brightness = match table.get("brightness") {
        None => Some(255),
        Some(v) => brightness(v),
    };
    if layer_brightness.is_none() {
        errors.push(err(
            source,
            &join(key, "brightness"),
            "must be a number from 0 to 1",
        ));
    }
    let mode = match table.get("mode") {
        None => Some(Mode::Max),
        Some(v) => v.as_str().and_then(Mode::parse),
    };
    if mode.is_none() {
        errors.push(err(source, &join(key, "mode"), "must be max or add"));
    }
    match (block, layer_brightness, mode, errors.is_empty()) {
        (Some(block), Some(brightness), Some(mode), true) => Ok(LayerSpec {
            block,
            brightness,
            mode,
        }),
        _ => Err(errors),
    }
}

fn required_color(
    source: &str,
    key: &str,
    table: &toml::Table,
    errors: &mut Errors,
) -> Option<Rgbw> {
    match table.get("color") {
        None => {
            errors.push(err(source, &join(key, "color"), "needs a colour"));
            None
        }
        Some(v) => match color(v) {
            Some(c) => Some(c),
            None => {
                errors.push(err(
                    source,
                    &join(key, "color"),
                    "must be a colour: \"rrggbb\" (or \"wwrrggbb\")",
                ));
                None
            }
        },
    }
}

fn optional_color(
    source: &str,
    key: &str,
    table: &toml::Table,
    name: &str,
    default: Rgbw,
    errors: &mut Errors,
) -> Option<Rgbw> {
    match table.get(name) {
        None => Some(default),
        Some(v) => match color(v) {
            Some(c) => Some(c),
            None => {
                errors.push(err(
                    source,
                    &join(key, name),
                    "must be a colour: \"rrggbb\" (or \"wwrrggbb\")",
                ));
                None
            }
        },
    }
}

fn optional_fraction(
    source: &str,
    key: &str,
    table: &toml::Table,
    name: &str,
    default: u16,
    errors: &mut Errors,
) -> Option<u16> {
    match table.get(name) {
        None => Some(default),
        Some(v) => match fraction(v) {
            Some(f) => Some(f),
            None => {
                errors.push(err(
                    source,
                    &join(key, name),
                    "must be a number from 0 to 1",
                ));
                None
            }
        },
    }
}

fn fill(source: &str, key: &str, table: &toml::Table, errors: &mut Errors) -> Option<Block> {
    required_color(source, key, table, errors).map(|color| Block::Fill { color })
}

fn comet(source: &str, key: &str, table: &toml::Table, errors: &mut Errors) -> Option<Block> {
    let color = required_color(source, key, table, errors);
    let period_ms = period(source, key, table, 1200, errors);
    let tail = match table.get("tail") {
        None => Some(5_000),
        Some(v) => number(v)
            .filter(|t| *t > 0.0 && *t <= MAX_FRAME_LEDS as f64)
            .map(|t| (t * 1000.0).round() as u16),
    };
    if tail.is_none() {
        errors.push(err(
            source,
            &join(key, "tail"),
            "must be a number of LEDs above 0 and at most 64",
        ));
    }
    let heads = match table.get("heads") {
        None => Some(1),
        Some(v) => whole(v, 1, 2).map(|h| h as u8),
    };
    if heads.is_none() {
        errors.push(err(source, &join(key, "heads"), "must be 1 or 2"));
    }
    let base = optional_fraction(source, key, table, "base", 0, errors);
    let reverse = match table.get("reverse") {
        None => Some(false),
        Some(v) => v.as_bool(),
    };
    if reverse.is_none() {
        errors.push(err(source, &join(key, "reverse"), "must be true or false"));
    }
    match (color, period_ms, tail, heads, base, reverse) {
        (Some(color), Some(period_ms), Some(tail), Some(heads), Some(base), Some(reverse)) => {
            Some(Block::Comet {
                color,
                period_ms,
                tail,
                heads,
                base,
                reverse,
            })
        }
        _ => None,
    }
}

fn arc(source: &str, key: &str, table: &toml::Table, errors: &mut Errors) -> Option<Block> {
    let color = required_color(source, key, table, errors);
    let track = optional_color(source, key, table, "track", Rgbw::rgb(0x101012), errors);
    let head = optional_fraction(source, key, table, "head", 350, errors);
    let fraction = match table.get("fraction") {
        None => Some(Fraction::Input),
        Some(v) if v.as_str() == Some("input") => Some(Fraction::Input),
        Some(v) => fraction(v).map(Fraction::Fixed),
    };
    if fraction.is_none() {
        errors.push(err(
            source,
            &join(key, "fraction"),
            "must be a number from 0 to 1, or \"input\"",
        ));
    }
    let sheen = match table.get("sheen") {
        None => Some(true),
        Some(v) => v.as_bool(),
    };
    if sheen.is_none() {
        errors.push(err(source, &join(key, "sheen"), "must be true or false"));
    }
    match (fraction, color, track, head, sheen) {
        (Some(fraction), Some(color), Some(track), Some(head), Some(sheen)) => Some(Block::Arc {
            fraction,
            color,
            track,
            head,
            sheen,
        }),
        _ => None,
    }
}

fn ripple(source: &str, key: &str, table: &toml::Table, errors: &mut Errors) -> Option<Block> {
    let color = required_color(source, key, table, errors);
    let origin = match table.get("origin") {
        None => Some(0),
        Some(v) => whole(v, 0, (MAX_FRAME_LEDS - 1) as i64).map(|o| o as u16),
    };
    if origin.is_none() {
        errors.push(err(
            source,
            &join(key, "origin"),
            "must be an LED from 0 to 63",
        ));
    }
    let speed = leds_per_second(table, "speed", 12.0, source, key, errors);
    let width = leds_per_second(table, "width", 2.2, source, key, errors);
    let settle_ms = match table.get("settle_ms") {
        None => Some(1400),
        Some(v) => whole(v, 0, 60_000).map(|s| s as u32),
    };
    if settle_ms.is_none() {
        errors.push(err(
            source,
            &join(key, "settle_ms"),
            "must be a whole number of milliseconds, 0 to 60000",
        ));
    }
    let glow = optional_fraction(source, key, table, "glow", 150, errors);
    match (color, origin, speed, width, settle_ms, glow) {
        (Some(color), Some(origin), Some(speed), Some(width), Some(settle_ms), Some(glow)) => {
            Some(Block::Ripple {
                origin,
                color,
                speed,
                width,
                settle_ms,
                glow,
            })
        }
        _ => None,
    }
}

/// LEDs (a speed or a width), as thousandths.
fn leds_per_second(
    table: &toml::Table,
    name: &str,
    default: f64,
    source: &str,
    key: &str,
    errors: &mut Errors,
) -> Option<u16> {
    let value = match table.get(name) {
        None => return Some((default * 1000.0).round() as u16),
        Some(v) => number(v)
            .filter(|v| *v > 0.0 && *v <= MAX_FRAME_LEDS as f64)
            .map(|v| (v * 1000.0).round() as u16),
    };
    if value.is_none() {
        errors.push(err(
            source,
            &join(key, name),
            "must be a number above 0 and at most 64",
        ));
    }
    value
}

fn frame(source: &str, key: &str, table: &toml::Table, errors: &mut Errors) -> Option<Block> {
    let Some(pixels) = table.get("pixels").and_then(toml::Value::as_array) else {
        errors.push(err(
            source,
            &join(key, "pixels"),
            "needs pixels: an array of colours",
        ));
        return None;
    };
    if pixels.is_empty() || pixels.len() > MAX_FRAME_LEDS {
        errors.push(err(
            source,
            &join(key, "pixels"),
            format!("has {} pixels; a frame has 1 to 64", pixels.len()),
        ));
        return None;
    }
    let mut out = [Rgbw::OFF; MAX_FRAME_LEDS];
    for (index, value) in pixels.iter().enumerate() {
        match color(value) {
            Some(c) => out[index] = c,
            None => {
                errors.push(err(
                    source,
                    &format!("{}[{index}]", join(key, "pixels")),
                    "must be a colour",
                ));
                return None;
            }
        }
    }
    Some(Block::Frame {
        pixels: out,
        len: pixels.len() as u8,
    })
}

/// A look as its file form: `[looks.<name>]` and the body. Reading it back
/// with [`parse_file`] gives the same look.
pub fn to_toml(name: &str, spec: &LookSpec) -> String {
    let mut out = format!("[looks.{}]\n", quote_key(name));
    out.push_str(&body_toml(spec));
    out
}

/// A look's body as TOML (what `led show --spec` takes).
pub fn body_toml(spec: &LookSpec) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "envelope = {}", envelope_text(&spec.envelope));
    let _ = writeln!(out, "brightness = {}", fraction_text(spec.brightness));
    let _ = writeln!(out, "min_brightness = {}", fraction_text(spec.floor));
    out.push_str("layers = [\n");
    for layer in spec.iter() {
        let _ = writeln!(out, "  {},", layer_text(layer));
    }
    out.push_str("]\n");
    out
}

fn quote_key(name: &str) -> String {
    if name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

/// A brightness byte as a fraction, three decimals.
fn fraction_text(b: u8) -> String {
    format!("{:.3}", f64::from(b) / 255.0)
}

/// Thousandths as a number of three decimals.
fn thousandths(v: u16) -> String {
    format!("{:.3}", f64::from(v) / 1000.0)
}

/// Signed thousandths (a Bézier's control point may be below 0).
fn signed_thousandths(v: i16) -> String {
    format!("{:.3}", f64::from(v) / 1000.0)
}

fn envelope_text(effect: &Effect) -> String {
    match *effect {
        Effect::Solid => "{ kind = \"solid\" }".into(),
        Effect::Blink { period_ms, duty } => format!(
            "{{ kind = \"blink\", period_ms = {period_ms}, duty = {} }}",
            thousandths(duty)
        ),
        Effect::Breathe {
            period_ms,
            depth,
            easing,
        } => format!(
            "{{ kind = \"breathe\", period_ms = {period_ms}, depth = {}, easing = \"{}\" }}",
            thousandths(depth),
            easing_text(easing)
        ),
    }
}

fn easing_text(easing: Easing) -> String {
    match easing {
        Easing::CubicBezier { x1, y1, x2, y2 } => format!(
            "cubic-bezier({}, {}, {}, {})",
            thousandths(x1),
            signed_thousandths(y1),
            thousandths(x2),
            signed_thousandths(y2)
        ),
        other => other.name().into(),
    }
}

fn layer_text(layer: &LayerSpec) -> String {
    let mut parts: Vec<String> = Vec::new();
    match layer.block {
        Block::Fill { color } => {
            parts.push("block = \"fill\"".into());
            parts.push(format!("color = \"{}\"", color_text(color)));
        }
        Block::Comet {
            color,
            period_ms,
            tail,
            heads,
            base,
            reverse,
        } => {
            parts.push("block = \"comet\"".into());
            parts.push(format!("color = \"{}\"", color_text(color)));
            parts.push(format!("period_ms = {period_ms}"));
            parts.push(format!("tail = {}", thousandths(tail)));
            parts.push(format!("heads = {heads}"));
            parts.push(format!("base = {}", thousandths(base)));
            parts.push(format!("reverse = {reverse}"));
        }
        Block::Arc {
            fraction,
            color,
            track,
            head,
            sheen,
        } => {
            parts.push("block = \"arc\"".into());
            match fraction {
                Fraction::Fixed(f) => parts.push(format!("fraction = {}", thousandths(f))),
                Fraction::Input => parts.push("fraction = \"input\"".into()),
            }
            parts.push(format!("color = \"{}\"", color_text(color)));
            parts.push(format!("track = \"{}\"", color_text(track)));
            parts.push(format!("head = {}", thousandths(head)));
            parts.push(format!("sheen = {sheen}"));
        }
        Block::Ripple {
            origin,
            color,
            speed,
            width,
            settle_ms,
            glow,
        } => {
            parts.push("block = \"ripple\"".into());
            parts.push(format!("color = \"{}\"", color_text(color)));
            parts.push(format!("origin = {origin}"));
            parts.push(format!("speed = {}", thousandths(speed)));
            parts.push(format!("width = {}", thousandths(width)));
            parts.push(format!("settle_ms = {settle_ms}"));
            parts.push(format!("glow = {}", thousandths(glow)));
        }
        Block::Frame { pixels, len } => {
            parts.push("block = \"frame\"".into());
            let list: Vec<String> = pixels[..usize::from(len)]
                .iter()
                .map(|c| format!("\"{}\"", color_text(*c)))
                .collect();
            parts.push(format!("pixels = [{}]", list.join(", ")));
        }
    }
    if layer.brightness != 255 {
        parts.push(format!("brightness = {}", fraction_text(layer.brightness)));
    }
    if layer.mode != Mode::Max {
        parts.push(format!("mode = \"{}\"", layer.mode.name()));
    }
    format!("{{ {} }}", parts.join(", "))
}
