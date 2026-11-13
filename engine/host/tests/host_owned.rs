//! A host-owned device, over a fake sysfs tree: the operating system's own driver owns it, so
//! Lemnos answers its reads and refuses every call that would change it, and leaves its files as
//! the host set them, through shutdown. A Lemnos-owned device on the same board still takes them.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lemnos_core::{Decode, Encode, MethodId, Reader, Status, Writer};
use lemnos_driver_host_clock::HostClock as ClockFacts;
use lemnos_driver_hwmon_fan::HwmonFan;
use lemnos_driver_thermal_zone::ThermalZone;
use lemnos_host::board::{Owner, parse};
use lemnos_host::{DeviceState, DriverEntry, Host, Options, entry};
use lemnos_sim::SimBoard;

const SET_DUTY: u16 = 1;
const DUTY: u16 = 2;
const RPM: u16 = 3;
const RELEASE: u16 = 4;
const READ_TEMPERATURE: u16 = 1;

fn drivers() -> Vec<DriverEntry> {
    vec![
        entry::<HwmonFan>(),
        entry::<ThermalZone>(),
        entry::<ClockFacts>(),
    ]
}

fn method(method: u16) -> MethodId {
    MethodId {
        contract: 0,
        method,
    }
}

/// A fake sysfs tree in its own directory, removed on drop.
struct Tree(PathBuf);

impl Tree {
    /// Two hwmon devices in automatic mode, `theirs` at `pwm1` 77 and `ours` at 10, and a
    /// thermal zone at 57.85 °C.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("host-owned-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let tree = Self(root);
        for (dir, name, pwm) in [("hwmon2", "theirs", "77"), ("hwmon3", "ours", "10")] {
            tree.write(&format!("class/hwmon/{dir}/name"), name);
            tree.write(&format!("class/hwmon/{dir}/pwm1"), pwm);
            tree.write(&format!("class/hwmon/{dir}/pwm1_enable"), "2");
            tree.write(&format!("class/hwmon/{dir}/fan1_input"), "1500");
        }
        tree.write("class/thermal/thermal_zone0/temp", "57850");
        tree
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path))
            .unwrap()
            .trim()
            .to_string()
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn start(tree: &Tree) -> Host {
    let sys = tree.root().display();
    let text = format!(
        r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "t"

[[devices]]
id = "theirs"
driver = "hwmon-fan"
owner = "host"
config = {{ sys_root = "{sys}", name = "theirs" }}

[[devices]]
id = "zone"
driver = "thermal-zone"
owner = "host"
config = {{ path = "{sys}/class/thermal/thermal_zone0" }}

[[devices]]
id = "ours"
driver = "hwmon-fan"
config = {{ sys_root = "{sys}", name = "ours" }}
"#
    );
    let board = parse(&text).unwrap();
    let sim = SimBoard::new();
    let clock = Arc::new(sim.clock());
    let host = Host::start(&board, &drivers(), Box::new(sim), clock, Options::default()).unwrap();
    for id in ["theirs", "zone", "ours"] {
        assert_eq!(host.device(id).unwrap().state, DeviceState::Running, "{id}");
    }
    host
}

fn index(host: &Host, id: &str) -> u32 {
    host.device(id).unwrap().index
}

fn call(host: &Host, device: u32, id: u16, args: &[u8]) -> Result<Vec<u8>, Status> {
    let mut reply = [0; 64];
    let n = host.call(device, method(id), args, &mut reply)?;
    Ok(reply[..n].to_vec())
}

fn duty_args(per_mille: u16) -> Vec<u8> {
    let mut buf = [0; 8];
    let mut writer = Writer::new(&mut buf);
    per_mille.encode(&mut writer).unwrap();
    let n = writer.len();
    buf[..n].to_vec()
}

fn decode<'a, T: Decode<'a>>(bytes: &'a [u8]) -> T {
    T::decode(&mut Reader::new(bytes)).unwrap()
}

#[test]
fn a_host_owned_device_answers_reads_and_refuses_every_change() {
    let tree = Tree::new("refuses");
    let host = start(&tree);
    let theirs = index(&host, "theirs");

    // Reads reach the driver.
    assert_eq!(decode::<u16>(&call(&host, theirs, DUTY, &[]).unwrap()), 302);
    assert_eq!(
        decode::<Option<u32>>(&call(&host, theirs, RPM, &[]).unwrap()),
        Some(1500)
    );
    let zone = index(&host, "zone");
    assert!(call(&host, zone, READ_TEMPERATURE, &[]).is_ok());

    // A control, an action, and a method the description doesn't name: refused.
    assert_eq!(
        call(&host, theirs, SET_DUTY, &duty_args(500)),
        Err(Status::NOT_PERMITTED)
    );
    assert_eq!(
        call(&host, theirs, RELEASE, &[]),
        Err(Status::NOT_PERMITTED)
    );
    assert_eq!(call(&host, theirs, 9, &[]), Err(Status::NOT_PERMITTED));
    assert_eq!(tree.read("class/hwmon/hwmon2/pwm1"), "77");
    assert_eq!(tree.read("class/hwmon/hwmon2/pwm1_enable"), "2");

    // Refusals are not faults: the device keeps running and keeps answering reads.
    assert_eq!(host.device("theirs").unwrap().state, DeviceState::Running);
    assert_eq!(decode::<u16>(&call(&host, theirs, DUTY, &[]).unwrap()), 302);

    // Shutting down writes nothing, because nothing was taken.
    host.shutdown();
    assert_eq!(tree.read("class/hwmon/hwmon2/pwm1"), "77");
    assert_eq!(tree.read("class/hwmon/hwmon2/pwm1_enable"), "2");
}

#[test]
fn a_lemnos_owned_device_on_the_same_board_still_takes_changes() {
    let tree = Tree::new("ours");
    let host = start(&tree);
    let ours = index(&host, "ours");

    call(&host, ours, SET_DUTY, &duty_args(500)).unwrap();
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1"), "128");
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1_enable"), "1");
    // Release puts the fan at full duty and leaves it in manual mode: nothing goes back to the
    // kernel's governor.
    call(&host, ours, RELEASE, &[]).unwrap();
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1"), "255");
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1_enable"), "1");

    // And the host-owned one beside it was never touched.
    assert_eq!(tree.read("class/hwmon/hwmon2/pwm1"), "77");
    assert_eq!(tree.read("class/hwmon/hwmon2/pwm1_enable"), "2");
}

#[test]
fn the_guest_board_is_host_owned_throughout_and_names_only_known_drivers() {
    let board = parse(include_str!("../../../testing/boards/guest.toml")).unwrap();
    assert!(!board.devices.is_empty());
    let known: Vec<_> = drivers().iter().map(|d| d.manifest().name).collect();
    for device in &board.devices {
        assert_eq!(device.owner, Owner::Host, "{}", device.id);
        assert!(known.contains(&device.driver.as_str()), "{}", device.driver);
    }
}
