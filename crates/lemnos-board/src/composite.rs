//! The fusion device's settings in `DriverRegistry`: its keys and choices, and
//! the checks the registry can make without the board. Lemnosd builds the
//! device (`lemnosd/src/fusion.rs`).

use crate::schema::{ConfigValue, DeviceSpec};
use crate::{BoardError, Buses};
use lemnos_device::BoxedDevice;
use lemnos_hal::ErrorKind;

/// The `config` keys of the fusion device (see `docs/imu-calibration-fusion.md`).
pub(crate) const FUSION_KEYS: &[&str] = &[
    "imu",
    "mag",
    "mode",
    "algorithm",
    "kp",
    "ki",
    "beta",
    "mount_roll_deg",
    "mount_pitch_deg",
    "mount_yaw_deg",
    "declination_deg",
    "dip_deg",
    "always",
];

pub(crate) const FUSION_CHOICES: &[(&str, &[&str])] = &[
    ("mode", &["6axis", "9axis"]),
    ("algorithm", &["mahony", "madgwick"]),
];

/// Numeric settings of the fusion device.
const FUSION_NUMBERS: &[&str] = &[
    "kp",
    "ki",
    "beta",
    "mount_roll_deg",
    "mount_pitch_deg",
    "mount_yaw_deg",
    "declination_deg",
    "dip_deg",
];

/// Checks a composite device's settings the registry can judge without the
/// board: the fusion's required references and value types. Whether `imu`
/// and `mag` name real devices is checked by lemnosd when it builds the
/// fusion device.
pub(crate) fn composite_problems(driver: &str, spec: &DeviceSpec) -> Vec<String> {
    let mut problems = Vec::new();
    if driver != "fusion" {
        return problems;
    }
    match spec.config.get("imu") {
        None => problems.push("fusion needs `imu` (the IMU's device id)".into()),
        Some(value) if value.as_str().is_none() => {
            problems.push("config \"imu\" must be a device id".into());
        }
        Some(_) => {}
    }
    let nine_axis = spec.config.get("mode").and_then(ConfigValue::as_str) == Some("9axis");
    if nine_axis && !spec.config.contains_key("mag") {
        problems.push("fusion mode \"9axis\" needs `mag` (the magnetometer's device id)".into());
    }
    if spec.config.get("mag").is_some_and(|v| v.as_str().is_none()) {
        problems.push("config \"mag\" must be a device id".into());
    }
    for key in FUSION_NUMBERS {
        if let Some(value) = spec.config.get(*key)
            && value.as_f64().is_none()
        {
            problems.push(format!("config {key:?} must be a number"));
        }
    }
    if let Some(value) = spec.config.get("always")
        && value.as_bool().is_none()
    {
        problems.push("config \"always\" must be true or false".into());
    }
    problems
}

/// Lemnosd hosts the fusion device (it needs the board's other devices).
pub(crate) fn build_fusion(
    spec: &DeviceSpec,
    _buses: &mut dyn Buses,
) -> Result<BoxedDevice, BoardError> {
    Err(BoardError::device(
        &spec.id,
        ErrorKind::Unsupported,
        "fusion is hosted by lemnosd",
    ))
}
