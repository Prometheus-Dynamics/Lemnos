//! The fan controller on the simulated board's fakes, as `lemnosd --sim` runs it: `fan-control`
//! binds to a `fake-fan` and a `fake-thermal-zone`, follows its curve, and fails the way the real
//! fan and zone fail.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Running, start_board};
use lemnos_client::{Client, DeviceHealth};
use lemnos_contract_fan::Fan;
use lemnos_contract_fan_control::FanControl;
use lemnos_driver_fan_control::FanController;
use lemnos_fake_devices::{FakeFan, FakeThermalZone};
use lemnos_host::{DriverEntry, entry};
use lemnos_sim::SimBoard;

/// The drivers `lemnosd --sim` serves for these devices.
fn drivers() -> Vec<DriverEntry> {
    vec![
        entry::<FanController>(),
        entry::<FakeFan>(),
        entry::<FakeThermalZone>(),
    ]
}

/// The fan's duty before the controller takes it: pwm1 179 on the B14 board, which is 702 per mille.
const BASELINE: u16 = 702;

/// The board file of the rehearsal's shape: a fan, a zone at `zone`, and the controller wired to
/// both with the B14 curve. The fan starts at [`BASELINE`] and keeps its duty in `fan` beside the
/// zone's directory, so a test can read it once the daemon has stopped. `fan_extra` and `zone_extra`
/// add settings to the two fakes.
fn board(zone: &Path, fan_extra: &str, zone_extra: &str, max_age_ms: u32) -> String {
    let fan = zone.parent().unwrap().join("fan");
    let fan = fan.display();
    let zone = zone.display();
    format!(
        "[[devices]]\nid = \"fan\"\ndriver = \"fake-fan\"\nconfig = {{ duty = {BASELINE}, file = \"{fan}\"{fan_extra} }}\n\
         [[devices]]\nid = \"cpu-thermal\"\ndriver = \"fake-thermal-zone\"\n\
         config = {{ path = \"{zone}\"{zone_extra} }}\n\
         [[devices]]\nid = \"fan-control\"\ndriver = \"fan-control\"\n\
         uses = {{ fan = \"fan\", t0 = \"cpu-thermal\" }}\n\
         config = {{ curve_t0 = \"30000:300,60000:600,75000:1000\", floor = 300, slew_per_s = 50, \
         max_age_ms = {max_age_ms} }}\n"
    )
}

/// A scratch directory for one test's zone, with its name.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fan-control-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("zone")).unwrap();
    dir
}

/// Sets the zone's temperature the way a sysfs zone's `temp` reads it: millidegrees.
fn set_temp(zone: &Path, millidegrees: i32) {
    fs::write(zone.join("temp"), format!("{millidegrees}\n")).unwrap();
}

/// Waits until `done` holds, with the step each second on the controller in view. Panics on
/// `what` when it does not hold in time.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn health_of(client: &Client, name: &str) -> DeviceHealth {
    client
        .list()
        .unwrap()
        .into_iter()
        .find(|device| device.name == name)
        .unwrap_or_else(|| panic!("no device {name}"))
        .health
}

fn index_of(client: &Client, name: &str) -> u32 {
    client
        .list()
        .unwrap()
        .into_iter()
        .find(|device| device.name == name)
        .unwrap_or_else(|| panic!("no device {name}"))
        .index
}

#[test]
fn the_controller_binds_to_the_fakes_and_follows_its_curve() {
    let dir = scratch("curve");
    let zone = dir.join("zone");
    set_temp(&zone, 45_000);
    let running: Running = start_board(
        &board(&zone, "", "", 3000),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    let mut fan = client.open::<dyn Fan>("fan").unwrap();

    // Bound and running: the controller is listed healthy, and its input and fan are there.
    assert_eq!(health_of(&client, "fan-control"), DeviceHealth::Ok);

    // 45 °C is halfway up the first segment: 300 + (15 / 30) * 300.
    wait_for("45 C at 450", || control.state().unwrap().duty == 450);
    assert_eq!(
        fan.duty().unwrap(),
        450,
        "the fan is driven to the curve's duty"
    );

    set_temp(&zone, 60_000);
    wait_for("60 C at 600", || control.state().unwrap().duty == 600);
    assert_eq!(fan.duty().unwrap(), 600);

    // 70 °C is two thirds up the last segment: 600 + (10 / 15) * 400, which is 866.67.
    set_temp(&zone, 70_000);
    wait_for("70 C near 867", || {
        let duty = control.state().unwrap().duty;
        (866..=867).contains(&duty)
    });
    assert_eq!(fan.duty().unwrap(), control.state().unwrap().duty as u16);

    // Cooling: the duty falls, and never by more than the slew limit per second. Each reading is
    // taken a little after the one before, so the bound is 50 per second of the gap, with one
    // second of slack for the step that may fall between two readings.
    set_temp(&zone, 30_000);
    let mut last = (Instant::now(), control.state().unwrap().duty);
    wait_for("cooled to the floor of 300", || {
        let now = Instant::now();
        let duty = control.state().unwrap().duty;
        let gap = now.duration_since(last.0).as_secs_f64();
        assert!(
            duty <= last.1 && f64::from(last.1 - duty) <= 50.0 * (gap + 1.0),
            "the duty fell from {} to {duty} in {gap:.2} s",
            last.1
        );
        last = (now, duty);
        duty == 300
    });
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_zone_that_fails_as_the_real_one_does_drops_the_fan_to_full_duty() {
    // A zone whose read fails with IO, the status a zone that will not give a number returns.
    let dir = scratch("io");
    let zone = dir.join("zone");
    set_temp(&zone, 30_000);
    let running = start_board(
        &board(&zone, "", ", fail = true", 500),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    let mut fan = client.open::<dyn Fan>("fan").unwrap();

    wait_for("the stale input takes the fan to full duty", || {
        let state = control.state().unwrap();
        state.degraded && state.duty == 1000
    });
    assert_eq!(fan.duty().unwrap(), 1000);
    assert_eq!(health_of(&client, "fan-control"), DeviceHealth::Degraded);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_zone_with_no_temp_file_fails_as_a_missing_zone_and_recovers() {
    // The zone's directory has no `temp`: its read fails with NOT_FOUND, as a missing sysfs zone's.
    let dir = scratch("missing");
    let zone = dir.join("zone");
    let running = start_board(&board(&zone, "", "", 500), drivers(), SimBoard::new(), None);
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();

    wait_for("a missing zone takes the fan to full duty", || {
        let state = control.state().unwrap();
        state.degraded && state.duty == 1000
    });
    // The zone appears, and the curves take over again.
    set_temp(&zone, 45_000);
    wait_for("the zone comes back on its curve", || {
        let state = control.state().unwrap();
        !state.degraded && state.duty == 450
    });
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn switching_the_zone_off_on_the_host_is_a_failed_sensor_until_it_is_back() {
    let dir = scratch("off");
    let zone = dir.join("zone");
    set_temp(&zone, 45_000);
    let running = start_board(&board(&zone, "", "", 500), drivers(), SimBoard::new(), None);
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    wait_for("45 C at 450", || control.state().unwrap().duty == 450);

    let zone_index = index_of(&client, "cpu-thermal");
    client.set_power(zone_index, false).unwrap();
    wait_for("the zone off takes the fan to full duty", || {
        let state = control.state().unwrap();
        state.degraded && state.duty == 1000
    });

    client.set_power(zone_index, true).unwrap();
    wait_for("the zone on comes back on its curve", || {
        let state = control.state().unwrap();
        !state.degraded && state.duty == 450
    });
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_fan_that_refuses_its_writes_is_degraded_and_keeps_its_duty() {
    // The fan refuses every write with IO, as hwmon does when the kernel says no. The controller
    // wants 300 for 30 C, and the fan stays where it was: at its baseline.
    let dir = scratch("refuse");
    let zone = dir.join("zone");
    set_temp(&zone, 30_000);
    let running = start_board(
        &board(&zone, ", fail = true", "", 3000),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut fan = client.open::<dyn Fan>("fan").unwrap();

    wait_for("the refused write degrades the controller", || {
        health_of(&client, "fan-control") == DeviceHealth::Degraded
    });
    assert_eq!(
        fan.duty().unwrap(),
        BASELINE,
        "a refused write leaves the fan's duty alone"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Waits up to `secs` for `done`, stepping in real time. Panics on `what` when it does not hold in time.
fn wait_up_to(secs: u64, what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The state the controller reports, or `None` when its duty is unknown (the state read fails).
fn state_duty(control: &mut dyn FanControl) -> Option<i32> {
    control.state().ok().map(|state| state.duty)
}

#[test]
fn case_1_writes_refused_and_the_drop_lands_the_duty_is_read_back_then_unknown_then_read_again_on_rebind()
 {
    // The fan refuses its writes and its release, so the fan stays at 450 and the duty reads back 450. The host
    // binds it out after five refusals; the drop (the fake's default: its drop lands) puts it at full duty. While it is
    // out nothing can be read, so the duty is unknown. Bound again, it is at full duty, and control resumes.
    let dir = scratch("boundout");
    let zone = dir.join("zone");
    set_temp(&zone, 45_000);
    let fan_file = dir.join("fan");
    let refusal = dir.join("fan.refuse");
    let mut running = start_board(
        &board(&zone, "", "", 3000),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    wait_for("45 C at 450", || control.state().unwrap().duty == 450);

    fs::write(&refusal, "").unwrap();
    wait_up_to(20, "the refused writes degrade the controller", || {
        health_of(&client, "fan-control") == DeviceHealth::Degraded
    });
    assert_eq!(
        state_duty(&mut control),
        Some(450),
        "the refused writes are read back: the fan is at 450"
    );
    assert_eq!(
        fs::read_to_string(&fan_file).unwrap().trim(),
        "450",
        "the fan has not moved"
    );

    wait_up_to(45, "the host binds the fan out", || {
        health_of(&client, "fan") == DeviceHealth::Failed
    });
    wait_up_to(5, "the drop lands: the fan is at full duty", || {
        fs::read_to_string(&fan_file).unwrap().trim() == "1000"
    });
    assert_eq!(
        state_duty(&mut control),
        None,
        "bound out: the duty cannot be read, so it is unknown, never a number"
    );

    fs::remove_file(&refusal).unwrap();
    wait_up_to(60, "the fan is bound again", || {
        health_of(&client, "fan") == DeviceHealth::Ok
    });
    wait_up_to(20, "control resumes on the curve", || {
        state_duty(&mut control) == Some(450)
            && health_of(&client, "fan-control") == DeviceHealth::Ok
    });

    running.service.take().unwrap().stop();
    assert_eq!(
        fs::read_to_string(&fan_file).unwrap().trim(),
        BASELINE.to_string(),
        "a stop after the fan was bound out and bound again hands it back to its baseline"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn case_2_writes_refused_and_the_drop_refused_too_the_duty_is_the_last_duty_and_control_resumes_on_rebind()
 {
    // The fan refuses its writes, its release and its drop: it stays at 600 all through, and the duty reads back 600.
    // Bound out, the duty is unknown. Bound again, it reads 600 and control resumes.
    let dir = scratch("refuse-drop");
    let zone = dir.join("zone");
    set_temp(&zone, 60_000);
    let fan_file = dir.join("fan");
    let refusal = dir.join("fan.refuse");
    let drop_refusal = dir.join("fan.refuse-drop");
    let mut running = start_board(
        &board(&zone, "", "", 3000),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    wait_for("60 C at 600", || control.state().unwrap().duty == 600);

    fs::write(&refusal, "").unwrap();
    fs::write(&drop_refusal, "").unwrap();
    wait_up_to(20, "the refused writes degrade the controller", || {
        health_of(&client, "fan-control") == DeviceHealth::Degraded
    });
    assert_eq!(
        state_duty(&mut control),
        Some(600),
        "the refused writes are read back: the fan is at 600"
    );

    wait_up_to(45, "the host binds the fan out", || {
        health_of(&client, "fan") == DeviceHealth::Failed
    });
    assert_eq!(
        fs::read_to_string(&fan_file).unwrap().trim(),
        "600",
        "the drop was refused: the fan stays at 600"
    );
    assert_eq!(state_duty(&mut control), None, "bound out: unknown");

    fs::remove_file(&refusal).unwrap();
    fs::remove_file(&drop_refusal).unwrap();
    wait_up_to(60, "the fan is bound again", || {
        health_of(&client, "fan") == DeviceHealth::Ok
    });
    wait_up_to(20, "control resumes on the curve", || {
        state_duty(&mut control) == Some(600)
            && health_of(&client, "fan-control") == DeviceHealth::Ok
    });

    running.service.take().unwrap().stop();
    assert_eq!(
        fs::read_to_string(&fan_file).unwrap().trim(),
        BASELINE.to_string(),
        "a stop hands the fan back to its baseline"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn case_3_writes_and_reads_both_fail_the_duty_is_absent_and_control_resumes_when_bound_again() {
    // The fan refuses its writes, its release and its drop, and its reads fail too. The duty is unknown from the first
    // failure, and stays unknown when the host binds the fan out. Bound again, the reads still fail: the controller's
    // write takes, and control resumes.
    let dir = scratch("unreadable");
    let zone = dir.join("zone");
    set_temp(&zone, 45_000);
    let fan_file = dir.join("fan");
    let refusal = dir.join("fan.refuse");
    let drop_refusal = dir.join("fan.refuse-drop");
    let unreadable = dir.join("fan.unreadable");
    let mut running = start_board(
        &board(&zone, "", "", 3000),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    wait_for("45 C at 450", || control.state().unwrap().duty == 450);

    fs::write(&refusal, "").unwrap();
    fs::write(&drop_refusal, "").unwrap();
    fs::write(&unreadable, "").unwrap();
    wait_up_to(
        20,
        "the refused writes degrade the controller, and the duty is unknown",
        || {
            health_of(&client, "fan-control") == DeviceHealth::Degraded
                && state_duty(&mut control).is_none()
        },
    );

    wait_up_to(45, "the host binds the fan out", || {
        health_of(&client, "fan") == DeviceHealth::Failed
    });
    assert_eq!(state_duty(&mut control), None, "bound out: unknown");

    fs::remove_file(&refusal).unwrap();
    fs::remove_file(&drop_refusal).unwrap();
    wait_up_to(60, "the fan is bound again", || {
        health_of(&client, "fan") == DeviceHealth::Ok
    });
    wait_up_to(
        20,
        "the controller's write takes, and control is back",
        || health_of(&client, "fan-control") == DeviceHealth::Ok,
    );
    assert_eq!(
        state_duty(&mut control),
        Some(450),
        "the duty the controller wrote: the reads still fail, so it is the written one"
    );

    fs::remove_file(&unreadable).unwrap();
    running.service.take().unwrap().stop();
    assert_eq!(
        fs::read_to_string(&fan_file).unwrap().trim(),
        BASELINE.to_string(),
        "a stop hands the fan back to its baseline"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disabling_the_controller_hands_the_fan_back_to_its_baseline() {
    let dir = scratch("disable");
    let zone = dir.join("zone");
    set_temp(&zone, 45_000);
    let running = start_board(
        &board(&zone, "", "", 3000),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    let mut fan = client.open::<dyn Fan>("fan").unwrap();
    wait_for("45 C at 450", || control.state().unwrap().duty == 450);
    assert_eq!(fan.duty().unwrap(), 450);

    control.set_enabled(false).unwrap();
    wait_for("the fan is handed back", || fan.duty().unwrap() != 450);
    assert_eq!(
        fan.duty().unwrap(),
        BASELINE,
        "set_enabled(false) hands the fan back to the duty it had before the take-over"
    );
    // A disabled controller writes nothing: the fan stays at the baseline.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        fan.duty().unwrap(),
        BASELINE,
        "nothing is written while disabled"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stopping_the_daemon_hands_the_fan_back_to_its_baseline() {
    let dir = scratch("stop");
    let zone = dir.join("zone");
    set_temp(&zone, 45_000);
    let mut running = start_board(
        &board(&zone, "", "", 3000),
        drivers(),
        SimBoard::new(),
        None,
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();
    wait_for("45 C at 450", || control.state().unwrap().duty == 450);

    // A SIGTERM does one thing in lemnosd: `Service::stop`, after which the host drops every
    // driver. The fan's duty is then read from its file, as the daemon is gone.
    running.service.take().unwrap().stop();
    let left = fs::read_to_string(dir.join("fan")).unwrap();
    assert_eq!(
        left.trim(),
        BASELINE.to_string(),
        "a stopped daemon hands the fan back to its baseline"
    );
    let _ = fs::remove_dir_all(&dir);
}
