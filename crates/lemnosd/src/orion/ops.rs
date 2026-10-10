//! Orion actions on a resource, as lemnosd operations (`docs/orion.md`):
//! parsing the arguments and turning lemnosd's answers into outcomes. No I/O.

use std::collections::BTreeMap;

use lemnos_device::{CalibrationCommand, CalibrationRoutine, CalibrationStatus};
use lemnos_ipc::{ClientError, Refusal};
use orion_control_plane::TypedConfigValue;

/// The action names a device resource accepts.
pub const ACTIONS: [&str; 22] = [
    "set",
    "restore",
    "release",
    "read",
    "power.set",
    "power.reset",
    "calibration.start",
    "calibration.stop",
    "calibration.apply",
    "calibration.discard",
    "calibration.reset",
    "calibration.status",
    "looks.preset.list",
    "looks.preset.show",
    "looks.preset.apply",
    "looks.preset.save",
    "looks.preset.delete",
    "looks.show_inline",
    "looks.look",
    "looks.off",
    "looks.locate",
    "light.brightness",
];

/// A light's look actions (the status ring's resource; `looks.*` and
/// `light.brightness`). Bodies are checked when the action is parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum LookOp {
    PresetList,
    PresetShow(String),
    PresetApply(String),
    PresetSave {
        name: String,
        body: String,
    },
    PresetDelete(String),
    /// A look in full (a look file's body), shown until replaced or for
    /// `seconds`.
    ShowInline {
        body: String,
        seconds: Option<f64>,
    },
    /// A named look, shown until replaced or for `seconds`.
    ShowLook {
        name: String,
        seconds: Option<f64>,
    },
    /// Drops the caller's intents on the light.
    Off,
    Locate {
        seconds: f64,
    },
    /// The ring-wide look brightness (0 to 1).
    Brightness {
        value: f64,
        persist: bool,
    },
}

/// A parsed action.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// A light's look or brightness action.
    Look(LookOp),
    /// `set`: write `value` (in the control's unit) to `control`.
    Set { control: String, value: f64 },
    /// `restore`: undo this caller's writes (to `control`, or all of them).
    Restore { control: Option<String> },
    /// `release`: hand a fan back to the kernel's governor.
    Release,
    /// `read`: the device's latest reading.
    Read,
    /// A calibration action (`calibration.*`): a command, or the status.
    Calibration(CalibrationOp),
}

/// A calibration action on the device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalibrationOp {
    /// `calibration.start` (`routine`), `stop`, `apply`, `discard`, `reset`.
    Command(CalibrationCommand),
    /// `calibration.status`: the revision, the routine running, the parts'
    /// confidences (see [`calibration_output`]).
    Status,
}

/// How an action ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Succeeded(BTreeMap<String, TypedConfigValue>),
    /// The action ran and the device failed (a device error).
    Failed(String),
    /// The action was refused (bad arguments, a write policy, an unknown name).
    Rejected(String),
}

/// Parses `name` with `args`. `Err` is the refusal's reason.
pub fn parse(name: &str, args: &BTreeMap<String, TypedConfigValue>) -> Result<Op, String> {
    match name {
        "set" => {
            let control =
                text(args, "control").ok_or_else(|| "`control` (string) is required".to_owned())?;
            let value =
                number(args, "value").ok_or_else(|| "`value` (number) is required".to_owned())?;
            if !value.is_finite() {
                return Err("`value` must be finite".to_owned());
            }
            Ok(Op::Set { control, value })
        }
        "restore" => Ok(Op::Restore {
            control: text(args, "control"),
        }),
        "release" => Ok(Op::Release),
        "read" => Ok(Op::Read),
        // A power switch: `power.set {on}` writes `power.on`, `power.reset
        // {off_ms}` writes `power.reset` (the off time, 0 to 10000 ms).
        "power.set" => {
            let on = match args.get("on") {
                Some(TypedConfigValue::Bool(on)) => *on,
                _ => return Err("`on` (bool) is required".to_owned()),
            };
            Ok(Op::Set {
                control: "power.on".to_owned(),
                value: if on { 1.0 } else { 0.0 },
            })
        }
        "looks.preset.list" => Ok(Op::Look(LookOp::PresetList)),
        "looks.preset.show" => Ok(Op::Look(LookOp::PresetShow(name_arg(args)?))),
        "looks.preset.apply" => Ok(Op::Look(LookOp::PresetApply(name_arg(args)?))),
        "looks.preset.delete" => Ok(Op::Look(LookOp::PresetDelete(name_arg(args)?))),
        "looks.preset.save" => Ok(Op::Look(LookOp::PresetSave {
            name: name_arg(args)?,
            body: body_arg(args)?,
        })),
        "looks.show_inline" => {
            let body = body_arg(args)?;
            // Checked here, so a bad look is refused before lemnosd sees it.
            lemnos_board::looks::from_toml("orion", &body).map_err(|errors| {
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            })?;
            Ok(Op::Look(LookOp::ShowInline {
                body,
                seconds: seconds_arg(args)?,
            }))
        }
        "looks.look" => Ok(Op::Look(LookOp::ShowLook {
            name: name_arg(args)?,
            seconds: seconds_arg(args)?,
        })),
        "looks.off" => Ok(Op::Look(LookOp::Off)),
        "looks.locate" => Ok(Op::Look(LookOp::Locate {
            seconds: seconds_arg(args)?.unwrap_or(10.0),
        })),
        "light.brightness" => {
            let value = number(args, "value")
                .ok_or_else(|| "`value` (number, 0 to 1) is required".to_owned())?;
            if !(0.0..=1.0).contains(&value) {
                return Err("`value` must be 0 to 1".to_owned());
            }
            let persist = matches!(args.get("persist"), Some(TypedConfigValue::Bool(true)));
            Ok(Op::Look(LookOp::Brightness { value, persist }))
        }
        "power.reset" => {
            let off_ms = number(args, "off_ms").unwrap_or(1000.0);
            if !(0.0..=10_000.0).contains(&off_ms) {
                return Err("`off_ms` must be 0 to 10000".to_owned());
            }
            Ok(Op::Set {
                control: "power.reset".to_owned(),
                value: off_ms,
            })
        }
        "calibration.start" => {
            let name = text(args, "routine").ok_or_else(|| {
                "`routine` (accel-six, mag-rotate or gyro-hold) is required".to_owned()
            })?;
            let routine = CalibrationRoutine::from_name(&name)
                .ok_or_else(|| format!("unknown routine `{name}`"))?;
            Ok(Op::Calibration(CalibrationOp::Command(
                CalibrationCommand::Start(routine),
            )))
        }
        "calibration.stop" => Ok(Op::Calibration(CalibrationOp::Command(
            CalibrationCommand::Stop,
        ))),
        "calibration.apply" => Ok(Op::Calibration(CalibrationOp::Command(
            CalibrationCommand::Apply,
        ))),
        "calibration.discard" => Ok(Op::Calibration(CalibrationOp::Command(
            CalibrationCommand::Discard,
        ))),
        "calibration.reset" => Ok(Op::Calibration(CalibrationOp::Command(
            CalibrationCommand::Reset,
        ))),
        "calibration.status" => Ok(Op::Calibration(CalibrationOp::Status)),
        other => Err(format!(
            "unsupported action `{other}` (one of {})",
            ACTIONS.join(", ")
        )),
    }
}

/// A required name argument.
fn name_arg(args: &BTreeMap<String, TypedConfigValue>) -> Result<String, String> {
    text(args, "name").ok_or_else(|| "`name` (string) is required".to_owned())
}

/// A required body argument (a look's TOML).
fn body_arg(args: &BTreeMap<String, TypedConfigValue>) -> Result<String, String> {
    text(args, "body").ok_or_else(|| "`body` (string, a look in TOML) is required".to_owned())
}

/// An optional duration in seconds (above 0, at most a day).
fn seconds_arg(args: &BTreeMap<String, TypedConfigValue>) -> Result<Option<f64>, String> {
    match number(args, "seconds") {
        None => Ok(None),
        Some(s) if s > 0.0 && s <= 86_400.0 => Ok(Some(s)),
        Some(_) => Err("`seconds` must be above 0 and at most 86400".to_owned()),
    }
}

/// A string argument, if present and not empty.
pub fn text(args: &BTreeMap<String, TypedConfigValue>, key: &str) -> Option<String> {
    match args.get(key) {
        Some(TypedConfigValue::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

/// A numeric argument (any integer or float).
#[allow(clippy::cast_precision_loss)]
pub fn number(args: &BTreeMap<String, TypedConfigValue>, key: &str) -> Option<f64> {
    match args.get(key)? {
        TypedConfigValue::F64(value) => Some(*value),
        TypedConfigValue::Int(value) => Some(*value as f64),
        TypedConfigValue::UInt(value) => Some(*value as f64),
        _ => None,
    }
}

/// A refusal from lemnosd: a device error is `Failed`, the rest `Rejected`.
pub fn refusal(refusal: Refusal) -> Outcome {
    match refusal {
        Refusal::Device(_) => Outcome::Failed(refusal.to_string()),
        other => Outcome::Rejected(other.to_string()),
    }
}

/// A failed call to lemnosd: a refusal as [`refusal`], anything else (the
/// connection, a timeout) as `Failed`.
pub fn client_error(error: ClientError) -> Outcome {
    match error {
        ClientError::Refused(refused) => refusal(refused),
        other => Outcome::Failed(format!("lemnosd: {other}")),
    }
}

/// The output of `calibration.status`: `revision`, `running` (`none` or the
/// routine's name), `progress` (0 to 1), `candidate` and `failed`, then for
/// each part (`accel`, `gyro`, `mag`) its `samples`, `confidence`, `coverage`,
/// `residual` (ratios, 0 to 1) and `active`. A part the device does not have
/// reads all zeros.
pub fn calibration_output(status: &CalibrationStatus) -> BTreeMap<String, TypedConfigValue> {
    let ratio = |permille: u16| TypedConfigValue::F64(f64::from(permille) / 1000.0);
    let mut out = BTreeMap::new();
    out.insert(
        "revision".to_owned(),
        TypedConfigValue::UInt(u64::from(status.revision)),
    );
    let running = status.running.map_or("none", CalibrationRoutine::name);
    out.insert(
        "running".to_owned(),
        TypedConfigValue::String(running.to_owned()),
    );
    out.insert("progress".to_owned(), ratio(status.progress));
    out.insert(
        "candidate".to_owned(),
        TypedConfigValue::Bool(status.candidate),
    );
    out.insert("failed".to_owned(), TypedConfigValue::Bool(status.failed));
    let parts = [
        ("accel", lemnos_device::PART_ACCEL),
        ("gyro", lemnos_device::PART_GYRO),
        ("mag", lemnos_device::PART_MAG),
    ];
    for (name, index) in parts {
        let part = status.parts[index];
        out.insert(
            format!("{name}.samples"),
            TypedConfigValue::UInt(u64::from(part.samples)),
        );
        out.insert(format!("{name}.confidence"), ratio(part.confidence));
        out.insert(format!("{name}.coverage"), ratio(part.coverage));
        out.insert(format!("{name}.residual"), ratio(part.residual));
        out.insert(
            format!("{name}.active"),
            TypedConfigValue::Bool(part.active),
        );
    }
    out
}

/// The output of a successful `set`.
pub fn set_output(control: &str, applied: f64) -> BTreeMap<String, TypedConfigValue> {
    BTreeMap::from([
        (
            "control".to_owned(),
            TypedConfigValue::String(control.to_owned()),
        ),
        ("applied".to_owned(), TypedConfigValue::F64(applied)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, TypedConfigValue)]) -> BTreeMap<String, TypedConfigValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn set_needs_a_control_and_a_finite_value() {
        let ok = args(&[
            ("control", TypedConfigValue::String("duty".into())),
            ("value", TypedConfigValue::UInt(1)),
        ]);
        assert_eq!(
            parse("set", &ok),
            Ok(Op::Set {
                control: "duty".into(),
                value: 1.0
            })
        );
        assert!(parse("set", &args(&[("value", TypedConfigValue::F64(1.0))])).is_err());
        let nan = args(&[
            ("control", TypedConfigValue::String("duty".into())),
            ("value", TypedConfigValue::F64(f64::NAN)),
        ]);
        assert!(parse("set", &nan).is_err());
    }

    #[test]
    fn restore_takes_an_optional_control_and_others_take_none() {
        assert_eq!(
            parse("restore", &BTreeMap::new()),
            Ok(Op::Restore { control: None })
        );
        assert_eq!(parse("release", &BTreeMap::new()), Ok(Op::Release));
        assert_eq!(parse("read", &BTreeMap::new()), Ok(Op::Read));
        assert!(parse("calibrate", &BTreeMap::new()).is_err());
    }

    #[test]
    fn power_actions_map_to_the_switch_controls() {
        let on = args(&[("on", TypedConfigValue::Bool(false))]);
        assert_eq!(
            parse("power.set", &on),
            Ok(Op::Set {
                control: "power.on".into(),
                value: 0.0
            })
        );
        assert!(parse("power.set", &BTreeMap::new()).is_err());
        let reset = args(&[("off_ms", TypedConfigValue::UInt(250))]);
        assert_eq!(
            parse("power.reset", &reset),
            Ok(Op::Set {
                control: "power.reset".into(),
                value: 250.0
            })
        );
        assert_eq!(
            parse("power.reset", &BTreeMap::new()),
            Ok(Op::Set {
                control: "power.reset".into(),
                value: 1000.0
            })
        );
        let too_long = args(&[("off_ms", TypedConfigValue::UInt(99_999))]);
        assert!(parse("power.reset", &too_long).is_err());
    }

    #[test]
    fn look_actions_parse_and_refuse_bad_arguments() {
        let name = args(&[("name", TypedConfigValue::String("scheme-c".into()))]);
        assert_eq!(
            parse("looks.preset.apply", &name),
            Ok(Op::Look(LookOp::PresetApply("scheme-c".into())))
        );
        assert!(parse("looks.preset.apply", &BTreeMap::new()).is_err());
        assert_eq!(
            parse("looks.preset.list", &BTreeMap::new()),
            Ok(Op::Look(LookOp::PresetList))
        );
        // A look in full is checked before lemnosd sees it.
        let good = args(&[(
            "body",
            TypedConfigValue::String("layers = [{ block = \"fill\", color = \"28c8ff\" }]".into()),
        )]);
        assert!(matches!(
            parse("looks.show_inline", &good),
            Ok(Op::Look(LookOp::ShowInline { .. }))
        ));
        let bad = args(&[(
            "body",
            TypedConfigValue::String("layers = [{ block = \"nope\" }]".into()),
        )]);
        assert!(parse("looks.show_inline", &bad).is_err());
        // Locate defaults to ten seconds; a day is the most.
        assert_eq!(
            parse("looks.locate", &BTreeMap::new()),
            Ok(Op::Look(LookOp::Locate { seconds: 10.0 }))
        );
        let long = args(&[("seconds", TypedConfigValue::UInt(100_000))]);
        assert!(parse("looks.locate", &long).is_err());
        // Brightness is 0 to 1, optionally persisted.
        let half = args(&[
            ("value", TypedConfigValue::F64(0.5)),
            ("persist", TypedConfigValue::Bool(true)),
        ]);
        assert_eq!(
            parse("light.brightness", &half),
            Ok(Op::Look(LookOp::Brightness {
                value: 0.5,
                persist: true
            }))
        );
        let over = args(&[("value", TypedConfigValue::F64(1.5))]);
        assert!(parse("light.brightness", &over).is_err());
    }

    #[test]
    fn device_errors_fail_and_refusals_reject() {
        assert!(matches!(refusal(Refusal::OutOfRange), Outcome::Rejected(_)));
        assert!(matches!(
            refusal(Refusal::Device(lemnos_ipc::ErrorKind::Busy)),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            client_error(ClientError::Timeout),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn calibration_actions_parse_and_name_their_routine() {
        assert_eq!(
            parse(
                "calibration.start",
                &args(&[("routine", TypedConfigValue::String("mag-rotate".into()))])
            ),
            Ok(Op::Calibration(CalibrationOp::Command(
                CalibrationCommand::Start(CalibrationRoutine::MagRotate)
            )))
        );
        assert!(parse("calibration.start", &BTreeMap::new()).is_err());
        assert!(
            parse(
                "calibration.start",
                &args(&[("routine", TypedConfigValue::String("spin".into()))])
            )
            .is_err()
        );
        for (name, command) in [
            ("calibration.stop", CalibrationCommand::Stop),
            ("calibration.apply", CalibrationCommand::Apply),
            ("calibration.discard", CalibrationCommand::Discard),
            ("calibration.reset", CalibrationCommand::Reset),
        ] {
            assert_eq!(
                parse(name, &BTreeMap::new()),
                Ok(Op::Calibration(CalibrationOp::Command(command)))
            );
        }
        assert_eq!(
            parse("calibration.status", &BTreeMap::new()),
            Ok(Op::Calibration(CalibrationOp::Status))
        );
        assert_eq!(ACTIONS.len(), 22);
    }

    #[test]
    fn calibration_status_reports_confidences_as_ratios() {
        let mut status = CalibrationStatus {
            revision: 3,
            running: Some(CalibrationRoutine::AccelSix),
            progress: 250,
            candidate: true,
            failed: false,
            parts: [Default::default(); 3],
        };
        status.parts[lemnos_device::PART_ACCEL].confidence = 800;
        status.parts[lemnos_device::PART_ACCEL].active = true;
        let out = calibration_output(&status);
        assert_eq!(out["running"], TypedConfigValue::String("accel-six".into()));
        assert_eq!(out["progress"], TypedConfigValue::F64(0.25));
        assert_eq!(out["accel.confidence"], TypedConfigValue::F64(0.8));
        assert_eq!(out["accel.active"], TypedConfigValue::Bool(true));
        assert_eq!(out["candidate"], TypedConfigValue::Bool(true));
    }
}
