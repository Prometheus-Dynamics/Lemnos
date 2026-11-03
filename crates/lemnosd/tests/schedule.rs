//! The read scheduler and the reading timestamps, over the real service with
//! mock buses. The slow bus takes a fixed time per transaction
//! (`MockI2c::set_latency`), so its device's reads take real time.

use lemnos_hal::mock::FifoLength;
use lemnos_hal::register::AddressWidth;
use lemnos_ipc::{ClientEvent, ClientOptions, Update};
use lemnos_linux_sys::time::boottime_us;
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The timing tests below measure the grid on real threads. Run beside each
/// other (the harness runs a binary's tests in parallel), four services on one
/// host push each other's reads late, and the grid deviation then measures the
/// host's load rather than the schedule. They take this lock and run one at a
/// time; the bounds are not loosened.
static TIMING: Mutex<()> = Mutex::new(());

fn timing_turn() -> MutexGuard<'static, ()> {
    TIMING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A plain thread that sleeps on the same 10 ms cadence as the IMU and stamps
/// each wakeup on the boot clock, while the test runs. Its p99 deviation is how
/// late this host wakes a thread: a host that delays every wakeup delays the
/// reads too, so the IMU's bounds are judged against it (relative bounds).
/// Returns the p99 deviation in microseconds once `stop` is set.
fn host_wakeups(stop: Arc<AtomicBool>) -> JoinHandle<i64> {
    std::thread::spawn(move || {
        let mut stamps = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_micros(PERIOD_US as u64));
            stamps.push(boottime_us() as i64);
        }
        let mut devs: Vec<i64> = stamps
            .windows(2)
            .map(|w| ((w[1] - w[0]) - PERIOD_US).abs())
            .collect();
        devs.sort_unstable();
        devs.get((devs.len() * 99) / 100).copied().unwrap_or(0)
    })
}

/// The rate floor for a nominal rate, tied to the host's wakeup p99 (as the
/// other bounds in this file are): a read that runs late past its sample's
/// margin loses the sample, and the p99 says how late reads run, so that
/// share of the nominal rate is allowed (at most half of it).
fn rate_floor(nominal_hz: f64, period_us: i64, host_p99_us: i64) -> f64 {
    let share = (host_p99_us as f64 / period_us as f64).min(0.5);
    nominal_hz * (1.0 - share)
}

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
config = { fifo = false }

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 10
config = { fifo = false }
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
    /// This host's p99 wakeup deviation during the run (`host_wakeups`).
    host_p99_us: i64,
}

/// The IMU's p99 spacing bound, in microseconds: `P99_DEV_US` plus
/// `HOST_FACTOR` times what this host adds to a sleeper's p99 wakeup in the
/// same run. On a quiet host the bound is about 5 ms. Under load the service's
/// own wakeups run late too: measured on a host with a load average near 100
/// on 32 cores, the service's p99 was 4 to 6 times the sleeper's p99.
const P99_DEV_US: i64 = 5_000;
const HOST_FACTOR: i64 = 8;

/// One reading as the client saw it.
struct Sample {
    /// The reading's timestamp (boot clock, microseconds): where it sits on
    /// the scheduler's grid. Arrival times depend on the host's load; the grid
    /// does not.
    at_us: i64,
    /// The receive time minus the reading's timestamp (boot clock).
    age_us: i64,
}

/// The spacing of the readings as a viewer receives them, and their ages.
fn stats(samples: &[Sample], host_p99_us: i64) -> Stats {
    let times: Vec<i64> = samples.iter().map(|s| s.at_us).collect();
    let deltas: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let span = times
        .last()
        .zip(times.first())
        .map_or(0.0, |(l, f)| (l - f) as f64 / 1e6);
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
        host_p99_us,
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
            .with_registers(0x68, 0x00, &[0x0f])
            // The accelerometer makes a sample every 10 ms (100 Hz), with its
            // data-ready bit: a polled read between samples has nothing new.
            .with_sampler(0x18, 0x12, Duration::from_millis(10));
    }
    hardware.i2c(2).set_latency(latency);
    MockLemnosd::start(BOARD, hardware).unwrap()
}

/// Subscribes the IMU at 10 ms (and the slow device, when `load`), then
/// records when the IMU's readings arrive for 3 s.
#[allow(clippy::print_stderr)]
fn run(label: &str, load: bool) -> Stats {
    run_with(
        label,
        if load {
            Duration::from_millis(3)
        } else {
            Duration::ZERO
        },
        load,
    )
}

/// `run`, with bus 2 taking `latency` per transaction.
#[allow(clippy::print_stderr)]
fn run_with(label: &str, latency: Duration, load: bool) -> Stats {
    let _turn = timing_turn();
    let service = service(latency);
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    client.subscribe("imu", 10).unwrap();
    if load {
        client.subscribe("slow", 10).unwrap();
    }
    // The samples fall half-way between the reads (see `restart_sampler`).
    for bus in [1, 2] {
        service
            .hardware()
            .i2c(bus)
            .restart_sampler(0x18, Duration::from_millis(5));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let host = host_wakeups(stop.clone());
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
                at_us: r.timestamp_us as i64,
                age_us,
            });
        }
    }
    stop.store(true, Ordering::Relaxed);
    let host_p99 = host.join().unwrap();
    let s = stats(&samples, host_p99);
    eprintln!("{label}: {s:?}");
    drop(client);
    service.stop();
    s
}

#[test]
fn imu_holds_100_hz_on_an_idle_bus() {
    let s = run("idle", false);
    assert!(
        s.rate_hz >= rate_floor(100.0, PERIOD_US, s.host_p99_us) && s.rate_hz <= 103.0,
        "{s:?}"
    );
    assert!(
        s.p99_dev_us <= P99_DEV_US + HOST_FACTOR * s.host_p99_us,
        "{s:?}"
    );
    assert!(s.max_dev_us <= 25_000, "{s:?}");
    assert!(s.p99_age_us <= 25_000, "{s:?}");
}

#[test]
fn imu_holds_100_hz_beside_a_slow_device() {
    let s = run("slow bus, 3 ms per transaction", true);
    assert!(
        s.rate_hz >= rate_floor(100.0, PERIOD_US, s.host_p99_us) && s.rate_hz <= 103.0,
        "{s:?}"
    );
    // The bounds allow for a busy host (the grid is checked, not arrival times); a schedule that lets a 6 ms read
    // delay the IMU misses the rate (98.7 Hz) and the p99 spacing.
    assert!(
        s.p99_dev_us <= P99_DEV_US + HOST_FACTOR * s.host_p99_us,
        "{s:?}"
    );
    assert!(s.max_dev_us <= 25_000, "{s:?}");
    assert!(s.p99_age_us <= 25_000, "{s:?}");
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

#[test]
fn imu_holds_100_hz_beside_a_read_longer_than_its_period() {
    // Each read on bus 2 takes 2 x 10 ms, longer than the IMU's 10 ms period.
    // The bus threads keep the IMU's bus free, so the IMU is not held back.
    let s = run_with(
        "slow bus, 10 ms per transaction",
        Duration::from_millis(10),
        true,
    );
    assert!(
        s.rate_hz >= rate_floor(100.0, PERIOD_US, s.host_p99_us) && s.rate_hz <= 103.0,
        "{s:?}"
    );
    assert!(
        s.p99_dev_us <= P99_DEV_US + HOST_FACTOR * s.host_p99_us,
        "{s:?}"
    );
    assert!(s.max_dev_us <= 25_000, "{s:?}");
}

/// The IMU alone, with its FIFOs. Each mock FIFO fills on the host's clock at
/// 400 Hz (one accelerometer and one gyroscope frame every 2.5 ms, from when
/// the bus is built), and a read takes what has arrived since the last one.
const FIFO_BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "fifo"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 10
config = { fifo = true, accel_rate = "400hz", gyro_rate = "400hz-47" }
"#;

/// One 400 Hz frame: 2.5 ms apart.
const FRAME_PERIOD: Duration = Duration::from_micros(2_500);
const ACCEL_FRAME: [u8; 7] = [0x84, 0, 0, 0, 0, 0, 0];
const GYRO_FRAME: [u8; 6] = [0; 6];

fn fifo_service() -> MockLemnosd {
    let hardware = MockHardware::new();
    hardware
        .i2c(1)
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f])
        .with_fifo_stream(
            0x18,
            0x26,
            FifoLength::Bytes(0x24),
            FRAME_PERIOD,
            &ACCEL_FRAME,
        )
        .with_fifo_stream(
            0x68,
            0x3f,
            FifoLength::Frames {
                register: 0x0e,
                frame: GYRO_FRAME.len(),
            },
            FRAME_PERIOD,
            &GYRO_FRAME,
        );
    MockLemnosd::start(FIFO_BOARD, hardware).unwrap()
}

/// The largest gap a batched IMU may show between two samples. A late drain
/// shows as a gap: the reads run every 10 ms, so a gap is one missed read
/// (20 ms) plus the host's scheduling slack. Fixed, not scaled by the host's
/// wakeups: the FIFO's frames arrive on their own clock, so a gap is the
/// service's to explain.
const MAX_GAP_US: i64 = 25_000;

#[test]
fn a_batched_imu_delivers_every_sample_on_its_own_timestamp() {
    let _turn = timing_turn();
    let service = fifo_service();
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    // 2 ms asks for every sample of the 400 Hz stream.
    client.subscribe("imu", 2).unwrap();
    // The samples fall half-way between the reads (see `restart_sampler`).
    service
        .hardware()
        .i2c(1)
        .restart_sampler(0x18, FRAME_PERIOD / 2);
    let warm = Instant::now() + Duration::from_millis(500);
    let end = warm + Duration::from_secs(3);
    let mut stamps: Vec<i64> = Vec::new();
    while Instant::now() < end {
        if let Ok(Some(ClientEvent::Data(Update::Reading(r)))) =
            client.next_event_timeout(Duration::from_millis(50))
            && r.device == "imu"
            && Instant::now() >= warm
        {
            stamps.push(r.timestamp_us as i64);
        }
    }
    let left = service.hardware().i2c(1).fifo_bytes(0x18);
    service.stop();
    assert!(stamps.len() > 100, "{}", stamps.len());
    let deltas: Vec<i64> = stamps.windows(2).map(|w| w[1] - w[0]).collect();
    let span = (stamps[stamps.len() - 1] - stamps[0]) as f64 / 1e6;
    let rate = (stamps.len() - 1) as f64 / span;
    assert!((300.0..=420.0).contains(&rate), "{rate} Hz");
    // Every read takes what arrived: no sample is replayed, so the stamps
    // strictly increase.
    assert!(
        deltas.iter().all(|d| *d > 0),
        "stamps do not increase: {:?}",
        deltas.iter().filter(|d| **d <= 0).collect::<Vec<_>>()
    );
    // The median is the stream's spacing, and the FIFO is drained: a read
    // leaves at most a read's worth behind (a few frames, 7 bytes each).
    let mut sorted = deltas.clone();
    sorted.sort_unstable();
    assert_eq!(sorted[sorted.len() / 2], 2_500, "median spacing");
    assert!(left < 10 * 7, "{left} bytes left in the FIFO");
    let max = sorted[sorted.len() - 1];
    assert!(max <= MAX_GAP_US, "largest gap {max} us");
}

#[test]
fn read_timing_is_reported_per_sensor() {
    let _turn = timing_turn();
    let service = service(Duration::from_millis(1));
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    client.subscribe("imu", 10).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let stats = client.stats().unwrap();
    service.stop();
    let imu = stats.iter().find(|s| s.device == "imu").expect("imu");
    assert!(imu.reads >= 10, "{imu:?}");
    assert!(imu.last_us <= imu.max_us, "{imu:?}");
    assert!(imu.total_us >= imu.max_us, "{imu:?}");
}

/// The IMU polled at 2 ms with a 400 Hz output: the device makes a sample every
/// 2.5 ms, so the scheduler reads it at that period.
const POLLED_400_BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "polled400"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 2
config = { fifo = false, accel_rate = "400hz", gyro_rate = "400hz-47" }
"#;

/// The accelerometer's output register is a sample counter that steps every
/// 2.5 ms, with its data-ready bit (`MockI2c::with_sampler`): a read with no
/// new sample is a miss, so each reading names its own sample.
fn polled_400_service() -> MockLemnosd {
    let hardware = MockHardware::new();
    hardware
        .i2c(1)
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f])
        .with_sampler(0x18, 0x12, FRAME_PERIOD);
    MockLemnosd::start(POLLED_400_BOARD, hardware).unwrap()
}

#[test]
fn a_polled_400_hz_imu_delivers_each_sample_once() {
    let _turn = timing_turn();
    let service = polled_400_service();
    let mut client = ClientOptions::new(service.socket(), "viewer")
        .devices()
        .unwrap();
    // A 2 ms subscription, on a device that makes a sample every 2.5 ms.
    client.subscribe("imu", 2).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let host = host_wakeups(stop.clone());
    let warm = Instant::now() + Duration::from_millis(500);
    let end = warm + Duration::from_secs(3);
    let mut readings: Vec<(i64, f64)> = Vec::new();
    while Instant::now() < end {
        if let Ok(Some(ClientEvent::Data(Update::Reading(r)))) =
            client.next_event_timeout(Duration::from_millis(50))
            && r.device == "imu"
            && Instant::now() >= warm
        {
            let counter = r.value("acceleration.x").unwrap_or(f64::NAN);
            readings.push((r.timestamp_us as i64, counter));
        }
    }
    stop.store(true, Ordering::Relaxed);
    let host_p99 = host.join().unwrap();
    service.stop();
    let rate = readings.len() as f64 / 3.0;
    // At most 400 a second (the sensor's rate; a 2 ms poll would deliver 500).
    // The floor is tied to the host's wakeup p99, as the other rate bounds
    // are: a read that runs late past its sample's margin loses it.
    let floor = rate_floor(400.0, 2_500, host_p99);
    assert!(
        rate >= floor && rate <= 420.0,
        "{rate} readings a second (floor {floor:.0})"
    );
    // No duplicates: a reading is a new sample, so its counter (the
    // accelerometer's X axis) differs from the last one's.
    let repeats = readings.windows(2).filter(|w| w[1].1 == w[0].1).count();
    assert_eq!(repeats, 0, "{repeats} readings repeat the last sample");
}
