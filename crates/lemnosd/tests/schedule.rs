//! The read scheduler and the reading timestamps, over the real service with
//! mock buses. The slow bus takes a fixed time per transaction
//! (`MockI2c::set_latency`), so its device's reads take real time.

use lemnos_hal::register::AddressWidth;
use lemnos_ipc::{ClientEvent, ClientOptions, Update};
use lemnos_linux_sys::time::boottime_us;
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::time::{Duration, Instant};

/// The slow device comes first, so a fixed slot order would poll it first.
const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "sched"

[[devices]]
id = "slow"
driver = "bmi088"
bus = "i2c-2"
address = 0x18
backend = "userspace"
poll_ms = 10

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 10
"#;

const PERIOD_US: i64 = 10_000;

#[derive(Debug)]
struct Stats {
    rate_hz: f64,
    /// How far the spacing between readings is from 10 ms.
    p99_dev_us: i64,
    max_dev_us: i64,
    /// How old each reading was when the client received it (receive time
    /// minus its timestamp).
    p99_age_us: i64,
}

/// One reading as the client saw it.
struct Sample {
    /// When the client received it.
    at: Instant,
    /// The receive time minus the reading's timestamp (boot clock).
    age_us: i64,
}

/// The spacing of the readings as a viewer receives them, and their ages.
fn stats(samples: &[Sample]) -> Stats {
    let times: Vec<Instant> = samples.iter().map(|s| s.at).collect();
    let deltas: Vec<i64> = times
        .windows(2)
        .map(|w| w[1].duration_since(w[0]).as_micros() as i64)
        .collect();
    let span = times
        .last()
        .zip(times.first())
        .map_or(0.0, |(l, f)| l.duration_since(*f).as_secs_f64());
    let mut devs: Vec<i64> = deltas.iter().map(|d| (d - PERIOD_US).abs()).collect();
    devs.sort_unstable();
    let mut ages: Vec<i64> = samples.iter().map(|s| s.age_us).collect();
    ages.sort_unstable();
    let p99 = |v: &[i64]| v.get((v.len() * 99) / 100).copied().unwrap_or(0);
    Stats {
        rate_hz: deltas.len() as f64 / span.max(1e-9),
        p99_dev_us: p99(&devs),
        max_dev_us: devs.last().copied().unwrap_or(0),
        p99_age_us: p99(&ages),
    }
}

/// Starts the board on mock hardware; bus 2 takes `latency` per transaction.
fn service(latency: Duration) -> MockLemnosd {
    let hardware = MockHardware::new();
    for bus in [1, 2] {
        hardware
            .i2c(bus)
            .with_target(0x18, AddressWidth::Bits8)
            .with_target(0x68, AddressWidth::Bits8)
            .with_registers(0x18, 0x00, &[0x1e])
            .with_registers(0x68, 0x00, &[0x0f]);
    }
    hardware.i2c(2).set_latency(latency);
    MockLemnosd::start(BOARD, hardware).unwrap()
}

/// Subscribes the IMU at 10 ms (and the slow device, when `load`), then
/// records when the IMU's readings arrive for 3 s.
#[allow(clippy::print_stderr)]
fn run(label: &str, load: bool) -> Stats {
    let service = service(if load {
        Duration::from_millis(3)
    } else {
        Duration::ZERO
    });
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    client.subscribe("imu", 10).unwrap();
    if load {
        client.subscribe("slow", 10).unwrap();
    }
    let warm = Instant::now() + Duration::from_millis(500);
    let end = warm + Duration::from_secs(3);
    let mut samples = Vec::new();
    while Instant::now() < end {
        if let Ok(Some(ClientEvent::Data(Update::Reading(r)))) =
            client.next_event_timeout(Duration::from_millis(50))
            && r.device == "imu"
            && Instant::now() >= warm
        {
            let age_us = boottime_us() as i64 - r.timestamp_us as i64;
            samples.push(Sample {
                at: Instant::now(),
                age_us,
            });
        }
    }
    let s = stats(&samples);
    eprintln!("{label}: {s:?}");
    drop(client);
    service.stop();
    s
}

#[test]
fn imu_holds_100_hz_on_an_idle_bus() {
    let s = run("idle", false);
    assert!((s.rate_hz - 100.0).abs() < 1.0, "{s:?}");
    assert!(s.p99_dev_us <= 3_000, "{s:?}");
    assert!(s.max_dev_us <= 10_000, "{s:?}");
    assert!(s.p99_age_us <= 5_000, "{s:?}");
}

#[test]
fn imu_holds_100_hz_beside_a_slow_device() {
    let s = run("slow bus, 3 ms per transaction", true);
    assert!((s.rate_hz - 100.0).abs() < 1.0, "{s:?}");
    // The bounds allow for a busy host; a schedule that lets a 6 ms read
    // delay the IMU misses the rate (98.7 Hz) and the p99 spacing.
    assert!(s.p99_dev_us <= 3_000, "{s:?}");
    assert!(s.max_dev_us <= 10_000, "{s:?}");
    assert!(s.p99_age_us <= 5_000, "{s:?}");
}

#[test]
fn reading_timestamps_continue_across_a_restart() {
    let first = service(Duration::ZERO);
    let mut client = ClientOptions::new(first.socket(), "viewer")
        .devices()
        .unwrap();
    let before = client.read("imu").unwrap().timestamp_us;
    drop(client);
    first.stop();

    let between = boottime_us();
    let second = service(Duration::ZERO);
    let mut client = ClientOptions::new(second.socket(), "viewer")
        .devices()
        .unwrap();
    let after = client.read("imu").unwrap().timestamp_us;
    // One boot clock: the new service's stamps continue the old one's.
    assert!(after > before, "{before} then {after}");
    assert!(after >= between, "{between} then {after}");
    assert!(after <= boottime_us());
}
