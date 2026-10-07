//! Handing fans back to the kernel: the plans `lemnosd` records when it binds
//! a fan, the state file a stop helper reads after the process is gone, and
//! the helper itself (`lemnos-ctl fan restore`).
//!
//! See [`lemnos_drivers_linux::FanRestore`] for the two kinds of fan
//! (`pwm-fan` and other cooling-device fans, and chips with an automatic
//! mode).

use lemnos_board::{BoardDefinition, ConfigValue, DeviceSpec};
use lemnos_drivers_linux::{FanRestore, HwmonFan, MODE_AUTOMATIC, MODE_MANUAL, SysRoot, sysfs};
use std::path::{Path, PathBuf};

/// The board driver name of a hwmon fan.
pub const FAN_DRIVER: &str = "hwmon-fan";

/// The state file's name, next to the socket (in `lemnosd`'s runtime
/// directory, `/run/lemnos`).
pub const FAN_STATE_FILE: &str = "fan-restore";

/// Where the fan state file lives for a socket.
pub fn fan_state_path(socket: &Path) -> PathBuf {
    socket
        .parent()
        .unwrap_or_else(|| Path::new("/run/lemnos"))
        .join(FAN_STATE_FILE)
}

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

/// Writes the plans, one line each, replacing the file atomically.
pub fn write_fan_state(path: &Path, plans: &[FanRestore]) -> std::io::Result<()> {
    let mut text = String::new();
    for plan in plans {
        text.push_str(&plan.to_line());
        text.push('\n');
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Reads the plans `lemnosd` recorded (none if the file is missing).
pub fn read_fan_state(path: &Path) -> Vec<FanRestore> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().filter_map(FanRestore::from_line).collect())
        .unwrap_or_default()
}

/// Applies each plan, ignoring errors (for a panic hook).
pub fn restore_fans(plans: &[FanRestore]) {
    for plan in plans {
        let _ = plan.apply();
    }
}

/// What [`restore_after_stop`] did for one fan.
#[derive(Debug)]
pub struct Restored {
    pub plan: FanRestore,
    pub result: Result<(), lemnos_drivers_linux::SysfsError>,
}

/// The stop helper: hands back, in order, the fans `lemnosd` recorded in
/// `state` (with the `pwm1_enable` read when it bound them), then the
/// board's other fans, then, with `all`, every other hwmon fan. Fans the
/// service did not record get `pwm1_enable = 1` (`pwm-fan`'s boot default)
/// on the cooling-device path.
pub fn restore_after_stop(
    board: Option<&BoardDefinition>,
    state: Option<&Path>,
    sys: &SysRoot,
    all: bool,
) -> Vec<Restored> {
    let thermal = sys.thermal();
    let mut plans: Vec<FanRestore> = state.map(read_fan_state).unwrap_or_default();
    let add = |plans: &mut Vec<FanRestore>, fan: &HwmonFan, mode: i32| {
        let root = canonical(fan.root());
        if plans.iter().any(|p| canonical(&p.fan) == root) {
            return;
        }
        if let Ok(plan) = fan.restore_plan_with(&thermal, mode, MODE_MANUAL) {
            plans.push(plan);
        }
    };
    for spec in board.map_or(&[][..], |b| &b.devices[..]) {
        if let Some(fan) = find_fan(spec, sys) {
            add(&mut plans, &fan, automatic_mode(spec));
        }
    }
    if all {
        for entry in sysfs::entries(&sys.hwmon()).unwrap_or_default() {
            if entry.join("pwm1_enable").exists() {
                add(&mut plans, &HwmonFan::new(entry), MODE_AUTOMATIC);
            }
        }
    }
    plans
        .into_iter()
        .map(|plan| {
            let result = plan.apply().map(|_| ());
            Restored { plan, result }
        })
        .collect()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
