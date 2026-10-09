//! The fan hand-back, end to end over a fake sysfs tree: a `pwm-fan` fan
//! (driver link, thermal cooling device) and a fan-controller chip with an
//! automatic mode, handed back by the service on stop and by the stop helper
//! (`lemnos-ctl fan restore`) after the process is gone.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::{CoolingRecord, FanRestore, RestoreKind, SysRoot};
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
    let zone = "devices/virtual/thermal/thermal_zone0";
    write(root, &format!("{zone}/type"), "cpu-thermal");
    write(root, &format!("{zone}/temp"), "57850");
    write(root, &format!("{zone}/policy"), "step_wise");
    link(root, &format!("{zone}/cdev0"), cdev);
    link(root, "class/thermal/thermal_zone0", zone);

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

    // The plans were recorded at bind. Until a client writes, the governor
    // owns the cooling device, so the stop helper's record holds no state
    // to restore (only pwm1_enable and the governor kick).
    let cdev = root.join("class/thermal/cooling_device0");
    let plans = read_fan_state(&fan_state_path(&socket));
    assert_eq!(plans.len(), 2);
    assert_eq!(
        plans[0].kind,
        RestoreKind::CoolingDevice {
            enable: 1,
            devices: vec![CoolingRecord {
                device: cdev.clone(),
                state: None,
            }],
        }
    );
    assert_eq!(plans[1].kind, RestoreKind::Automatic { mode: 5 });

    // The governor steps up to 2, then a client takes both fans over; the
    // kernel moves cur_state to match the client's pwm1 (Raze: 4).
    write(
        &root,
        "devices/virtual/thermal/cooling_device0/cur_state",
        "2",
    );
    let mut client = ClientOptions::new(&socket, "helios").devices().unwrap();
    assert_eq!(client.set("fan", "pwm_mode", 0.0).unwrap(), 0.0);
    write(
        &root,
        "devices/virtual/thermal/cooling_device0/cur_state",
        "4",
    );
    assert_eq!(client.set("fan", "duty", 1.0).unwrap(), 1.0);
    assert_eq!(client.set("case-fan", "pwm_mode", 1.0).unwrap(), 1.0);
    // The record holds the governor's state from just before the first write.
    // Read it while the writer is connected.
    let plans = read_fan_state(&fan_state_path(&socket));
    assert!(matches!(
        &plans[0].kind,
        RestoreKind::CoolingDevice { devices, .. } if devices[0].state == Some(2)
    ));
    drop(client);
    // The service hands the fan back when the writer disconnects and then
    // rewrites the record with the states left out. Wait for that rewrite
    // before the checks below: the service only stops after this.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !matches!(
        read_fan_state(&fan_state_path(&socket)).first().map(|p| &p.kind),
        Some(RestoreKind::CoolingDevice { devices, .. }) if devices[0].state.is_none()
    ) {
        assert!(
            std::time::Instant::now() < deadline,
            "the writer's disconnect never handed the fan back"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Make the policy write visible: date the file to the epoch.
    let policy = root.join("class/thermal/thermal_zone0/policy");
    fs::File::options()
        .write(true)
        .open(&policy)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    // pwm-fan: the bind-time pwm1_enable, the governor's state from before
    // the client, and the zone's governor re-evaluated (policy written back).
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");
    assert_eq!(read(&root, "class/thermal/cooling_device0/cur_state"), "2");
    assert_eq!(
        read(&root, "class/thermal/thermal_zone0/policy"),
        "step_wise"
    );
    assert!(fs::metadata(&policy).unwrap().modified().unwrap() > std::time::UNIX_EPOCH);
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
            devices: vec![CoolingRecord {
                device: root.join("class/thermal/cooling_device0"),
                state: Some(3),
            }],
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
    assert_eq!(read(&root, "class/thermal/cooling_device0/cur_state"), "3");
    assert_eq!(read(&root, "class/hwmon/hwmon5/pwm1_enable"), "5");

    // No record, no board (`--all` after a crash before the record was
    // written): the pwm-fan gets its boot default 1 and only a governor
    // re-evaluation (its state stays until then), the chip 2.
    write(&root, "class/hwmon/hwmon2/pwm1_enable", "0");
    write(
        &root,
        "devices/virtual/thermal/cooling_device0/cur_state",
        "4",
    );
    let restored = restore_after_stop(None, None, &sys, true);
    assert_eq!(restored.len(), 2);
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");
    assert_eq!(read(&root, "class/thermal/cooling_device0/cur_state"), "4");
    assert!(matches!(
        restored[0].plan.kind,
        RestoreKind::CoolingDevice { enable: 1, .. }
    ));
    assert_eq!(read(&root, "class/hwmon/hwmon5/pwm1_enable"), "2");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn release_hands_a_fan_back_while_the_service_runs() {
    let root = root("release");
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
    let cdev_state = "devices/virtual/thermal/cooling_device0/cur_state";
    let state = |root: &Path| read(root, "class/thermal/cooling_device0/cur_state");

    // Take the fan over (the governor was at 1; the kernel follows pwm1 to 4).
    let mut client = ClientOptions::new(&socket, "helios").devices().unwrap();
    assert_eq!(client.set("fan", "pwm_mode", 0.0).unwrap(), 0.0);
    write(&root, cdev_state, "4");
    assert_eq!(client.set("fan", "duty", 1.0).unwrap(), 1.0);

    // Release: pwm1_enable and the governor's state come back, the zone
    // re-evaluates, and the service keeps running.
    client.release("fan").unwrap();
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");
    assert_eq!(state(&root), "1");
    // The stop helper's record no longer holds a governor state to restore.
    let plans = read_fan_state(&fan_state_path(&socket));
    assert!(matches!(
        &plans[0].kind,
        RestoreKind::CoolingDevice { devices, .. } if devices[0].state.is_none()
    ));
    // Only fans can be released; the write policy applies.
    assert!(client.release("cpu-thermal").is_err());
    assert!(client.release("nope").is_err());
    let mut other = ClientOptions::new(&socket, "photonvision")
        .devices()
        .unwrap();
    assert!(
        other.release("fan").is_ok(),
        "no writers: anyone may release"
    );
    drop(other);

    // The governor moves on; the next write takes the fan back, recording
    // the governor's state at that moment.
    write(&root, cdev_state, "2");
    assert_eq!(client.set("fan", "duty", 0.9).unwrap(), 0.902);
    write(&root, cdev_state, "4");
    let plans = read_fan_state(&fan_state_path(&socket));
    assert!(matches!(
        &plans[0].kind,
        RestoreKind::CoolingDevice { devices, .. } if devices[0].state == Some(2)
    ));
    client.release("fan").unwrap();
    assert_eq!(state(&root), "2");

    // Released at stop: the governor's later state is left alone.
    write(&root, cdev_state, "3");
    drop(client);
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    assert_eq!(state(&root), "3");
    assert_eq!(read(&root, "class/hwmon/hwmon2/pwm1_enable"), "1");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_fan_override_ends_with_the_writers_connection() {
    let root = root("disconnect");
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
    let cdev_state = "devices/virtual/thermal/cooling_device0/cur_state";

    // HeliOS takes the fan over, then dies without cleaning up.
    let mut helios = ClientOptions::new(&socket, "helios").devices().unwrap();
    assert_eq!(helios.set("fan", "pwm_mode", 0.0).unwrap(), 0.0);
    write(&root, cdev_state, "4");
    drop(helios);
    // The service, still running, hands the fan back to the governor.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while read(&root, "class/hwmon/hwmon2/pwm1_enable") != "1" {
        assert!(std::time::Instant::now() < deadline, "not handed back");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(read(&root, "class/thermal/cooling_device0/cur_state"), "1");

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    let _ = fs::remove_dir_all(&root);
}
