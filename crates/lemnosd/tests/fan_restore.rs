//! The fan hand-back, end to end over a fake sysfs tree: a `pwm-fan` fan
//! (driver link, thermal cooling device) and a fan-controller chip with an
//! automatic mode, handed back by the service on stop and by the stop helper
//! (`lemnos-ctl fan restore`) after the process is gone.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::{FanRestore, RestoreKind, SysRoot};
use lemnos_ipc::ClientOptions;
use lemnosd::fans::{fan_state_path, read_fan_state, restore_after_stop};
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

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap().trim().into()
}

/// The Raze's fan as the kernel shows it, plus a chip fan with an automatic
/// mode.
fn tree(root: &Path) {
    let _ = fs::remove_dir_all(root);
    let fan = "devices/platform/cooling_fan";
    write(root, &format!("{fan}/hwmon/hwmon2/name"), "pwmfan");
    write(root, &format!("{fan}/hwmon/hwmon2/pwm1"), "77");
    write(root, &format!("{fan}/hwmon/hwmon2/pwm1_enable"), "1");
    link(root, &format!("{fan}/hwmon/hwmon2/device"), fan);
    write(root, "bus/platform/drivers/pwm-fan/bind", "");
    link(
        root,
        &format!("{fan}/driver"),
        "bus/platform/drivers/pwm-fan",
    );
    link(root, "class/hwmon/hwmon2", &format!("{fan}/hwmon/hwmon2"));
    let cdev = "devices/virtual/thermal/cooling_device0";
    write(root, &format!("{cdev}/type"), "pwm-fan");
    write(root, &format!("{cdev}/cur_state"), "1");
    write(root, &format!("{cdev}/max_state"), "4");
    link(root, "class/thermal/cooling_device0", cdev);
    write(root, "class/thermal/thermal_zone0/type", "cpu-thermal");
    write(root, "class/thermal/thermal_zone0/temp", "57850");

    write(root, "class/hwmon/hwmon5/name", "nct6775");
    write(root, "class/hwmon/hwmon5/pwm1", "100");
    write(root, "class/hwmon/hwmon5/pwm1_enable", "5");
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

[[devices]]
id = "case-fan"
driver = "hwmon-fan"
match = { name = "nct6775" }
config = { restore_mode = 5 }

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = { type = "cpu-thermal" }
"#,
    )
    .unwrap()
}

fn root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lemnosd-fans-{name}-{}", std::process::id()))
}

#[test]
fn service_hands_both_kinds_of_fan_back() {
    let root = root("service");
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

    // The plans were recorded at bind, before any client wrote.
    let plans = read_fan_state(&fan_state_path(&socket));
    assert_eq!(plans.len(), 2);
    assert_eq!(
        plans[0].kind,
        RestoreKind::CoolingDevice {
            enable: 1,
            devices: vec![root.join("class/thermal/cooling_device0")],
        }
    );
    assert_eq!(plans[1].kind, RestoreKind::Automatic { mode: 5 });

    // A client takes both fans over.
    let mut client = ClientOptions::new(&socket, "helios").devices().unwrap();
    assert_eq!(client.set("fan", "pwm_mode", 0.0).unwrap(), 0.0);
    assert_eq!(client.set("fan", "duty", 1.0).unwrap(), 1.0);
    assert_eq!(client.set("case-fan", "pwm_mode", 1.0).unwrap(), 1.0);
    drop(client);

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    // pwm-fan: the bind-time pwm1_enable, and the governor's level re-applied
    // (written as 0, then 1 again).
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");
    assert_eq!(read(&root, "class/thermal/cooling_device0/cur_state"), "1");
    // The chip: its automatic mode.
    assert_eq!(read(&root, "class/hwmon/hwmon5/pwm1_enable"), "5");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn stop_helper_uses_recorded_plans_then_falls_back() {
    let root = root("helper");
    tree(&root);
    let sys = SysRoot::new(&root);
    let state = root.join("run/fan-restore");
    fs::create_dir_all(state.parent().unwrap()).unwrap();

    // Recorded: the service bound the pwm-fan while it was at 2.
    let fan_dir = root.join("class/hwmon/hwmon2");
    let recorded = FanRestore {
        fan: fan_dir.clone(),
        kind: RestoreKind::CoolingDevice {
            enable: 2,
            devices: vec![root.join("class/thermal/cooling_device0")],
        },
    };
    lemnosd::fans::write_fan_state(&state, std::slice::from_ref(&recorded)).unwrap();
    write(&root, "class/hwmon/hwmon2/pwm1_enable", "0");
    write(&root, "class/hwmon/hwmon5/pwm1_enable", "1");
    let restored = restore_after_stop(Some(&board()), Some(&state), &sys, false);
    assert_eq!(restored.len(), 2);
    assert!(restored.iter().all(|r| r.result.is_ok()));
    assert_eq!(restored[0].plan, recorded);
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "2");
    assert_eq!(read(&root, "class/hwmon/hwmon5/pwm1_enable"), "5");

    // No record, no board (a crash before the first bind, `--all`): the
    // pwm-fan gets its boot default 1 and its cooling device, the chip 2.
    write(&root, "class/hwmon/hwmon2/pwm1_enable", "0");
    let restored = restore_after_stop(None, None, &sys, true);
    assert_eq!(restored.len(), 2);
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");
    assert!(matches!(
        restored[0].plan.kind,
        RestoreKind::CoolingDevice { enable: 1, .. }
    ));
    assert_eq!(read(&root, "class/hwmon/hwmon5/pwm1_enable"), "2");
    let _ = fs::remove_dir_all(&root);
}
