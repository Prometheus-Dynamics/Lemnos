//! Allocations on the read-to-client path: a polled IMU read on its bus thread, handed to the
//! loop, encoded and written to a subscribed client. Counted over every thread but this one
//! (the client's decoding is not the service's).

#![allow(clippy::print_stdout)]

use lemnos_hal::register::AddressWidth;
use lemnos_ipc::{ClientEvent, ClientOptions, Update};
use lemnos_linux_sys::test_alloc::{self, ALLOCATIONS, Counting, TOTAL};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "alloc"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 5
config = { fifo = false, accel_rate = "200hz", gyro_rate = "200hz-23" }
"#;

fn service() -> MockLemnosd {
    let hardware = MockHardware::new();
    hardware
        .i2c(1)
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f])
        .with_sampler(0x18, 0x12, Duration::from_millis(5))
        .set_logging(false);
    MockLemnosd::start(BOARD, hardware).unwrap()
}

/// Readings received and allocations made by every other thread over `span`.
fn measure(client: &mut lemnos_ipc::DeviceClient, span: Duration, record: bool) -> (usize, usize) {
    let mine = ALLOCATIONS.with(std::cell::Cell::get);
    let all = TOTAL.load(Ordering::Relaxed);
    test_alloc::set_recording(record);
    let end = Instant::now() + span;
    let mut readings = 0;
    while Instant::now() < end {
        if let Ok(Some(ClientEvent::Data(Update::Reading(_)))) =
            client.next_event_timeout(Duration::from_millis(50))
        {
            readings += 1;
        }
    }
    test_alloc::set_recording(false);
    let mine = ALLOCATIONS.with(std::cell::Cell::get) - mine;
    let all = TOTAL.load(Ordering::Relaxed) - all;
    (readings, all.saturating_sub(mine))
}

/// After a warm-up (each sensor's first read, the frame pool filling), a subscribed reading
/// costs the service nothing from the allocator: not the bus thread's read, not the hand-off
/// to the loop, not the frame or the client's queue.
#[test]
fn a_subscribed_reading_allocates_nothing_in_the_service() {
    let service = service();
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    client.subscribe("imu", 5).unwrap();
    let _ = measure(&mut client, Duration::from_millis(500), false);
    let (readings, allocations) = measure(&mut client, Duration::from_secs(1), false);
    service.stop();
    assert!(readings >= 50, "only {readings} readings");
    assert_eq!(allocations, 0, "over {readings} readings");
}

#[test]
#[ignore = "diagnosis: prints where the service allocates per reading"]
fn where_the_read_path_allocates() {
    let service = service();
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    client.subscribe("imu", 5).unwrap();
    let _ = measure(&mut client, Duration::from_millis(500), false);
    let (readings, allocations) = measure(&mut client, Duration::from_secs(2), false);
    println!("unrecorded: {readings} readings, {allocations} allocations elsewhere");
    let (readings, allocations) = measure(&mut client, Duration::from_millis(300), true);
    let mut records = test_alloc::take_records();
    records.retain(|r| !r.starts_with("thread viewer") && !r.starts_with("thread where_"));
    println!("{readings} readings, {allocations} allocations elsewhere");
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for r in &records {
        // The first frame inside lemnos code names the site.
        let site: String = r
            .lines()
            .filter(|l| l.contains("lemnos") && !l.contains("test_alloc"))
            .take(6)
            .collect::<Vec<_>>()
            .join(" | ");
        *seen
            .entry(format!("{} :: {site}", r.lines().next().unwrap_or("")))
            .or_default() += 1;
    }
    let mut sites: Vec<_> = seen.into_iter().collect();
    sites.sort_by_key(|s| std::cmp::Reverse(s.1));
    for (site, n) in sites.iter().take(25) {
        println!("{n:6}  {site}");
    }
    if std::env::var_os("ALLOC_FULL").is_some() {
        let mut shown = std::collections::BTreeSet::new();
        for r in &records {
            let key: String = r
                .lines()
                .filter(|l| l.contains("lemnosd::"))
                .take(2)
                .collect();
            if !r.contains("lemnos_hal::mock") && shown.insert(key) {
                let frames: Vec<&str> = r
                    .lines()
                    .filter(|l| l.contains("lemnos") || l.contains(" at "))
                    .take(24)
                    .collect();
                println!("----\n{}", frames.join("\n"));
            }
        }
    }
    service.stop();
}
