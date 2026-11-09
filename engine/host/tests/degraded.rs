//! An optional part missing is degraded. A required part missing is failed.

mod common;

use common::{MAG, SIX_AXIS, host, host_with, method, quick};
use lemnos_core::{Decode, Health, Reader, Vector3};
use lemnos_host::DeviceState;
use std::time::Duration;

#[test]
fn no_magnetometer_gives_degraded_six_axis() {
    let (host, _) = host(SIX_AXIS);
    let info = host.device("orientation").unwrap();
    assert_eq!(info.state, DeviceState::Running);
    assert!(
        matches!(info.health, Health::Degraded(_)),
        "{:?}",
        info.health
    );

    let mut reply = [0; 32];
    let n = host.call(info.index, method(1), &[], &mut reply).unwrap();
    let value = Vector3::decode(&mut Reader::new(&reply[..n])).unwrap();
    assert_eq!(value.z, 0.0);
}

#[test]
fn a_missing_required_part_keeps_the_device_retrying() {
    let (host, clock) = host(
        r#"
        [[devices]]
        id = "orientation"
        driver = "orientation"
        uses = { accel = "absent", gyro = "gyro" }
        [[devices]]
        id = "gyro"
        driver = "fake-gyroscope"
        "#,
    );
    let info = host.device("orientation").unwrap();
    assert!(matches!(info.state, DeviceState::Backoff { .. }));
    assert!(matches!(info.health, Health::Failed(_)));
    assert_eq!(host.device("gyro").unwrap().state, DeviceState::Running);
    clock.advance(Duration::from_secs(60));
    host.tick();
    assert!(matches!(
        host.device("orientation").unwrap().state,
        DeviceState::Backoff { .. }
    ));
}

#[test]
fn health_passes_upward_when_a_child_goes_off() {
    let (host, _) = host(&format!("{SIX_AXIS}{MAG}"));
    assert_eq!(host.device("orientation").unwrap().health, Health::Ok);

    host.off("mag").unwrap();
    assert!(matches!(
        host.device("orientation").unwrap().health,
        Health::Degraded(_)
    ));
    host.on("mag").unwrap();

    host.off("imu-accel").unwrap();
    assert!(matches!(
        host.device("orientation").unwrap().health,
        Health::Failed(_)
    ));
}

#[test]
fn an_optional_part_that_arrives_later_is_picked_up() {
    let text = format!(
        "{SIX_AXIS}\n[[devices]]\nid = \"mag\"\ndriver = \"fake-magnetometer\"\ndisabled = true\n"
    );
    let (host, _) = host_with(&text, quick());
    assert!(matches!(
        host.device("orientation").unwrap().health,
        Health::Degraded(_)
    ));
    host.on("mag").unwrap();
    host.tick();
    assert_eq!(host.device("orientation").unwrap().health, Health::Ok);
}
