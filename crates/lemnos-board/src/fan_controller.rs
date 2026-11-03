//! The fan controller's board entry: the `[[composites]]` table, its checks,
//! and the parsed settings `lemnosd` runs (`docs/composite-devices.md`).
//!
//! ```toml
//! [[composites]]
//! id = "fan-control"
//! driver = "fan-controller"
//! inputs = { cpu = "cpu-thermal.temperature" }   # role -> <device>.<channel>
//! outputs = { fan = "fan" }                      # role -> the fan device
//! config = { floor = 700, hysteresis = 3000, slew = 50, kick_ms = 0, max_age_ms = 3000, curve_cpu = [[50000, 700], [65000, 850], [75000, 1000]] }
//! ```
//!
//! Duties are per-mille. A curve's values are in the input's unit. Every input
//! has a `curve_<role>` key; a role that is not an input is an error.

use crate::schema::{BoardDefinition, CompositeSpec, ConfigValue};
use lemnos_device::fan_control::{self, Point, Settings};

/// The driver id of the fan controller composite.
pub const FAN_CONTROLLER: &str = "fan-controller";
/// The writer name `lemnosd` uses for the controller's writes to the fan
/// (`writers` of the fan must admit it, or be empty).
pub const FAN_WRITER: &str = "lemnosd.fan";
/// The output role the controller drives.
pub const FAN_ROLE: &str = "fan";

const SCALAR_KEYS: &[&str] = &["floor", "hysteresis", "slew", "kick_ms", "max_age_ms"];

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
    pub inputs: Vec<FanInput>,
    /// The fan device the controller drives.
    pub fan: String,
}

fn int(
    config: &std::collections::BTreeMap<String, ConfigValue>,
    key: &str,
) -> Result<Option<i64>, String> {
    match config.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("config {key:?} must be an integer")),
    }
}

fn pairs(value: &ConfigValue, key: &str) -> Result<Vec<Point>, String> {
    let ConfigValue::List(items) = value else {
        return Err(format!(
            "config {key:?} must be a list of [value, duty] pairs"
        ));
    };
    items
        .iter()
        .map(|item| match item {
            ConfigValue::List(pair) if pair.len() == 2 => {
                let value = pair[0].as_i64();
                let duty = pair[1].as_i64();
                match (value, duty) {
                    (Some(value), Some(duty)) => Ok(Point {
                        value: i32::try_from(value)
                            .map_err(|_| format!("config {key:?}: value out of range"))?,
                        duty: i32::try_from(duty)
                            .map_err(|_| format!("config {key:?}: duty out of range"))?,
                    }),
                    _ => Err(format!("config {key:?}: pairs are integers")),
                }
            }
            _ => Err(format!("config {key:?}: pairs are [value, duty]")),
        })
        .collect()
}

/// Parses a `fan-controller` entry against the board's devices. Returns every
/// problem found, not just the first.
pub fn parse(
    board: &BoardDefinition,
    spec: &CompositeSpec,
) -> Result<FanControllerSpec, Vec<String>> {
    let mut problems = Vec::new();
    let device_exists = |id: &str| board.devices.iter().any(|d| d.id == id);
    let id = &spec.id;

    if spec.inputs.is_empty() || spec.inputs.len() > fan_control::MAX_INPUTS {
        problems.push(format!(
            "composite {id:?}: 1 to {} inputs, found {}",
            fan_control::MAX_INPUTS,
            spec.inputs.len()
        ));
    }
    match spec.outputs.get(FAN_ROLE) {
        Some(fan) if spec.outputs.len() == 1 => {
            if !device_exists(fan) {
                problems.push(format!("composite {id:?}: fan {fan:?} is not a device"));
            }
        }
        _ => problems.push(format!(
            "composite {id:?}: outputs must be exactly {{ {FAN_ROLE} = <device> }}"
        )),
    }
    for role in spec.config.keys() {
        let known = SCALAR_KEYS.contains(&role.as_str())
            || role
                .strip_prefix("curve_")
                .is_some_and(|r| spec.inputs.contains_key(r));
        if !known {
            problems.push(format!("composite {id:?}: unknown config key {role:?}"));
        }
    }

    let mut inputs = Vec::new();
    for (role, reference) in &spec.inputs {
        let Some((device, channel)) = reference.split_once('.') else {
            problems.push(format!(
                "composite {id:?}: input {role:?} must be <device>.<channel>"
            ));
            continue;
        };
        if !device_exists(device) {
            problems.push(format!(
                "composite {id:?}: input {role:?} names no device {device:?}"
            ));
        }
        let key = format!("curve_{role}");
        let points = match spec.config.get(&key) {
            None => {
                problems.push(format!("composite {id:?}: missing config {key:?}"));
                Vec::new()
            }
            Some(value) => match pairs(value, &key) {
                Ok(points) => match fan_control::check_curve(&points) {
                    Ok(()) => points,
                    Err(error) => {
                        problems.push(format!("composite {id:?}: {key:?}: {error:?}"));
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
            role: role.clone(),
            device: device.to_string(),
            channel: channel.to_string(),
            points,
            max_age_ms: 3_000,
        });
    }

    let mut settings = Settings {
        floor: 0,
        hysteresis: 0,
        slew_per_s: 50,
        kick_ms: 0,
    };
    let mut max_age = 3_000i64;
    let checks = [
        ("floor", 0i64, 1000i64),
        ("hysteresis", 0, i64::from(i32::MAX)),
        ("slew", 0, 1000),
        ("kick_ms", 0, i64::from(u32::MAX)),
        ("max_age_ms", 1, i64::from(u32::MAX)),
    ];
    for (key, low, high) in checks {
        match int(&spec.config, key) {
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
    inputs.sort_by(|a, b| a.role.cmp(&b.role));

    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(FanControllerSpec {
        id: id.clone(),
        settings,
        inputs,
        fan: spec.outputs[FAN_ROLE].clone(),
    })
}

/// Checks every `[[composites]]` entry: unique ids, known drivers, and the
/// parse above. Returns the problems for [`BoardDefinition::validate`].
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
inputs = { cpu = "cpu-thermal.temperature" }
outputs = { fan = "fan" }
config = { floor = 300, hysteresis = 3000, slew = 50, curve_cpu = [[30000, 300], [60000, 600], [75000, 1000]] }
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
        b.composites[0].config.insert(
            "curve_cpu".into(),
            ConfigValue::List(vec![ConfigValue::List(vec![
                ConfigValue::Integer(5),
                ConfigValue::Integer(2000),
            ])]),
        );
        b.composites[0]
            .config
            .insert("bogus".into(), ConfigValue::Integer(1));
        b.composites[0].inputs.insert("board".into(), "nope".into());
        let problems = check(&b).join("\n");
        assert!(problems.contains("Duty"), "{problems}");
        assert!(
            problems.contains("unknown config key \"bogus\""),
            "{problems}"
        );
        assert!(
            problems.contains("must be <device>.<channel>"),
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
