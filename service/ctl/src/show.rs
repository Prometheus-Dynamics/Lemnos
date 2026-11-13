//! `lemnos-ctl show DEVICE`: a device as a panel, built from its description alone. It holds no
//! code for any kind of device or driver: the name, kind, bus, address and state come from the
//! description the host serves, and each value's unit, range, limit and reading type come from
//! its items. A test greps this file for the names of drivers and contracts to keep it so.

use std::fmt::Write as _;

use lemnos_core::{ItemKind, UnitState};
use lemnos_wire::{DeviceDescription, OwnedItem};

/// The panel for a described device, one line each.
pub fn panel(description: &DeviceDescription) -> Vec<String> {
    let mut lines = Vec::new();
    let name = if description.label.is_empty() {
        "(unnamed)"
    } else {
        description.label.as_str()
    };
    lines.push(name.to_string());
    lines.push(format!("  kind     {}", or_unknown(&description.kind)));
    lines.push(format!("  bus      {}", bus_text(description)));
    lines.push(format!(
        "  address  {}",
        description
            .address
            .map_or_else(|| "unknown".to_string(), |a| format!("{a:#04x}"))
    ));
    if !description.location.is_empty() {
        lines.push(format!("  location {}", description.location));
    }
    lines.push(format!("  state    {}", state_text(description)));
    if !description.contracts.is_empty() {
        lines.push(format!("  provides {}", contract_list(description)));
    }
    for (heading, kind) in [
        ("values", ItemKind::Value),
        ("controls", ItemKind::Control),
        ("actions", ItemKind::Action),
    ] {
        let items: Vec<&OwnedItem> = description
            .items
            .iter()
            .filter(|i| i.kind == kind)
            .collect();
        if items.is_empty() {
            continue;
        }
        lines.push(heading.to_string());
        for item in items {
            lines.push(item_row(item));
        }
    }
    lines
}

fn or_unknown(text: &str) -> &str {
    if text.is_empty() { "unknown" } else { text }
}

fn bus_text(description: &DeviceDescription) -> String {
    if description.bus.is_empty() {
        "none".to_string()
    } else {
        description.bus.clone()
    }
}

fn state_text(description: &DeviceDescription) -> String {
    let state = match description.state {
        Some(0) => "running",
        Some(1) => "retrying",
        Some(2) => "off",
        _ => "unknown",
    };
    if description.reason.is_empty() {
        state.to_string()
    } else {
        format!("{state} ({})", description.reason)
    }
}

fn contract_list(description: &DeviceDescription) -> String {
    description
        .contracts
        .iter()
        .map(|c| c.tag.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One item: its name, its unit, its range and step, its warning limits and its reading type.
/// A figure the device does not give is written `unknown`, never guessed.
fn item_row(item: &OwnedItem) -> String {
    let mut row = format!("  {}", item.name);
    // Three states: a unit, a dimensionless count or switch, and a unit the driver did not say.
    let unit = match item.unit_state {
        UnitState::Known => item.unit.clone(),
        UnitState::Dimensionless => "dimensionless".to_string(),
        UnitState::Unknown => "unknown".to_string(),
    };
    let _ = write!(row, "  unit {unit}");
    let range = match (item.min, item.max) {
        (Some(min), Some(max)) => format!(
            "{}..{}",
            scaled(min, item.exponent),
            scaled(max, item.exponent)
        ),
        _ => "unknown".to_string(),
    };
    let _ = write!(row, "  range {range}");
    if let Some(step) = item.step {
        let _ = write!(row, "  step {}", scaled(step, item.exponent));
    }
    let limit = match (item.warn_low, item.warn_high) {
        (None, None) => "none".to_string(),
        (low, high) => {
            let mut parts = Vec::new();
            if let Some(low) = low {
                parts.push(format!("below {}", scaled(low, item.exponent)));
            }
            if let Some(high) = high {
                parts.push(format!("above {}", scaled(high, item.exponent)));
            }
            parts.join(", ")
        }
    };
    let _ = write!(row, "  limit {limit}");
    let reading = item.reading.map_or("unknown", |r| reading_name(r));
    let _ = write!(row, "  type {reading}");
    if let Some(group) = &item.group {
        let axis = item.field.as_deref().unwrap_or("");
        let _ = write!(row, "  group {group} {axis}");
    }
    row
}

fn reading_name(reading: lemnos_core::Reading) -> &'static str {
    use lemnos_core::Reading;
    match reading {
        Reading::Number => "number",
        Reading::Range => "range",
        Reading::Switch => "switch",
        Reading::Text => "text",
        Reading::ColourArray => "colour array",
        Reading::ThreeAxes => "three axes",
    }
}

/// A raw figure in its item's unit: `raw * 10^exponent`, written without a float's noise.
fn scaled(raw: i64, exponent: i8) -> String {
    if exponent >= 0 {
        return format!(
            "{}",
            raw.saturating_mul(10i64.saturating_pow(exponent as u32))
        );
    }
    let places = usize::from(exponent.unsigned_abs());
    let digits = raw.unsigned_abs().to_string();
    let padded = format!("{digits:0>width$}", width = places + 1);
    let (whole, fraction) = padded.split_at(padded.len() - places);
    let sign = if raw < 0 { "-" } else { "" };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

#[cfg(test)]
mod tests {
    use super::scaled;

    #[test]
    fn a_raw_figure_is_written_in_its_unit() {
        assert_eq!(scaled(1000, -3), "1");
        assert_eq!(scaled(1500, -3), "1.5");
        assert_eq!(scaled(-1250, -3), "-1.25");
        assert_eq!(scaled(7, -3), "0.007");
        assert_eq!(scaled(42, 0), "42");
        assert_eq!(scaled(3, 2), "300");
    }
}

#[cfg(test)]
mod location_tests {
    use super::panel;
    use lemnos_wire::DeviceDescription;

    #[test]
    fn the_panel_gives_a_location_only_when_the_driver_names_one() {
        let mut description = DeviceDescription {
            label: "fan".into(),
            ..DeviceDescription::default()
        };
        assert!(
            !panel(&description)
                .iter()
                .any(|line| line.contains("location"))
        );
        description.location = "/sys/class/hwmon/hwmon2".into();
        assert!(
            panel(&description)
                .iter()
                .any(|line| line == "  location /sys/class/hwmon/hwmon2")
        );
    }
}

#[cfg(test)]
mod unit_tests {
    use super::item_row;
    use lemnos_core::{ItemKind, UnitState};
    use lemnos_wire::OwnedItem;

    fn row(unit: &str, state: UnitState) -> String {
        let item = OwnedItem {
            kind: ItemKind::Value,
            name: "x".into(),
            unit: unit.into(),
            unit_state: state,
            exponent: 0,
            min: None,
            max: None,
            contract: 0,
            method: 0,
            field: None,
            reading: None,
            step: None,
            warn_low: None,
            warn_high: None,
            group: None,
        };
        item_row(&item)
    }

    #[test]
    fn the_three_unit_states_render_three_ways() {
        assert!(row("V", UnitState::Known).contains("unit V"));
        assert!(row("", UnitState::Dimensionless).contains("unit dimensionless"));
        assert!(row("", UnitState::Unknown).contains("unit unknown"));
    }
}
