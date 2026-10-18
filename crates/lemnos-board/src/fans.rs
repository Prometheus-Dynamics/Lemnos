//! The board's fans, for handing them back to the kernel: which hwmon fan a
//! `hwmon-fan` spec names, and the automatic mode it hands back to. The
//! hand-back itself (state file, stop helper) is
//! `lemnos_drivers_linux::fan_restore`, which needs no board.

use crate::schema::{BoardDefinition, ConfigValue, DeviceSpec};
use lemnos_drivers_linux::{HwmonFan, MODE_AUTOMATIC, SysRoot};

/// The board driver name of a hwmon fan.
pub const FAN_DRIVER: &str = "hwmon-fan";

/// A fan spec's automatic `pwm1_enable` mode (`restore_mode`, default 2),
/// used for fans without a cooling device.
pub fn automatic_mode(spec: &DeviceSpec) -> i32 {
    spec.config
        .get("restore_mode")
        .and_then(ConfigValue::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(MODE_AUTOMATIC)
}

/// The hwmon fan a board spec names: its `path`, else the first fan whose
/// `name` matches.
pub fn find_fan(spec: &DeviceSpec, sys: &SysRoot) -> Option<HwmonFan> {
    if spec.driver != FAN_DRIVER {
        return None;
    }
    match &spec.path {
        Some(path) => Some(HwmonFan::new(path)),
        None => HwmonFan::find(&sys.hwmon(), spec.matches.get("name").map(String::as_str))
            .ok()
            .flatten(),
    }
}

/// The board's hwmon fans, each with the automatic mode it hands back to,
/// in the board's device order.
pub fn board_fans(board: Option<&BoardDefinition>, sys: &SysRoot) -> Vec<(HwmonFan, i32)> {
    board
        .map_or(&[][..], |b| &b.devices[..])
        .iter()
        .filter_map(|spec| find_fan(spec, sys).map(|fan| (fan, automatic_mode(spec))))
        .collect()
}
