//! The devices of a camera chain, taken from what the board declares.
//!
//! A proof tool names no device. It starts from the sensor (given on the command line, or the
//! one device the board tags as a camera sensor) and follows the board's `link` from one device to
//! the next, exactly as `lemnos-ctl chain SENSOR` does ([`lemnos_client::follow`]): the sensor, its
//! receiver, the receiver's front end, the front end's back end. Each stage can be named on the
//! command line instead, and a name given there wins. A stage that cannot be resolved is an error
//! that says what was looked for, what the chain from the sensor holds, and what the board has.

use lemnos_client::{DeviceEntry, follow};
use lemnos_contract_camera_sensor::CameraSensor;
use lemnos_core::Contract;

/// One stage after the sensor: what to call it in a message, the flag that names it, and the name
/// the flag gave, if it did.
#[derive(Debug, Clone, Copy)]
pub struct Stage<'a> {
    pub label: &'static str,
    pub flag: &'static str,
    pub name: Option<&'a str>,
}

/// The names of the sensor and of each stage, in the order the stages were asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub sensor: String,
    pub stages: Vec<String>,
}

fn board_has(devices: &[DeviceEntry]) -> String {
    if devices.is_empty() {
        return "no devices".into();
    }
    devices
        .iter()
        .map(|d| d.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The sensor to start from: the one named, or the only device tagged as a camera sensor.
fn pick_sensor(devices: &[DeviceEntry], named: Option<&str>) -> Result<String, String> {
    if let Some(name) = named {
        return devices
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.name.clone())
            .ok_or_else(|| {
                format!(
                    "no device {name} (the sensor you named); the board has: {}",
                    board_has(devices)
                )
            });
    }
    let tag = <dyn CameraSensor as Contract>::TAG.as_str();
    let sensors: Vec<&DeviceEntry> = devices
        .iter()
        .filter(|d| d.tags.iter().any(|t| t == tag))
        .collect();
    match sensors.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => Err(format!(
            "looked for a device tagged {tag} (no --sensor given) and found none; the board has: {}",
            board_has(devices)
        )),
        many => Err(format!(
            "looked for the one device tagged {tag} (no --sensor given) and found {}: {}; name one with --sensor",
            many.len(),
            many.iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Resolves the sensor and `stages` against the board's device list.
///
/// Stage `i` is the device named by its flag; with no name it is the `i`-th device after the
/// sensor on the board's `link` chain.
///
/// # Errors
/// Names what was looked for and what the board has: a sensor or a named stage that is not on the
/// board, or a stage the chain does not reach.
pub fn resolve(
    devices: &[DeviceEntry],
    sensor: Option<&str>,
    stages: &[Stage<'_>],
) -> Result<Resolved, String> {
    let sensor = pick_sensor(devices, sensor)?;
    let chain: Vec<&str> = follow(devices, &sensor)
        .into_iter()
        .map(|d| d.name.as_str())
        .collect();
    let mut found = Vec::with_capacity(stages.len());
    for (i, stage) in stages.iter().enumerate() {
        let name = match stage.name {
            Some(name) => {
                if !devices.iter().any(|d| d.name == name) {
                    return Err(format!(
                        "no device {name} (the {} you named with {}); the board has: {}",
                        stage.label,
                        stage.flag,
                        board_has(devices)
                    ));
                }
                name.to_string()
            }
            None => chain
                .get(i + 1)
                .map(ToString::to_string)
                .ok_or_else(|| {
                    format!(
                        "looked for the {} as device {} of the chain that starts at {sensor} and follows each device's link (name it with {}); the chain holds: {}; the board has: {}",
                        stage.label,
                        i + 2,
                        stage.flag,
                        chain.join(" -> "),
                        board_has(devices)
                    )
                })?,
        };
        found.push(name);
    }
    Ok(Resolved {
        sensor,
        stages: found,
    })
}

/// Resolves the sensor and the ISP the chain ends at: the device named by `stage`'s flag, or the
/// last device of the chain from the sensor when that is at least the third (sensor, receiver, ISP).
///
/// # Errors
/// As [`resolve`].
pub fn resolve_end(
    devices: &[DeviceEntry],
    sensor: Option<&str>,
    stage: Stage<'_>,
) -> Result<Resolved, String> {
    let sensor = pick_sensor(devices, sensor)?;
    let chain: Vec<&str> = follow(devices, &sensor)
        .into_iter()
        .map(|d| d.name.as_str())
        .collect();
    let name = match (stage.name, chain.as_slice()) {
        (Some(_), _) => {
            return resolve(devices, Some(&sensor), &[stage]);
        }
        (None, [_, _, .., last]) => (*last).to_string(),
        (None, _) => {
            return Err(format!(
                "looked for the {} at the end of the chain that starts at {sensor} (sensor, receiver, then the ISPs; name it with {}); the chain holds: {}; the board has: {}",
                stage.label,
                stage.flag,
                chain.join(" -> "),
                board_has(devices)
            ));
        }
    };
    Ok(Resolved {
        sensor,
        stages: vec![name],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_client::{DeviceHealth, DeviceState};

    fn entry(index: u32, name: &str, tag: &str, link: Option<&str>) -> DeviceEntry {
        DeviceEntry {
            index,
            name: name.into(),
            driver: "x".into(),
            tags: vec![tag.into()],
            state: DeviceState::Running,
            health: DeviceHealth::Ok,
            reason: String::new(),
            link: link.map(Into::into),
        }
    }

    fn sensor_tag() -> &'static str {
        <dyn CameraSensor as Contract>::TAG.as_str()
    }

    fn board(isp: &str, back_link: Option<&str>) -> Vec<DeviceEntry> {
        vec![
            entry(0, "cam0-power", "power", None),
            entry(1, "cam0-sensor", sensor_tag(), Some("cam0-receiver")),
            entry(2, "cam0-receiver", "r", Some("cam0-fe")),
            entry(3, "cam0-fe", "i", Some(isp)),
            entry(4, isp, "i", back_link),
        ]
    }

    const STAGES: [Stage<'static>; 3] = [
        Stage {
            label: "receiver",
            flag: "--receiver",
            name: None,
        },
        Stage {
            label: "front end",
            flag: "--front-end",
            name: None,
        },
        Stage {
            label: "back end",
            flag: "--back-end",
            name: None,
        },
    ];

    #[test]
    fn the_chain_follows_the_boards_links_whatever_the_names_are() {
        for isp in ["cam0-isp", "cam0-be", "weird"] {
            let got = resolve(&board(isp, None), Some("cam0-sensor"), &STAGES).unwrap();
            assert_eq!(got.sensor, "cam0-sensor");
            assert_eq!(got.stages, ["cam0-receiver", "cam0-fe", isp]);
        }
    }

    #[test]
    fn the_only_sensor_is_found_by_its_tag_when_none_is_named() {
        let got = resolve(&board("cam0-isp", None), None, &STAGES).unwrap();
        assert_eq!(got.sensor, "cam0-sensor");
        let mut two = board("cam0-isp", None);
        two.push(entry(9, "cam1-sensor", sensor_tag(), None));
        let err = resolve(&two, None, &STAGES).unwrap_err();
        assert!(
            err.contains("found 2") && err.contains("cam1-sensor"),
            "{err}"
        );
        let none = vec![entry(0, "cam0-power", "power", None)];
        let err = resolve(&none, None, &STAGES).unwrap_err();
        assert!(
            err.contains("found none") && err.contains("cam0-power"),
            "{err}"
        );
    }

    #[test]
    fn an_override_wins_and_must_be_on_the_board() {
        let mut stages = STAGES;
        stages[2].name = Some("cam0-fe");
        let got = resolve(&board("cam0-isp", None), Some("cam0-sensor"), &stages).unwrap();
        assert_eq!(got.stages[2], "cam0-fe");
        stages[2].name = Some("cam0-be");
        let err = resolve(&board("cam0-isp", None), Some("cam0-sensor"), &stages).unwrap_err();
        assert!(
            err.contains("no device cam0-be")
                && err.contains("--back-end")
                && err.contains("cam0-isp"),
            "{err}"
        );
    }

    #[test]
    fn a_chain_that_ends_early_says_where_and_what_the_board_has() {
        let mut devices = board("cam0-isp", None);
        devices[2].link = None;
        let err = resolve(&devices, Some("cam0-sensor"), &STAGES).unwrap_err();
        assert!(
            err.contains("front end")
                && err.contains("cam0-sensor -> cam0-receiver")
                && err.contains("--front-end")
                && err.contains("cam0-isp"),
            "{err}"
        );
    }

    #[test]
    fn a_sensor_that_is_not_on_the_board_is_an_error_naming_the_board() {
        let err = resolve(&board("cam0-isp", None), Some("nope"), &STAGES).unwrap_err();
        assert!(
            err.contains("no device nope") && err.contains("cam0-fe"),
            "{err}"
        );
    }

    #[test]
    fn the_end_of_the_chain_is_the_isp_and_a_short_chain_is_an_error() {
        let end = Stage {
            label: "ISP",
            flag: "--isp",
            name: None,
        };
        let got = resolve_end(&board("cam0-isp", None), None, end).unwrap();
        assert_eq!(got.stages, ["cam0-isp"]);
        let mut short = board("cam0-isp", None);
        short[2].link = None;
        let err = resolve_end(&short, Some("cam0-sensor"), end).unwrap_err();
        assert!(
            err.contains("--isp") && err.contains("cam0-sensor -> cam0-receiver"),
            "{err}"
        );
        let named = Stage {
            name: Some("cam0-fe"),
            ..end
        };
        assert_eq!(
            resolve_end(&short, None, named).unwrap().stages,
            ["cam0-fe"]
        );
    }

    #[test]
    fn a_circle_of_links_ends_the_chain() {
        let mut devices = board("cam0-isp", Some("cam0-fe"));
        devices[4].link = Some("cam0-fe".into());
        let got = resolve(&devices, Some("cam0-sensor"), &STAGES).unwrap();
        assert_eq!(got.stages.len(), 3);
    }
}
