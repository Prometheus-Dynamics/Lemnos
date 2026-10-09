//! Orion actions on a resource, as lemnosd operations (`docs/orion.md`):
//! parsing the arguments and turning lemnosd's answers into outcomes. No I/O.

use std::collections::BTreeMap;

use lemnos_ipc::{ClientError, Refusal};
use orion_control_plane::TypedConfigValue;

/// The action names a device resource accepts.
pub const ACTIONS: [&str; 4] = ["set", "restore", "release", "read"];

/// A parsed action.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// `set`: write `value` (in the control's unit) to `control`.
    Set { control: String, value: f64 },
    /// `restore`: undo this caller's writes (to `control`, or all of them).
    Restore { control: Option<String> },
    /// `release`: hand a fan back to the kernel's governor.
    Release,
    /// `read`: the device's latest reading.
    Read,
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
        other => Err(format!(
            "unsupported action `{other}` (one of {})",
            ACTIONS.join(", ")
        )),
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
}
