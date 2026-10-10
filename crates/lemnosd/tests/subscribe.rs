//! Subscription answers: the granted period, and the refusals that say why
//! nothing will arrive.

use lemnos_hal::register::AddressWidth;
use lemnos_ipc::{ClientError, ClientOptions, Refusal};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "subscribe"

[[devices]]
id = "slow"
driver = "bmi088"
bus = "i2c-2"
address = 0x18
backend = "userspace"
poll_ms = 10

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/dev/leds0"
config = { count = 4, offset = 0, fade_ms = 0, status_effect = "solid", ok = 0x00ff00 }
"#;

fn start() -> MockLemnosd {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // One directory per service: the tests run in parallel.
    let root = std::env::temp_dir().join(format!(
        "lemnosd-subscribe-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("sys")).unwrap();
    fs::create_dir_all(root.join("dev")).unwrap();
    fs::write(root.join("dev/leds0"), "").unwrap();
    let hardware = MockHardware::new();
    hardware
        .i2c(2)
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f]);
    MockLemnosd::start_in(root, BOARD, hardware).unwrap()
}

#[test]
fn subscribe_says_what_it_granted_and_why_it_refused() {
    let service = start();
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();

    assert_eq!(client.subscribe("slow", 10).unwrap(), 10);
    // Unsubscribing is always granted.
    assert_eq!(client.subscribe("slow", 0).unwrap(), 0);

    assert!(matches!(
        client.subscribe("nope", 10),
        Err(ClientError::Refused(Refusal::UnknownDevice))
    ));
    // A light produces no readings.
    assert!(matches!(
        client.subscribe("ring", 10),
        Err(ClientError::Refused(Refusal::Unsupported))
    ));
}

#[test]
fn the_granted_period_is_at_least_the_device_read_time() {
    // Each read is two transactions of 8 ms: no faster than 16 ms. The
    // latency is set once the device is up (its init runs on the loop). An
    // idle IMU is not read, so the first subscription makes the service read
    // it and measure the read; the grant after that reflects the measurement.
    let service = start();
    service
        .hardware()
        .i2c(2)
        .set_latency(Duration::from_millis(8));
    std::thread::sleep(Duration::from_millis(200));
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    client.subscribe("slow", 1).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let granted = client.subscribe("slow", 1).unwrap();
    assert!((16..=20).contains(&granted), "{granted}");
    // A period longer than the read time is granted as asked.
    assert_eq!(client.subscribe("slow", 50).unwrap(), 50);
}

#[test]
fn a_channel_subscription_gets_its_channels_and_refuses_unknown_names() {
    use lemnos_device::NO_VALUE;
    use lemnos_ipc::{ClientEvent, Update};
    let service = start();
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();

    assert_eq!(
        client
            .subscribe_channels("slow", &["angular_rate.z"], 10)
            .unwrap(),
        10
    );
    assert!(matches!(
        client.subscribe_channels("slow", &["angular_rate.q"], 10),
        Err(ClientError::Refused(Refusal::UnknownChannel))
    ));
    assert!(matches!(
        client.subscribe_channels("slow", &["nope.*"], 10),
        Err(ClientError::Refused(Refusal::UnknownChannel))
    ));
    // Only the yaw rate has a value; the other channels have none.
    let mut readings = 0;
    while readings < 3 {
        // Connection and status events come too: skip anything else.
        let r = loop {
            match client.next_event_timeout(Duration::from_secs(2)).unwrap() {
                Some(ClientEvent::Data(Update::Reading(r))) => break r,
                Some(_) => continue,
                None => panic!("no readings"),
            }
        };
        if r.device != "slow" {
            continue;
        }
        readings += 1;
        assert_eq!(r.raw.len(), 6);
        assert!(r.raw[..5].iter().all(|v| *v == NO_VALUE), "{:?}", r.raw);
        assert_ne!(r.raw[5], NO_VALUE);
    }
    // Ending the selection stops its readings.
    assert_eq!(
        client
            .subscribe_channels("slow", &["angular_rate.z"], 0)
            .unwrap(),
        0
    );
}
