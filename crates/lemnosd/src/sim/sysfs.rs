//! The simulated board's sysfs tree, laid out as the kernel lays out the
//! attributes the devices read: I2C adapters with their device-tree nodes
//! (what an `i2c:compatible=` selector matches), the hwmon fan (`pwm1`,
//! `pwm1_enable`, `fan1_input`) and the thermal zone (`type`, `temp`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One I2C adapter the board's selectors name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Adapter {
    pub bus: u32,
    /// The adapter's `name` attribute.
    pub name: Option<String>,
    /// The `compatible` of its device-tree node.
    pub compatible: Option<String>,
    /// The device-tree path of its node (`of=`), as the selector spells it.
    pub of: Option<String>,
    /// The last component of its device-tree path (`node=`).
    pub node: Option<String>,
}

/// The fake device tree: a node's path under it is what `of_node` resolves to,
/// as the kernel's device-tree links do on a board (`firmware/devicetree/base`).
const DEVICETREE: &str = "firmware/devicetree/base";

/// Writes the adapter's directory under `sys/bus/i2c/devices`: its `name`, and
/// its `of_node` link to a node of the fake device tree carrying the
/// `compatible`, at the path the selector names (`of=`), or under `sim/` named
/// for the selector's `node=`.
pub(crate) fn write_adapter(sys: &Path, adapter: &Adapter) -> io::Result<()> {
    let dir = sys
        .join("bus/i2c/devices")
        .join(format!("i2c-{}", adapter.bus));
    fs::create_dir_all(&dir)?;
    if let Some(name) = &adapter.name {
        fs::write(dir.join("name"), format!("{name}\n"))?;
    }
    if adapter.compatible.is_none() && adapter.of.is_none() && adapter.node.is_none() {
        return Ok(());
    }
    let base = sys.join(DEVICETREE);
    let node = match (&adapter.of, &adapter.node) {
        (Some(of), _) => base.join(of.trim_start_matches('/')),
        (None, Some(node)) => base.join("sim").join(node),
        (None, None) => base.join("sim").join(format!("i2c-{}", adapter.bus)),
    };
    fs::create_dir_all(&node)?;
    if let Some(compatible) = &adapter.compatible {
        // A device-tree `compatible` is NUL-separated strings.
        fs::write(node.join("compatible"), format!("{compatible}\0"))?;
    }
    // Canonicalized by the resolver, so the link must be absolute.
    let node = fs::canonicalize(&node)?;
    let link = dir.join("of_node");
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&node, &link)?;
    Ok(())
}

/// The hwmon fan's directory (`class/hwmon/hwmon0`), in manual mode at 50 %
/// with the tachometer at rest.
pub(crate) fn write_fan(sys: &Path, name: &str) -> io::Result<PathBuf> {
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
pub(crate) fn write_thermal_zone(sys: &Path, zone_type: &str, temp_mc: i64) -> io::Result<PathBuf> {
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
                of: None,
                node: None,
            },
        )
        .unwrap();
        let selector = lemnos_board::I2cSelector::parse("compatible=i2c-gpio").unwrap();
        let sys_root = lemnos_drivers_linux::SysRoot::new(&sys);
        assert_eq!(selector.resolve(&sys_root), Ok(17));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn device_tree_paths_and_nodes_resolve() {
        let root = std::env::temp_dir().join(format!("lemnosd-sysfs-of-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sys = root.join("sys");
        let of = "/axi/pcie@1000120000/rp1/i2c@74000";
        write_adapter(
            &sys,
            &Adapter {
                bus: 18,
                name: None,
                compatible: None,
                of: Some(of.into()),
                node: None,
            },
        )
        .unwrap();
        write_adapter(
            &sys,
            &Adapter {
                bus: 19,
                name: None,
                compatible: Some("snps,designware-i2c".into()),
                of: None,
                node: Some("i2c@70000".into()),
            },
        )
        .unwrap();
        let sys_root = lemnos_drivers_linux::SysRoot::new(&sys);
        let by_of = lemnos_board::I2cSelector::parse(&format!("of={of}")).unwrap();
        assert_eq!(by_of.resolve(&sys_root), Ok(18));
        let by_node = lemnos_board::I2cSelector::parse("node=i2c@70000").unwrap();
        assert_eq!(by_node.resolve(&sys_root), Ok(19));
        let _ = fs::remove_dir_all(&root);
    }
}
