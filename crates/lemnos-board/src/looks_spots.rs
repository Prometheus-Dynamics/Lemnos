//! The `spots` block of a look: its file form, read and written (see
//! `docs/looks.md`, *Spots*). Kept apart from `looks.rs` for its size.

use super::looks::{
    Errors, color, color_text, err, fraction, join, number, only_keys, required_color, thousandths,
    whole,
};
use lemnos_light::{
    Block, DEFAULT_FOLLOW_MS, DEFAULT_WIDTH, MAX_SPOTS, Spot, SpotList, SpotSource, SpotsSpec,
};

/// The keys of a `spots` layer, besides the layer's own.
pub(super) const SPOTS_KEYS: &[&str] = &[
    "block",
    "color",
    "width",
    "follow_ms",
    "max_spots",
    "spots",
    "brightness",
    "mode",
];

/// The keys of one fixed spot.
const SPOT_KEYS: &[&str] = &["at", "level", "color"];

/// A `spots` block: `color` (required), `width` (LEDs, 1.5), `follow_ms`
/// (120), `max_spots` (8), and `spots`: `"input"` (the request's spots) or
/// a list of `{ at = 0.25, level = 1.0, color = "..." }`.
pub(super) fn spots(
    source: &str,
    key: &str,
    table: &toml::Table,
    errors: &mut Errors,
) -> Option<Block> {
    let color = required_color(source, key, table, errors);
    let width = match table.get("width") {
        None => Some(DEFAULT_WIDTH),
        Some(v) => number(v)
            .filter(|w| *w > 0.0 && *w <= 64.0)
            .map(|w| (w * 1000.0).round() as u16),
    };
    if width.is_none() {
        errors.push(err(
            source,
            &join(key, "width"),
            "must be a number of LEDs above 0 and at most 64",
        ));
    }
    let follow_ms = match table.get("follow_ms") {
        None => Some(DEFAULT_FOLLOW_MS),
        Some(v) => whole(v, 10, 10_000).map(|f| f as u32),
    };
    if follow_ms.is_none() {
        errors.push(err(source, &join(key, "follow_ms"), "must be 10 to 10000"));
    }
    let max_spots = match table.get("max_spots") {
        None => Some(MAX_SPOTS as u8),
        Some(v) => whole(v, 1, MAX_SPOTS as i64).map(|m| m as u8),
    };
    if max_spots.is_none() {
        errors.push(err(source, &join(key, "max_spots"), "must be 1 to 8"));
    }
    let source_kind = match table.get("spots") {
        None => {
            errors.push(err(
                source,
                &join(key, "spots"),
                "needs spots: \"input\" (from the request) or a list of { at, level }",
            ));
            None
        }
        Some(v) if v.as_str() == Some("input") => Some(SpotSource::Input),
        Some(v) => list(source, &join(key, "spots"), v, errors).map(SpotSource::Fixed),
    };
    match (color, width, follow_ms, max_spots, source_kind) {
        (Some(color), Some(width), Some(follow_ms), Some(max_spots), Some(source)) => {
            Some(Block::Spots(SpotsSpec {
                color,
                width,
                follow_ms,
                max_spots,
                source,
            }))
        }
        _ => None,
    }
}

/// A fixed list of spots: an array of tables.
fn list(source: &str, key: &str, value: &toml::Value, errors: &mut Errors) -> Option<SpotList> {
    let Some(items) = value.as_array() else {
        errors.push(err(
            source,
            key,
            "must be \"input\" or an array of { at, level } tables",
        ));
        return None;
    };
    if items.len() > MAX_SPOTS {
        errors.push(err(
            source,
            key,
            format!("has {} spots; a spots block has at most 8", items.len()),
        ));
        return None;
    }
    let mut out = Vec::with_capacity(items.len());
    let mut ok = true;
    for (index, item) in items.iter().enumerate() {
        let here = format!("{key}[{index}]");
        let Some(table) = item.as_table() else {
            errors.push(err(source, &here, "must be a table: { at, level }"));
            ok = false;
            continue;
        };
        only_keys(source, &here, table, SPOT_KEYS, errors);
        let at = match table.get("at") {
            None => {
                errors.push(err(
                    source,
                    &join(&here, "at"),
                    "needs at: 0 to 1 of the ring",
                ));
                None
            }
            Some(v) => fraction(v),
        };
        if table.contains_key("at") && at.is_none() {
            errors.push(err(
                source,
                &join(&here, "at"),
                "must be a number from 0 to 1",
            ));
        }
        let level = match table.get("level") {
            None => Some(1000),
            Some(v) => fraction(v),
        };
        if level.is_none() {
            errors.push(err(
                source,
                &join(&here, "level"),
                "must be a number from 0 to 1",
            ));
        }
        let spot_color = match table.get("color") {
            None => None,
            Some(v) => match color(v) {
                Some(c) => Some(c),
                None => {
                    errors.push(err(
                        source,
                        &join(&here, "color"),
                        "must be a colour: \"rrggbb\" (or \"wwrrggbb\")",
                    ));
                    ok = false;
                    None
                }
            },
        };
        match (at, level) {
            (Some(at), Some(level)) => out.push(Spot {
                at,
                level,
                color: spot_color,
            }),
            _ => ok = false,
        }
    }
    ok.then(|| SpotList::new(&out))
}

/// The block's file form, as a TOML inline table's fields.
pub(super) fn parts(spots: &SpotsSpec) -> Vec<String> {
    let mut parts = vec![
        "block = \"spots\"".to_string(),
        format!("color = \"{}\"", color_text(spots.color)),
        format!("width = {}", thousandths(spots.width)),
        format!("follow_ms = {}", spots.follow_ms),
        format!("max_spots = {}", spots.max_spots),
    ];
    match &spots.source {
        SpotSource::Input => parts.push("spots = \"input\"".to_string()),
        SpotSource::Fixed(list) => {
            let items: Vec<String> = list.iter().map(spot_text).collect();
            parts.push(format!("spots = [{}]", items.join(", ")));
        }
    }
    parts
}

/// One fixed spot as a TOML inline table.
fn spot_text(spot: &Spot) -> String {
    let mut out = format!(
        "{{ at = {}, level = {}",
        thousandths(spot.at),
        thousandths(spot.level)
    );
    if let Some(c) = spot.color {
        out.push_str(&format!(", color = \"{}\"", color_text(c)));
    }
    out.push_str(" }");
    out
}
