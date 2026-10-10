//! `gpio-power-switch` end to end over the mock server (mock GPIO only): the
//! line starts at its default level with no later write, a client's switch
//! is latched (it outlives the client), the write policy applies, a saved
//! state survives a restart, `on_exit` acts when the service stops, reset
//! cycles the line, and a tripped fault input degrades the device.

use lemnos_hal::raw::Direction;
use lemnos_ipc::{ClientError, ClientEvent, ClientOptions, DeviceClient, Event, Refusal, Update};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::path::PathBuf;
use std::time::Duration;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "usb-a-power"
driver = "gpio-power-switch"
writers = ["orion:*", "atlas"]
config = { chip = "pinctrl-rp1", line = 20, default_on = true, persist = true }

[[devices]]
id = "usb-c-power"
driver = "gpio-power-switch"
writers = ["atlas"]
config = { chip = "pinctrl-rp1", line = 16, default_on = true, on_exit = "off" }

[[devices]]
id = "aux-power"
driver = "gpio-power-switch"
poll_ms = 10
config = { chip = "pinctrl-rp1", line = 19, fault_line = 18, default_on = false }
"#;

fn hardware() -> MockHardware {
    let hardware = MockHardware::new();
    hardware.label_chip("pinctrl-rp1", "gpiochip0");
    hardware
}

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lemnosd-power-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    root
}

fn client(socket: &std::path::Path, name: &str) -> DeviceClient {
    ClientOptions::new(socket, name).devices().unwrap()
}

#[test]
fn starts_at_its_default_level_with_no_later_write() {
    let hw = hardware();
    let service = MockLemnosd::start_in(root("default"), BOARD, hw.clone()).unwrap();
    let line = hw.line("gpiochip0", 20);
    let configs = line.configs();
    assert_eq!(
        configs.len(),
        1,
        "requested once, at the default level: {configs:?}"
    );
    assert_eq!(configs[0].direction, Direction::Output);
    assert!(configs[0].initial);
    assert_eq!(line.level(), Some(true));
    let aux = hw.line("gpiochip0", 19);
    assert_eq!(aux.level(), Some(false), "aux-power defaults off");
    drop(service);
}

#[test]
fn a_write_drives_the_line_and_reads_back() {
    let hw = hardware();
    let service = MockLemnosd::start_in(root("write"), BOARD, hw.clone()).unwrap();
    let mut atlas = client(service.socket(), "atlas");
    assert_eq!(atlas.set("usb-c-power", "power.on", 0.0).unwrap(), 0.0);
    assert_eq!(hw.line("gpiochip0", 16).level(), Some(false));
    let reading = atlas.read("usb-c-power").unwrap();
    assert_eq!(reading.value("power.on"), Some(0.0));
    drop(service);
}

#[test]
fn a_write_outlives_the_client_that_made_it() {
    let hw = hardware();
    let service = MockLemnosd::start_in(root("latched"), BOARD, hw.clone()).unwrap();
    {
        let mut orion = client(service.socket(), "orion:bob");
        orion.set("usb-a-power", "power.on", 0.0).unwrap();
    }
    // Give the service a moment to see the disconnect.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(hw.line("gpiochip0", 20).level(), Some(false));
    let mut atlas = client(service.socket(), "atlas");
    assert_eq!(
        atlas.read("usb-a-power").unwrap().value("power.on"),
        Some(0.0)
    );
    drop(service);
}

#[test]
fn the_write_policy_applies() {
    let hw = hardware();
    let service = MockLemnosd::start_in(root("policy"), BOARD, hw.clone()).unwrap();
    let mut other = client(service.socket(), "helios");
    match other.set("usb-a-power", "power.on", 0.0) {
        Err(ClientError::Refused(Refusal::NotAllowed)) => {}
        result => panic!("expected a refusal, got {result:?}"),
    }
    assert_eq!(hw.line("gpiochip0", 20).level(), Some(true));
    drop(service);
}

#[test]
fn a_saved_state_is_the_start_state_after_a_restart() {
    let hw = hardware();
    let root = root("restart");
    let service = MockLemnosd::start_in(root.clone(), BOARD, hw.clone()).unwrap();
    client(service.socket(), "atlas")
        .set("usb-a-power", "power.on", 0.0)
        .unwrap();
    service.stop_keep_root();

    // A restart: the saved "off" is the default the line is requested at.
    let service = MockLemnosd::start_in(root, BOARD, hw.clone()).unwrap();
    let configs = hw.line("gpiochip0", 20).configs();
    assert!(!configs.last().unwrap().initial, "{configs:?}");
    assert_eq!(hw.line("gpiochip0", 20).level(), Some(false));
    drop(service);
}

#[test]
fn a_switch_without_persist_starts_at_its_configured_level() {
    let hw = hardware();
    let root = root("nopersist");
    let service = MockLemnosd::start_in(root.clone(), BOARD, hw.clone()).unwrap();
    client(service.socket(), "atlas")
        .set("usb-c-power", "power.on", 0.0)
        .unwrap();
    service.stop();
    let service = MockLemnosd::start_in(root, BOARD, hw.clone()).unwrap();
    // usb-c does not persist, so it is requested on again. Its on_exit
    // ("off") left it off, but the configured default is on.
    assert!(hw.line("gpiochip0", 16).configs().last().unwrap().initial);
    drop(service);
}

#[test]
fn on_exit_off_takes_the_line_low_when_the_service_stops() {
    let hw = hardware();
    let service = MockLemnosd::start_in(root("exit"), BOARD, hw.clone()).unwrap();
    assert_eq!(hw.line("gpiochip0", 16).level(), Some(true));
    service.stop();
    assert_eq!(hw.line("gpiochip0", 16).level(), Some(false));
    // keep (the default) leaves the line as it was.
    assert_eq!(hw.line("gpiochip0", 20).level(), Some(true));
}

#[test]
fn reset_cycles_the_switch_and_leaves_it_on() {
    let hw = hardware();
    let service = MockLemnosd::start_in(root("reset"), BOARD, hw.clone()).unwrap();
    let mut atlas = client(service.socket(), "atlas");
    atlas.set("usb-c-power", "power.reset", 20.0).unwrap();
    assert_eq!(hw.line("gpiochip0", 16).level(), Some(true));
    assert_eq!(
        atlas.read("usb-c-power").unwrap().value("power.on"),
        Some(1.0)
    );
    drop(service);
}

#[test]
fn a_tripped_fault_degrades_the_switch_and_clearing_it_restores_it() {
    let hw = hardware();
    let service = MockLemnosd::start_in(root("fault"), BOARD, hw.clone()).unwrap();
    let mut atlas = client(service.socket(), "atlas");
    let fault = hw.line("gpiochip0", 18);
    // Active low: the physical level 1 is no fault. Start clear, then trip.
    fault.drive(true);
    std::thread::sleep(Duration::from_millis(100));
    fault.drive(false);
    let degraded = wait_for_status(&mut atlas, "aux-power", "degraded");
    assert!(degraded, "a fault should degrade aux-power");
    fault.drive(true);
    assert!(wait_for_status(&mut atlas, "aux-power", "available"));
    drop(service);
}

fn wait_for_status(client: &mut DeviceClient, device: &str, want: &str) -> bool {
    for _ in 0..200 {
        if let Ok(Some(ClientEvent::Data(Update::Event(Event::Status {
            device: got,
            status,
            ..
        })))) = client.next_event_timeout(Duration::from_millis(50))
            && got == device
            && format!("{status:?}").to_lowercase() == want
        {
            return true;
        }
    }
    false
}
