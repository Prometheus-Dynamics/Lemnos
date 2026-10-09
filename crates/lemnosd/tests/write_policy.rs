//! The write policy's client names, end to end over a fake sysfs tree: a
//! device whose `writers` ends in `*` admits every client with that prefix
//! (here the Orion bridge's `orion:<requested_by>` connections), and an
//! exact entry admits only that name.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::SysRoot;
use lemnos_ipc::{ClientError, ClientOptions, Refusal};
use lemnosd::{Service, ServiceConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct SysBuses(PathBuf);

impl Buses for SysBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        Err(BoardError::Device {
            device: format!("i2c-{bus}"),
            kind: lemnos_hal::ErrorKind::NotFound,
            reason: "no I2C in this test".into(),
        })
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.0)
    }
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn link(root: &Path, link: &str, target: &str) {
    let link = root.join(link);
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.join(target), link).unwrap();
}

/// Two chip fans, each with its own `pwm1`.
fn tree(root: &Path) {
    let _ = fs::remove_dir_all(root);
    for (hwmon, name) in [("hwmon2", "pwmfan"), ("hwmon5", "nct6775")] {
        let dir = format!("devices/platform/{name}");
        write(root, &format!("{dir}/hwmon/{hwmon}/name"), name);
        write(root, &format!("{dir}/hwmon/{hwmon}/pwm1"), "77");
        write(root, &format!("{dir}/hwmon/{hwmon}/pwm1_enable"), "1");
        link(root, &format!("{dir}/hwmon/{hwmon}/device"), &dir);
        link(
            root,
            &format!("class/hwmon/{hwmon}"),
            &format!("{dir}/hwmon/{hwmon}"),
        );
    }
}

fn board() -> BoardDefinition {
    BoardDefinition::from_toml_str(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = { name = "pwmfan" }
writers = ["orion:*"]

[[devices]]
id = "case-fan"
driver = "hwmon-fan"
match = { name = "nct6775" }
writers = ["helios"]
"#,
    )
    .unwrap()
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap().trim().into()
}

#[test]
fn a_prefix_wildcard_admits_orion_callers_and_nothing_else() {
    let root = std::env::temp_dir().join(format!("lemnosd-writers-{}", std::process::id()));
    tree(&root);
    let socket = root.join("run/lemnosd.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (thread_stop, thread_root, thread_socket) =
        (Arc::clone(&stop), root.clone(), socket.clone());
    let handle = std::thread::spawn(move || {
        let config = ServiceConfig::new(board(), thread_socket);
        let mut service = Service::new(config, Box::new(SysBuses(thread_root))).unwrap();
        ready_tx.send(()).unwrap();
        service.run(&thread_stop).unwrap();
        service.shutdown(false);
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();

    // An Orion caller's connection is named `orion:<requested_by>`.
    let mut orion = ClientOptions::new(&socket, "orion:operator:x")
        .devices()
        .unwrap();
    assert!(
        orion.set("fan", "duty", 0.9).is_ok(),
        "wildcard should admit"
    );
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1"), "230");
    // Not in the list of a device with an exact entry, even though the
    // prefix matches the wildcard elsewhere.
    assert!(matches!(
        orion.set("case-fan", "duty", 0.5),
        Err(ClientError::Refused(Refusal::NotAllowed))
    ));
    assert_eq!(read(&root, "class/hwmon/hwmon5/pwm1"), "77");

    // A name outside the prefix is refused on the wildcard device.
    let mut other = ClientOptions::new(&socket, "photonvision")
        .devices()
        .unwrap();
    assert!(matches!(
        other.set("fan", "duty", 0.5),
        Err(ClientError::Refused(Refusal::NotAllowed))
    ));

    drop(orion);
    drop(other);
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    let _ = fs::remove_dir_all(&root);
}
