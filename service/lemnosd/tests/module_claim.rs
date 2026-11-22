//! The root phase's claim of the module's nodes, from the reference board file and its SoC map, on a
//! platform bus laid out as the board's listing shows it: the PiSP back end held by the built-in
//! stock driver, the camera receiver with no driver, the module's two drivers registered with
//! nothing bound, every override unset.

use std::fs;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn board() -> lemnos_host::board::BoardFile {
    let text = fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    lemnos_host::board::parse(&text).unwrap()
}

fn map() -> lemnos_devtree::SocMap {
    let text = fs::read_to_string(repo().join("socs/bcm2712-rp1.toml")).unwrap();
    lemnos_devtree::SocMap::parse(&text).unwrap()
}

struct Bus(PathBuf);

impl Bus {
    fn new(tag: &str, module_loaded: bool) -> Self {
        let root = std::env::temp_dir().join(format!("lemnosd-claim-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut drivers = vec!["pispbe"];
        if module_loaded {
            drivers.extend(["lemnos", "lemnos-rp1-csi2"]);
        }
        for driver in drivers {
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
        fs::read_to_string(self.0.join(path)).unwrap_or_else(|e| format!("({e})"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn each_node_the_board_gives_the_module_goes_to_the_driver_its_map_names() {
    let bus = Bus::new("both", true);
    let map = map();
    lemnosd::privilege::claim_module_nodes(&board(), Some(&map), bus.path());
    // The back end: the built-in stock driver let it go, and the block driver took it.
    assert_eq!(
        bus.read("devices/1000880000.pisp_be/driver_override"),
        "lemnos"
    );
    assert_eq!(bus.read("drivers/pispbe/unbind"), "1000880000.pisp_be");
    assert_eq!(bus.read("drivers/lemnos/bind"), "1000880000.pisp_be");
    // The receiver: no driver held it; the module's receiver driver took it.
    assert_eq!(
        bus.read("devices/1f00110000.csi/driver_override"),
        "lemnos-rp1-csi2"
    );
    assert_eq!(bus.read("drivers/lemnos-rp1-csi2/bind"), "1f00110000.csi");
}

#[test]
fn before_the_module_loads_the_overrides_are_set_for_its_load() {
    let bus = Bus::new("late", false);
    let map = map();
    lemnosd::privilege::claim_module_nodes(&board(), Some(&map), bus.path());
    assert_eq!(
        bus.read("devices/1000880000.pisp_be/driver_override"),
        "lemnos"
    );
    assert_eq!(bus.read("drivers/pispbe/unbind"), "1000880000.pisp_be");
    assert_eq!(
        bus.read("devices/1f00110000.csi/driver_override"),
        "lemnos-rp1-csi2"
    );
}

#[test]
fn without_a_map_nothing_is_written() {
    let bus = Bus::new("nomap", true);
    lemnosd::privilege::claim_module_nodes(&board(), None, bus.path());
    assert_eq!(
        bus.read("devices/1000880000.pisp_be/driver_override"),
        "(null)\n"
    );
    assert_eq!(bus.read("drivers/pispbe/unbind"), "");
}
