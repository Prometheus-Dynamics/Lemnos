//! Hands a platform device to a kernel module's driver, as root, the way the kernel's driver core
//! takes it: `driver_override` names the driver, the stock driver (when one holds the device) lets
//! it go through its `unbind`, and the module's driver takes it through its `bind`. A driver with no
//! match table binds nothing else, so this is the only way the module gets a node; a stock driver
//! built into the kernel binds its node at boot and is not stopped by a blacklist.
//!
//! Only the stock driver the caller names is unbound. A device held by any other driver is left as
//! it is, and the reason says so. Each write is one `write` to an existing file: nothing is
//! created, so a file the kernel does not have fails here as it would on a board.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// What a claim did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claimed {
    /// The module's driver already holds the device. Nothing was written.
    Already,
    /// The override is set and the module's driver took the device; `unbound` is the stock driver
    /// that let it go, when one held it.
    Bound { unbound: Option<String> },
    /// The override is set (and the stock driver let it go, when one held it), but the module's
    /// driver is not registered yet: the kernel binds the device to it when the module loads.
    Waiting { unbound: Option<String> },
}

/// Claims `device` (its name under `platform/devices`) for `driver`, letting `stock` go first when it
/// holds the device. `platform` is the bus directory, `/sys/bus/platform` on a board.
///
/// # Errors
///
/// No such device; the device held by a driver that is neither `driver` nor `stock`; or a write the
/// kernel refused, each named with the file and the error. A refused bind leaves the override set,
/// so the device binds when the module's driver can take it.
pub fn claim(
    platform: &Path,
    device: &str,
    driver: &str,
    stock: Option<&str>,
) -> Result<Claimed, String> {
    let node = platform.join("devices").join(device);
    if !node.is_dir() {
        return Err(format!(
            "{device}: no such platform device ({})",
            node.display()
        ));
    }
    let current = std::fs::read_link(node.join("driver"))
        .ok()
        .and_then(|target| target.file_name().map(|n| n.to_string_lossy().into_owned()));
    if current.as_deref() == Some(driver) {
        return Ok(Claimed::Already);
    }
    if let Some(other) = &current
        && Some(other.as_str()) != stock
    {
        return Err(format!(
            "{device} is bound to {other}, which is not the stock driver the SoC map names ({}): left as it is",
            stock.unwrap_or("none")
        ));
    }
    if let Some(other) = &current {
        // A driver built with suppress_bind_attrs has no unbind file: the kernel never lets it go
        // at run time, so the node can be the module's only with that driver a module or off.
        // Said before the override is written, so nothing is left half done.
        let unbind = platform.join("drivers").join(other).join("unbind");
        if !unbind.exists() {
            return Err(format!(
                "{device} is bound to {other}, which the kernel does not let go at run time (no {}: \
                 the driver is built with suppress_bind_attrs); build {other} as a module or leave it out",
                unbind.display()
            ));
        }
    }
    put(&node.join("driver_override"), driver)?;
    if let Some(other) = &current {
        put(&platform.join("drivers").join(other).join("unbind"), device)?;
    }
    let bind = platform.join("drivers").join(driver).join("bind");
    if !bind.exists() {
        return Ok(Claimed::Waiting { unbound: current });
    }
    put(&bind, device)?;
    Ok(Claimed::Bound { unbound: current })
}

/// One write of `text` to the existing file `path`. `O_TRUNC` is a no-op on a sysfs attribute (each
/// write replaces the value); it makes a plain file hold exactly what was written, as sysfs does.
fn put(path: &Path, text: &str) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// The board's platform bus as its listing shows it (a bcm2712 board on the image under test):
    /// the PiSP back end held by the built-in `pispbe`, the camera receiver with no driver, the
    /// module's two drivers registered with nothing bound, each override unset (`(null)`).
    struct Bus(PathBuf);

    impl Bus {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("lemnos-claim-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            for driver in ["pispbe", "lemnos", "lemnos-rp1-csi2", "other"] {
                let dir = root.join("drivers").join(driver);
                fs::create_dir_all(&dir).unwrap();
                for file in ["bind", "unbind", "uevent"] {
                    fs::write(dir.join(file), "").unwrap();
                }
            }
            for device in ["1000880000.pisp_be", "1f00110000.csi"] {
                let dir = root.join("devices").join(device);
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("driver_override"), "(null)\n").unwrap();
            }
            std::os::unix::fs::symlink(
                root.join("drivers/pispbe"),
                root.join("devices/1000880000.pisp_be/driver"),
            )
            .unwrap();
            Self(root)
        }

        fn read(&self, path: &str) -> String {
            fs::read_to_string(self.0.join(path)).unwrap()
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_node_the_stock_driver_holds_is_let_go_and_bound_to_the_module() {
        let bus = Bus::new("stock");
        let claimed = claim(&bus.0, "1000880000.pisp_be", "lemnos", Some("pispbe")).unwrap();
        assert_eq!(
            claimed,
            Claimed::Bound {
                unbound: Some("pispbe".into())
            }
        );
        assert_eq!(
            bus.read("devices/1000880000.pisp_be/driver_override"),
            "lemnos"
        );
        assert_eq!(bus.read("drivers/pispbe/unbind"), "1000880000.pisp_be");
        assert_eq!(bus.read("drivers/lemnos/bind"), "1000880000.pisp_be");
    }

    #[test]
    fn a_node_with_no_driver_is_bound_to_the_driver_named_for_it() {
        let bus = Bus::new("free");
        let claimed = claim(&bus.0, "1f00110000.csi", "lemnos-rp1-csi2", Some("rp1-cfe")).unwrap();
        assert_eq!(claimed, Claimed::Bound { unbound: None });
        assert_eq!(
            bus.read("devices/1f00110000.csi/driver_override"),
            "lemnos-rp1-csi2"
        );
        assert_eq!(bus.read("drivers/lemnos-rp1-csi2/bind"), "1f00110000.csi");
        assert_eq!(bus.read("drivers/lemnos/bind"), "");
    }

    #[test]
    fn a_node_the_module_holds_is_not_touched() {
        let bus = Bus::new("held");
        fs::remove_file(bus.0.join("devices/1000880000.pisp_be/driver")).unwrap();
        std::os::unix::fs::symlink(
            bus.0.join("drivers/lemnos"),
            bus.0.join("devices/1000880000.pisp_be/driver"),
        )
        .unwrap();
        assert_eq!(
            claim(&bus.0, "1000880000.pisp_be", "lemnos", Some("pispbe")).unwrap(),
            Claimed::Already
        );
        assert_eq!(
            bus.read("devices/1000880000.pisp_be/driver_override"),
            "(null)\n"
        );
        assert_eq!(bus.read("drivers/lemnos/bind"), "");
    }

    #[test]
    fn a_driver_the_map_does_not_name_is_left_holding_the_node() {
        let bus = Bus::new("other");
        let why = claim(&bus.0, "1000880000.pisp_be", "lemnos", Some("someone-else")).unwrap_err();
        assert!(
            why.contains("bound to pispbe, which is not the stock driver"),
            "{why}"
        );
        assert_eq!(
            bus.read("devices/1000880000.pisp_be/driver_override"),
            "(null)\n"
        );
        assert_eq!(bus.read("drivers/pispbe/unbind"), "");
    }

    #[test]
    fn a_module_not_loaded_yet_leaves_the_override_for_its_load() {
        let bus = Bus::new("late");
        fs::remove_dir_all(bus.0.join("drivers/lemnos")).unwrap();
        let claimed = claim(&bus.0, "1000880000.pisp_be", "lemnos", Some("pispbe")).unwrap();
        assert_eq!(
            claimed,
            Claimed::Waiting {
                unbound: Some("pispbe".into())
            }
        );
        assert_eq!(
            bus.read("devices/1000880000.pisp_be/driver_override"),
            "lemnos"
        );
    }

    #[test]
    fn a_stock_driver_the_kernel_never_lets_go_is_said_and_nothing_is_written() {
        // A driver built with suppress_bind_attrs: its directory has no bind or unbind file.
        let bus = Bus::new("suppressed");
        fs::remove_file(bus.0.join("drivers/pispbe/unbind")).unwrap();
        fs::remove_file(bus.0.join("drivers/pispbe/bind")).unwrap();
        let why = claim(&bus.0, "1000880000.pisp_be", "lemnos", Some("pispbe")).unwrap_err();
        assert!(
            why.contains("suppress_bind_attrs") && why.contains("build pispbe as a module"),
            "{why}"
        );
        assert_eq!(
            bus.read("devices/1000880000.pisp_be/driver_override"),
            "(null)\n",
            "no override is left behind"
        );
    }

    #[test]
    fn a_missing_device_or_a_file_the_kernel_lacks_fails_by_name() {
        let bus = Bus::new("missing");
        let why = claim(&bus.0, "nope.block", "lemnos", None).unwrap_err();
        assert!(why.contains("nope.block: no such platform device"), "{why}");
        // No override file: the write fails as it would on a kernel without one; nothing is made.
        fs::remove_file(bus.0.join("devices/1f00110000.csi/driver_override")).unwrap();
        let why = claim(&bus.0, "1f00110000.csi", "lemnos-rp1-csi2", None).unwrap_err();
        assert!(why.contains("driver_override"), "{why}");
        assert!(
            !bus.0
                .join("devices/1f00110000.csi/driver_override")
                .exists()
        );
    }
}
