//! The fan controller's board entry: the `[[composites]]` table, its checks,
//! and the parsed settings `lemnosd` runs (`docs/composite-devices.md`).
//!
//! ```toml
//! [[composites]]
//! id = "fan-control"
//! driver = "fan-controller"
//! config = { input_cpu = "cpu-thermal.temperature", output_fan = "fan", floor = 300, hysteresis = 3000, slew = 50, kick_ms = 0, max_age_ms = 3000, curve_cpu = [[30000, 300], [60000, 600], [75000, 1000]] }
//! ```
//!
//! The entry is a device table (`DeviceSpec`), so it reuses the device
//! deserializer. Roles are `input_<role>` (`<device>.<channel>`) and
//! `output_<role>` (a device; the controller takes only `output_fan`), each
//! with a `curve_<role>` for an input. Scalars are the other keys.

use crate::schema::{BoardDefinition, ConfigValue, DeviceSpec};
use lemnos_device::fan_control::{self, Point, Settings};
use std::collections::BTreeMap;

/// The driver id of the fan controller composite.
pub const FAN_CONTROLLER: &str = "fan-controller";
/// The writer name `lemnosd` uses for the controller's writes to the fan
/// (`writers` of the fan must admit it, or be empty).
pub const FAN_WRITER: &str = "lemnosd.fan";
/// The output role the controller drives.
pub const FAN_ROLE: &str = "fan";

const SCALARS: &[&str] = &["floor", "hysteresis", "slew", "kick_ms", "max_age_ms"];

/// One input of the controller, resolved from the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanInput {
    pub role: String,
    /// The device whose channel is read.
    pub device: String,
    /// The channel's name, as the device reports it (`temperature`).
    pub channel: String,
    pub points: Vec<Point>,
    pub max_age_ms: u32,
}

/// A parsed fan controller: its settings, its inputs and the fan it drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanControllerSpec {
    pub id: String,
    pub settings: Settings,
    /// In role order (the board's config keys are sorted).
    pub inputs: Vec<FanInput>,
    /// The fan device the controller drives.
    pub fan: String,
}

fn int(config: &BTreeMap<String, ConfigValue>, key: &str) -> Result<Option<i64>, String> {
    match config.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("config {key:?} must be an integer")),
    }
}

fn points(value: &ConfigValue, key: &str) -> Result<Vec<Point>, String> {
    let ConfigValue::List(items) = value else {
        return Err(format!(
            "config {key:?} must be a list of [value, duty] pairs"
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let ConfigValue::List(pair) = item else {
            return Err(format!("config {key:?}: pairs are [value, duty]"));
        };
        let (Some(value), Some(duty)) = (
            pair.first().and_then(ConfigValue::as_i64),
            pair.get(1).and_then(ConfigValue::as_i64),
        ) else {
            return Err(format!("config {key:?}: pairs are two integers"));
        };
        let value =
            i32::try_from(value).map_err(|_| format!("config {key:?}: value out of range"))?;
        let duty = i32::try_from(duty).map_err(|_| format!("config {key:?}: duty out of range"))?;
        out.push(Point { value, duty });
    }
    Ok(out)
}

/// Parses a `fan-controller` entry against the board's devices. Returns every
/// problem found, not just the first.
#[inline(never)]
pub fn parse(board: &BoardDefinition, spec: &DeviceSpec) -> Result<FanControllerSpec, Vec<String>> {
    let id = &spec.id;
    let config = &spec.config;
    let mut problems = Vec::new();
    let device_exists = |name: &str| board.devices.iter().any(|d| d.id == name);

    let mut inputs = Vec::new();
    let mut fan = None;
    for (key, value) in config {
        if let Some(role) = key.strip_prefix("input_") {
            let Some(reference) = value.as_str() else {
                problems.push(format!(
                    "composite {id:?}: {key} must be <device>.<channel>"
                ));
                continue;
            };
            let Some((device, channel)) = reference.split_once('.') else {
                problems.push(format!(
                    "composite {id:?}: {key} must be <device>.<channel>"
                ));
                continue;
            };
            if !device_exists(device) {
                problems.push(format!(
                    "composite {id:?}: {key} names no device {device:?}"
                ));
            }
            let curve = format!("curve_{role}");
            let points = match config.get(&curve) {
                None => {
                    problems.push(format!("composite {id:?}: missing config {curve:?}"));
                    Vec::new()
                }
                Some(value) => match points(value, &curve) {
                    Ok(points) => match fan_control::check_curve(&points) {
                        Ok(()) => points,
                        Err(error) => {
                            problems.push(format!("composite {id:?}: {curve:?}: {error:?}"));
                            Vec::new()
                        }
                    },
                    Err(error) => {
                        problems.push(format!("composite {id:?}: {error}"));
                        Vec::new()
                    }
                },
            };
            inputs.push(FanInput {
                role: role.to_string(),
                device: device.to_string(),
                channel: channel.to_string(),
                points,
                max_age_ms: 3_000,
            });
        } else if let Some(role) = key.strip_prefix("output_") {
            match (role == FAN_ROLE, value.as_str()) {
                (true, Some(device)) => {
                    if !device_exists(device) {
                        problems.push(format!("composite {id:?}: fan {device:?} is not a device"));
                    }
                    fan = Some(device.to_string());
                }
                _ => problems.push(format!(
                    "composite {id:?}: {key} is not supported (the controller drives output_{FAN_ROLE})"
                )),
            }
        } else if !key.starts_with("curve_") && !SCALARS.contains(&key.as_str()) {
            problems.push(format!("composite {id:?}: unknown config key {key:?}"));
        }
    }
    for key in config.keys().filter(|k| k.starts_with("curve_")) {
        let role = &key["curve_".len()..];
        if !inputs.iter().any(|i| i.role == role) {
            problems.push(format!("composite {id:?}: {key:?} has no input_{role}"));
        }
    }
    if inputs.is_empty() || inputs.len() > fan_control::MAX_INPUTS {
        problems.push(format!(
            "composite {id:?}: 1 to {} inputs, found {}",
            fan_control::MAX_INPUTS,
            inputs.len()
        ));
    }
    let Some(fan) = fan else {
        problems.push(format!("composite {id:?}: output_{FAN_ROLE} is required"));
        return Err(problems);
    };

    let mut settings = Settings {
        floor: 0,
        hysteresis: 0,
        slew_per_s: 50,
        kick_ms: 0,
    };
    let mut max_age = 3_000i64;
    for (key, low, high) in [
        ("floor", 0i64, 1000i64),
        ("hysteresis", 0, i64::from(i32::MAX)),
        ("slew", 0, 1000),
        ("kick_ms", 0, i64::from(u32::MAX)),
        ("max_age_ms", 1, i64::from(u32::MAX)),
    ] {
        match int(config, key) {
            Ok(Some(v)) if (low..=high).contains(&v) => match key {
                "floor" => settings.floor = v as i32,
                "hysteresis" => settings.hysteresis = v as i32,
                "slew" => settings.slew_per_s = v as i32,
                "kick_ms" => settings.kick_ms = v as u32,
                _ => max_age = v,
            },
            Ok(Some(_)) => {
                problems.push(format!("composite {id:?}: config {key:?} is out of range"))
            }
            Ok(None) => {}
            Err(error) => problems.push(format!("composite {id:?}: {error}")),
        }
    }
    for input in &mut inputs {
        input.max_age_ms = max_age as u32;
    }

    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(FanControllerSpec {
        id: id.clone(),
        settings,
        inputs,
        fan,
    })
}

/// Checks every `[[composites]]` entry: unique ids, known drivers, and the
/// parse above. Returns the problems for [`BoardDefinition::validate`].
#[inline(never)]
pub(crate) fn check(board: &BoardDefinition) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, spec) in board.composites.iter().enumerate() {
        if board.devices.iter().any(|d| d.id == spec.id)
            || board.composites[..index].iter().any(|c| c.id == spec.id)
        {
            problems.push(format!("composite {:?}: id is already used", spec.id));
        }
        if spec.driver != FAN_CONTROLLER {
            problems.push(format!(
                "composite {:?}: unknown driver {:?} (composites: {FAN_CONTROLLER:?})",
                spec.id, spec.driver
            ));
            continue;
        }
        match parse(board, spec) {
            Err(errors) => problems.extend(errors),
            Ok(parsed) => {
                let fan = board.devices.iter().find(|d| d.id == parsed.fan);
                if let Some(fan) = fan
                    && !fan.writers.is_empty()
                    && !fan
                        .writers
                        .iter()
                        .any(|w| crate::client_matches(w, FAN_WRITER))
                {
                    problems.push(format!(
                        "composite {:?}: fan {:?} writers do not admit {FAN_WRITER:?}",
                        spec.id, parsed.fan
                    ));
                }
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "fan-test"
[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = { type = "cpu-thermal" }
[[devices]]
id = "fan"
driver = "hwmon-fan"
match = { name = "pwmfan" }
writers = ["lemnosd.fan"]
[[composites]]
id = "fan-control"
driver = "fan-controller"
config = { input_cpu = "cpu-thermal.temperature", output_fan = "fan", floor = 300, hysteresis = 3000, slew = 50, curve_cpu = [[30000, 300], [60000, 600], [75000, 1000]] }
"#;

    fn board(extra: &str) -> BoardDefinition {
        BoardDefinition::from_toml_str(&format!("{BOARD}{extra}")).unwrap()
    }

    #[test]
    fn a_valid_entry_parses_to_settings_and_inputs() {
        let b = board("");
        let parsed = parse(&b, &b.composites[0]).unwrap();
        assert_eq!(parsed.fan, "fan");
        assert_eq!(parsed.settings.floor, 300);
        assert_eq!(parsed.settings.hysteresis, 3_000);
        assert_eq!(parsed.inputs[0].device, "cpu-thermal");
        assert_eq!(parsed.inputs[0].channel, "temperature");
        assert_eq!(parsed.inputs[0].points.len(), 3);
        assert!(check(&b).is_empty());
    }

    #[test]
    fn problems_are_reported_together() {
        let mut b = board("");
        let config = &mut b.composites[0].config;
        config.insert(
            "curve_cpu".into(),
            ConfigValue::List(vec![ConfigValue::List(vec![
                ConfigValue::Integer(5),
                ConfigValue::Integer(2000),
            ])]),
        );
        config.insert("bogus".into(), ConfigValue::Integer(1));
        config.insert("input_board".into(), ConfigValue::String("nope".into()));
        let problems = check(&b).join("\n");
        assert!(problems.contains("Duty"), "{problems}");
        assert!(
            problems.contains("unknown config key \"bogus\""),
            "{problems}"
        );
        assert!(
            problems.contains("input_board must be <device>.<channel>"),
            "{problems}"
        );
    }

    #[test]
    fn the_fan_must_admit_the_controller() {
        let mut b = board("");
        b.devices[1].writers = vec!["app".into()];
        let problems = check(&b).join("\n");
        assert!(
            problems.contains("do not admit \"lemnosd.fan\""),
            "{problems}"
        );
    }

    #[test]
    fn an_unknown_driver_is_refused() {
        let mut b = board("");
        b.composites[0].driver = "fusion".into();
        assert!(check(&b)[0].contains("unknown driver"));
    }
}
