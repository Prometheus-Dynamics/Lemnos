//! A host-owned device, end to end over a fake sysfs tree: its host's
//! services drive it, so no client may write it (not even a client the
//! board's `writers` would otherwise admit), and shutting lemnosd down
//! leaves its files as the host set them (no fan hand-back is recorded).

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

/// One fan the host drives (`pwmfan`, at duty 77, automatic mode 1).
fn tree(root: &Path) {
    let _ = fs::remove_dir_all(root);
    let dir = "devices/platform/pwmfan";
    write(root, &format!("{dir}/hwmon/hwmon2/name"), "pwmfan");
    write(root, &format!("{dir}/hwmon/hwmon2/pwm1"), "77");
    write(root, &format!("{dir}/hwmon/hwmon2/pwm1_enable"), "1");
    link(root, &format!("{dir}/hwmon/hwmon2/device"), dir);
    link(root, "class/hwmon/hwmon2", &format!("{dir}/hwmon/hwmon2"));
}

fn board() -> BoardDefinition {
    BoardDefinition::from_toml_str(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"

[[devices]]
id = "fan"
driver = "hwmon-fan"
owner = "host"
match = { name = "pwmfan" }
"#,
    )
    .unwrap()
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap().trim().into()
}

#[test]
fn a_host_owned_device_refuses_every_write_and_is_left_as_the_host_set_it() {
    let root = std::env::temp_dir().join(format!("lemnosd-host-owned-{}", std::process::id()));
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

    // Any client, including one a `writers` list would name: refused.
    let mut app = ClientOptions::new(&socket, "app").devices().unwrap();
    assert!(matches!(
        app.set("fan", "duty", 0.5),
        Err(ClientError::Refused(Refusal::NotAllowed))
    ));
    let mut orion = ClientOptions::new(&socket, "orion:operator:x")
        .devices()
        .unwrap();
    assert!(matches!(
        orion.set("fan", "duty", 0.9),
        Err(ClientError::Refused(Refusal::NotAllowed))
    ));
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1"), "77");
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");

    drop(app);
    drop(orion);
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    // Shutdown hands back no fan: the host's own mode is still there.
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");
    let _ = fs::remove_dir_all(&root);
}
