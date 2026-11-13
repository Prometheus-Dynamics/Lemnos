use super::*;
use lemnos_core::{ConfigValue, Reply};
use std::fs;

/// A fake zone directory under the system temporary directory (`TMPDIR`),
/// removed when the test ends.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("lemnos-thermal-zone-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn set_temp(&self, text: &str) {
        fs::write(self.0.join("temp"), text).unwrap();
    }

    fn path(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts a zone whose `path` is `dir`.
fn zone(dir: &str) -> ThermalZone {
    let entries = [("path", ConfigValue::Str(dir))];
    ThermalZone::start(ThermalZoneNeeds {}, &Config::new(&entries)).unwrap()
}

#[test]
fn reads_millidegrees_as_celsius() {
    // The values the frozen branch's board tests read from a fake zone: 51000 and 67750 m°C.
    let tree = Tree::new("celsius");
    let mut dev = zone(&tree.path());
    tree.set_temp("51000\n");
    assert_eq!(dev.read().unwrap(), 51.0);
    tree.set_temp("67750");
    assert_eq!(dev.read().unwrap(), 67.75);
    tree.set_temp("-5000\n");
    assert_eq!(dev.read().unwrap(), -5.0);
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn reads_through_the_contract_wire_path() {
    let tree = Tree::new("wire");
    tree.set_temp("31000");
    let mut dev = zone(&tree.path());
    let mut buf = [0u8; 16];
    let mut reply = Reply::new(&mut buf);
    let status = lemnos_core::Driver::dispatch(&mut dev, READ, &[], &mut reply);
    assert!(status.is_ok());
    assert_eq!(reply.len(), 4);
    assert_eq!(f32::from_le_bytes(buf[..4].try_into().unwrap()), 31.0);
}

#[test]
fn missing_path_is_invalid_argument() {
    let result = ThermalZone::start(ThermalZoneNeeds {}, &Config::default());
    assert_eq!(result.err(), Some(Status::INVALID_ARGUMENT));
}

#[test]
fn bind_does_not_touch_sysfs() {
    // A zone that does not exist yet still starts; the read reports it.
    let tree = Tree::new("absent-at-start");
    let dir = tree.0.join("thermal_zone9").display().to_string();
    let mut dev = zone(&dir);
    assert_eq!(dev.health(), Health::Ok);
    assert_eq!(dev.read(), Err(Status::NOT_FOUND));
}

#[test]
fn missing_zone_fails_each_read_and_recovers() {
    let tree = Tree::new("recover");
    let mut dev = zone(&tree.path());
    // The zone directory has no `temp` yet, as when a zone is removed.
    fs::remove_file(tree.0.join("temp")).ok();
    for _ in 0..3 {
        assert_eq!(dev.read(), Err(Status::NOT_FOUND));
        assert!(matches!(dev.health(), Health::Degraded(_)));
    }
    tree.set_temp("42000");
    assert_eq!(dev.read().unwrap(), 42.0);
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn zone_directory_removed_and_returned() {
    let tree = Tree::new("removed");
    tree.set_temp("40000");
    let mut dev = zone(&tree.path());
    assert_eq!(dev.read().unwrap(), 40.0);
    fs::remove_dir_all(&tree.0).unwrap();
    assert_eq!(dev.read(), Err(Status::NOT_FOUND));
    assert!(matches!(dev.health(), Health::Degraded(_)));
    fs::create_dir_all(&tree.0).unwrap();
    tree.set_temp("41000");
    assert_eq!(dev.read().unwrap(), 41.0);
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn garbage_is_an_io_error_and_recovers() {
    let tree = Tree::new("garbage");
    let mut dev = zone(&tree.path());
    tree.set_temp("not a number");
    assert_eq!(dev.read(), Err(Status::IO));
    assert!(matches!(dev.health(), Health::Degraded(_)));
    tree.set_temp("35000");
    assert_eq!(dev.read().unwrap(), 35.0);
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn oversized_or_non_utf8_contents_are_refused() {
    let tree = Tree::new("oversized");
    let mut dev = zone(&tree.path());
    tree.set_temp(&"1".repeat(SMALL));
    assert_eq!(dev.read(), Err(Status::IO));
    fs::write(tree.0.join("temp"), [0xff, 0xfe, b'1']).unwrap();
    assert_eq!(dev.read(), Err(Status::IO));
}

#[test]
fn describes_one_temperature_value() {
    let tree = Tree::new("describe");
    let dev = zone(&tree.path());
    let mut out = Description::default();
    dev.describe(&mut out);
    assert_eq!(out.items().count(), 1);
    assert_eq!(out.find("temperature").unwrap().unit, Unit::DegreeCelsius);
}

#[test]
fn the_board_gives_the_zones_range_and_warning_limits() {
    let tree = Tree::new("limits");
    let path = tree.path();
    let entries = [
        ("path", ConfigValue::Str(&path)),
        ("min_c", ConfigValue::Int(-20)),
        ("max_c", ConfigValue::Int(100)),
        ("warn_below_c", ConfigValue::Int(-10)),
        ("warn_above_c", ConfigValue::Int(85)),
    ];
    let dev = ThermalZone::start(ThermalZoneNeeds {}, &Config::new(&entries)).unwrap();
    let mut out = Description::default();
    dev.describe(&mut out);
    let item = out.find("temperature").unwrap();
    assert_eq!(
        (item.min, item.max, item.warn_low, item.warn_high),
        (Some(-20), Some(100), Some(-10), Some(85))
    );
    // A range the other way round is refused at bind.
    let backwards = [
        ("path", ConfigValue::Str(&path)),
        ("min_c", ConfigValue::Int(50)),
        ("max_c", ConfigValue::Int(10)),
    ];
    assert!(ThermalZone::start(ThermalZoneNeeds {}, &Config::new(&backwards)).is_err());
}

#[test]
fn the_zone_names_its_directory_as_its_location() {
    let tree = Tree::new("location");
    let dev = zone(&tree.path());
    let mut out = Description::default();
    dev.describe(&mut out);
    assert_eq!(out.location(), tree.path());
}
