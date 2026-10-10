//! Settings the user chose that survive a restart (and a reboot): the state
//! directory (`LEMNOSD_STATE_DIR`, default `/var/lib/lemnos`), one small file
//! per setting. Written atomically (a temporary file, then a rename).

use lemnos_board::ConfigValue;
use lemnos_board::DeviceSpec;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The default state directory (`LEMNOSD_STATE_DIR`).
pub const DEFAULT_STATE_DIR: &str = "/var/lib/lemnos";

/// The driver name of a power switch.
pub const POWER_DRIVER: &str = "gpio-power-switch";

/// Whether `spec` is a power switch that keeps its state across restarts
/// (`persist = true`).
pub fn power_persists(spec: &DeviceSpec) -> bool {
    spec.driver == POWER_DRIVER
        && spec
            .config
            .get("persist")
            .and_then(ConfigValue::as_bool)
            .unwrap_or(false)
}

/// What a power switch does when `lemnosd` stops: `keep` (the default, the
/// last commanded state stays), `on` or `off`.
pub fn power_on_exit(spec: &DeviceSpec) -> &str {
    spec.config
        .get("on_exit")
        .and_then(ConfigValue::as_str)
        .unwrap_or("keep")
}

/// A power switch's enable delay in milliseconds (0 when none, and capped at
/// the driver's maximum).
pub fn power_enable_delay_ms(spec: &DeviceSpec) -> u32 {
    spec.config
        .get("enable_delay_ms")
        .and_then(ConfigValue::as_i64)
        .and_then(|ms| u32::try_from(ms).ok())
        .map_or(0, |ms| {
            ms.min(lemnos_device::power_switch::MAX_ENABLE_DELAY_MS)
        })
}

/// A light's saved ring brightness (thousandths), if one was saved:
/// `<dir>/light/<device>.brightness`.
pub fn load_light_brightness(dir: &Path, device: &str) -> Option<u16> {
    let text = fs::read_to_string(dir.join("light").join(format!("{device}.brightness"))).ok()?;
    text.trim().parse::<u16>().ok().filter(|p| *p <= 1_000)
}

/// Saves a light's ring brightness (thousandths).
pub fn save_light_brightness(dir: &Path, device: &str, permille: u16) -> io::Result<()> {
    write_atomic(
        &dir.join("light").join(format!("{device}.brightness")),
        &format!("{permille}\n"),
    )
}

/// A ring brightness in thousandths as the light's `look_brightness` (0 to
/// 255, rounded as the board's own setting is).
pub fn permille_to_scale(permille: u16) -> u8 {
    u8::try_from((u32::from(permille) * 255 + 500) / 1000).unwrap_or(u8::MAX)
}

/// Sets `spec`'s `default_on` from the saved state, for a switch that
/// persists (the saved state is what it starts at).
pub fn apply_saved_power(spec: &mut DeviceSpec, dir: &Path) {
    if power_persists(spec)
        && let Some(on) = load_power(dir, &spec.id)
    {
        spec.config
            .insert("default_on".into(), ConfigValue::Bool(on));
    }
}

/// A power switch's last commanded state: `<dir>/power/<device>.state`.
fn power_path(dir: &Path, device: &str) -> PathBuf {
    dir.join("power").join(format!("{device}.state"))
}

/// The saved state of power switch `device`, if one was saved.
pub fn load_power(dir: &Path, device: &str) -> Option<bool> {
    match fs::read_to_string(power_path(dir, device)).ok()?.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Saves power switch `device`'s state.
pub fn save_power(dir: &Path, device: &str, on: bool) -> io::Result<()> {
    write_atomic(&power_path(dir, device), if on { "on\n" } else { "off\n" })
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lemnosd-state-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_saved_power_state_loads_back() {
        let dir = dir("power");
        assert_eq!(load_power(&dir, "usb-a-power"), None);
        save_power(&dir, "usb-a-power", false).unwrap();
        assert_eq!(load_power(&dir, "usb-a-power"), Some(false));
        save_power(&dir, "usb-a-power", true).unwrap();
        assert_eq!(load_power(&dir, "usb-a-power"), Some(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_garbled_file_is_no_state() {
        let dir = dir("garbled");
        write_atomic(&power_path(&dir, "usb-c-power"), "maybe").unwrap();
        assert_eq!(load_power(&dir, "usb-c-power"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
