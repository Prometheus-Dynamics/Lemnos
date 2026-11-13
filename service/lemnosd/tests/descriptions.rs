//! What a UI needs from a device's description, for every device the simulated board runs.
//! The walk fails when a running device's description lacks a label, a kind or a state, or
//! has an item with no reading type. No code here knows a kind of device.

mod common;

use common::start_board;
use lemnos_client::Client;
use lemnos_fake_devices::{FakeAccelerometer, FakeCounter};
use lemnos_host::{DriverEntry, entry};
use lemnos_sim::{Layout, SimBoard};
use lemnosd::builtin;

/// The built-in drivers, and the fakes that run on any board.
fn drivers() -> Vec<DriverEntry> {
    let mut drivers = builtin::drivers();
    drivers.push(entry::<FakeAccelerometer>());
    drivers.push(entry::<FakeCounter>());
    drivers
}

const BUS_DEVICES: &str = "\
[[devices]]\nid = \"power\"\ndriver = \"ina2xx\"\nbus = \"i2c-1\"\n\
config = { chip = \"ina226\", shunt_ohms = 0.01, max_current_a = 8.192 }\n\
[[devices]]\nid = \"imu\"\ndriver = \"bmi088\"\nbus = \"i2c-1\"\n\
[[devices]]\nid = \"orientation\"\ndriver = \"orientation\"\nuses = { accel = \"imu\", gyro = \"imu\" }\n\
[[devices]]\nid = \"switch\"\ndriver = \"gpio-power-switch\"\nbus = \"gpio-power\"\n\
[[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\nbus = \"pwm-fan\"\n\
[[devices]]\nid = \"light\"\ndriver = \"ws2812\"\nbus = \"spidev0.0\"\nconfig = { count = 8 }\n\
[[devices]]\nid = \"fake-accel\"\ndriver = \"fake-accelerometer\"\n\
[[devices]]\nid = \"counter\"\ndriver = \"fake-counter\"\nconfig = { start = 41 }\n\
[[devices]]\nid = \"stuck\"\ndriver = \"bmi088\"\nbus = \"i2c-9\"\n\
label = \"Not here\"\n";

/// The simulated chips: an INA226 at 0x40 (manufacturer and die ids), a BMI088's two dies.
fn board() -> SimBoard {
    let board = SimBoard::new();
    board.add_line("gpio-power");
    board.add_pwm("pwm-fan");
    board.add_spi("spidev0.0");
    let sim = board.add_i2c("i2c-1");
    sim.attach(0x40, Layout::Words);
    sim.set_word(0x40, 0xfe, &0x5449u16.to_be_bytes());
    sim.set_word(0x40, 0xff, &0x2260u16.to_be_bytes());
    sim.attach(0x18, Layout::Bytes);
    sim.set_bytes(0x18, 0x00, &[0x1e]);
    sim.attach(0x68, Layout::Bytes);
    sim.set_bytes(0x68, 0x00, &[0x0f]);
    board
}

#[test]
fn every_running_device_describes_its_name_kind_state_and_readings() {
    let running = start_board(BUS_DEVICES, drivers(), board(), None);
    let client = Client::connect(&running.socket, "descriptions").unwrap();
    let mut seen = Vec::new();
    for entry in client.list().unwrap() {
        if entry.state != lemnos_client::DeviceState::Running {
            // Not running: no description, and the list says why.
            assert!(
                !entry.reason.is_empty(),
                "{} is not running and says why not",
                entry.name
            );
            continue;
        }
        let description = client.describe(entry.index).unwrap();
        assert!(!description.label.is_empty(), "{} has no label", entry.name);
        assert!(!description.kind.is_empty(), "{} has no kind", entry.name);
        assert!(description.state.is_some(), "{} has no state", entry.name);
        for item in &description.items {
            assert!(
                item.reading.is_some(),
                "{}.{} has no reading type",
                entry.name,
                item.name
            );
        }
        seen.push((entry.name.clone(), description.kind.clone()));
    }
    let names: Vec<&str> = seen.iter().map(|(name, _)| name.as_str()).collect();
    assert!(
        names.contains(&"power"),
        "the power monitor runs: {names:?}"
    );
    assert!(names.contains(&"imu"), "the IMU runs: {names:?}");
    assert!(
        names.contains(&"orientation"),
        "the orientation runs: {names:?}"
    );
    assert!(seen.contains(&("power".to_string(), "power".to_string())));
    assert!(seen.contains(&("imu".to_string(), "sensor".to_string())));
}

#[test]
fn a_device_that_does_not_run_says_why_and_has_no_description() {
    let running = start_board(BUS_DEVICES, drivers(), board(), None);
    let client = Client::connect(&running.socket, "descriptions").unwrap();
    let entry = client
        .list()
        .unwrap()
        .into_iter()
        .find(|d| d.name == "stuck")
        .unwrap();
    assert_ne!(entry.state, lemnos_client::DeviceState::Running);
    assert!(!entry.reason.is_empty());
    assert!(client.describe(entry.index).is_err());
}

#[test]
fn every_built_in_contract_declares_a_kind() {
    for driver in builtin::drivers() {
        for contract in driver.manifest().provides {
            assert!(
                [
                    "sensor", "actuator", "camera", "power", "system", "light", "bus", "compute",
                    "other"
                ]
                .contains(&contract.kind),
                "{} provides {} with kind {:?}",
                driver.manifest().name,
                contract.tag.as_str(),
                contract.kind
            );
        }
    }
}
