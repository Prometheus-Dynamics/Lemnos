#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::MethodId;
use lemnos_driver_orientation::Orientation;
use lemnos_fake_devices::{FakeAccelerometer, FakeCounter, FakeGyroscope, FakeMagnetometer};
use lemnos_host::board::parse;
use lemnos_host::{DriverEntry, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

pub fn drivers() -> Vec<DriverEntry> {
    vec![
        entry::<FakeAccelerometer>(),
        entry::<FakeGyroscope>(),
        entry::<FakeMagnetometer>(),
        entry::<FakeCounter>(),
        entry::<Orientation>(),
    ]
}

pub fn host_with(devices: &str, options: Options) -> (Host, SimClock) {
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}");
    let board = parse(&text).unwrap();
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &board,
        &drivers(),
        Box::new(sim),
        Arc::new(clock.clone()),
        options,
    )
    .unwrap();
    (host, clock)
}

pub fn host(devices: &str) -> (Host, SimClock) {
    host_with(devices, Options::default())
}

pub fn quick() -> Options {
    Options {
        call_deadline: Duration::from_millis(100),
        ..Options::default()
    }
}

pub fn method(id: u16) -> MethodId {
    MethodId {
        contract: 0,
        method: id,
    }
}

pub fn get_counter(host: &Host, device: u32) -> lemnos_core::Result<u32> {
    let mut reply = [0; 4];
    host.call(device, method(1), &[], &mut reply)?;
    Ok(u32::from_le_bytes(reply))
}

pub const COUNTERS: &str = r#"
[[devices]]
id = "bad"
driver = "fake-counter"

[[devices]]
id = "good"
driver = "fake-counter"
"#;

pub const SIX_AXIS: &str = r#"
[[devices]]
id = "orientation"
driver = "orientation"
uses = { accel = "imu-accel", gyro = "imu-gyro", mag = "mag" }

[[devices]]
id = "imu-accel"
driver = "fake-accelerometer"
config = { x = 0.0, y = 0.0, z = 9.8 }

[[devices]]
id = "imu-gyro"
driver = "fake-gyroscope"
"#;

pub const MAG: &str = r#"
[[devices]]
id = "mag"
driver = "fake-magnetometer"
config = { x = 0.00002, y = 0.0, z = 0.0 }
"#;
