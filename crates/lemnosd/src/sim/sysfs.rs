//! The simulated board's sysfs tree, laid out as the kernel lays out the
//! attributes the devices read: I2C adapters with their device-tree nodes
//! (what an `i2c:compatible=` selector matches), the hwmon fan (`pwm1`,
//! `pwm1_enable`, `fan1_input`) and the thermal zone (`type`, `temp`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One I2C adapter the board's selectors name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub bus: u32,
    /// The adapter's `name` attribute.
    pub name: Option<String>,
    /// The `compatible` of its device-tree node.
    pub compatible: Option<String>,
}

/// Writes the adapter's directory under `sys/bus/i2c/devices`.
pub fn write_adapter(sys: &Path, adapter: &Adapter) -> io::Result<()> {
    let dir = sys
        .join("bus/i2c/devices")
        .join(format!("i2c-{}", adapter.bus));
    fs::create_dir_all(&dir)?;
    if let Some(name) = &adapter.name {
        fs::write(dir.join("name"), format!("{name}\n"))?;
    }
    if let Some(compatible) = &adapter.compatible {
        // A device-tree `compatible` is NUL-separated strings.
        let node = dir.join("of_node");
        fs::create_dir_all(&node)?;
        fs::write(node.join("compatible"), format!("{compatible}\0"))?;
    }
    Ok(())
}

/// The hwmon fan's directory (`class/hwmon/hwmon0`), in manual mode at 50 %
/// with the tachometer at rest.
pub fn write_fan(sys: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = sys.join("class/hwmon/hwmon0");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("name"), format!("{name}\n"))?;
    fs::write(dir.join("pwm1"), "128\n")?;
    fs::write(dir.join("pwm1_enable"), "1\n")?;
    fs::write(dir.join("fan1_input"), "0\n")?;
    Ok(dir)
}

/// The thermal zone's directory (`class/thermal/thermal_zone0`) and its
/// `temp` file, at `temp_mc` millidegrees.
pub fn write_thermal_zone(sys: &Path, zone_type: &str, temp_mc: i64) -> io::Result<PathBuf> {
    let dir = sys.join("class/thermal/thermal_zone0");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("type"), format!("{zone_type}\n"))?;
    fs::write(dir.join("temp"), format!("{temp_mc}\n"))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapters_are_found_by_the_selector_resolution() {
        let root = std::env::temp_dir().join(format!("lemnosd-sysfs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sys = root.join("sys");
        write_adapter(
            &sys,
            &Adapter {
                bus: 17,
                name: None,
                compatible: Some("i2c-gpio".into()),
            },
        )
        .unwrap();
        let selector = lemnos_board::I2cSelector::parse("compatible=i2c-gpio").unwrap();
        let sys_root = lemnos_drivers_linux::SysRoot::new(&sys);
        assert_eq!(selector.resolve(&sys_root), Ok(17));
        let _ = fs::remove_dir_all(&root);
    }
}
