//! Allocations of a whole board's service with no client: a shipped board definition
//! (`/etc/lemnos/board.toml` of its image) on mock I2C and a fake sysfs. The IMU is read every
//! 5 ms for the ring's gravity, the magnetometer and the power monitor every 100 ms, the fan
//! and the zone every second, and the ring runs its effect. Counted over every thread but this
//! one. Its own test binary: the count is process-wide.

#![allow(clippy::print_stdout)]

use lemnos_hal::register::AddressWidth;
use lemnos_ipc::ClientOptions;
use lemnos_linux_sys::test_alloc::{self, ALLOCATIONS, Counting, TOTAL};
use lemnosd::mock::{MockHardware, MockLemnosd};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The count is process-wide: the tests here run one at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// The board's pwm-fan, its cooling device and the CPU zone.
fn sys(root: &Path) {
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
    write(root, "dev/leds0", "");
}

/// The board as its image ships it, but on `i2c-1` for both buses (mock I2C) and
/// without the two GPIO power switches.
fn board(root: &Path) -> String {
    format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "camera-board"

[[devices]]
id = "status-ring"
driver = "ws2812"
path = "{root}/sys/dev/leds0"
config = {{ count = 16, wire = "rgb", offset = 5, direction = "cw", gpio = 13, fade_ms = 250, easing = "ease-in-out", status_effect = "breathe", gravity_device = "imu", gravity_plane = ["x", "y"], gravity_led0_deg = 112.5, default_down = 8 }}

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = {{ name = "pwmfan" }}
poll_ms = 1000

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = {{ type = "cpu-thermal" }}
poll_ms = 1000

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
poll_ms = 5
config = {{ accel_rate = "200hz", gyro_rate = "200hz-23", gyro_address = 0x68 }}

[[devices]]
id = "magnetometer"
driver = "bmm150"
bus = "i2c-1"
address = 0x10
poll_ms = 100

[[devices]]
id = "power"
driver = "ina238"
bus = "i2c-1"
address = 0x40
poll_ms = 100
config = {{ shunt_micro_ohms = 10000, max_current_micro_amps = 16384000 }}

[[devices]]
id = "orientation"
driver = "fusion"
config = {{ imu = "imu", mag = "magnetometer", mode = "9axis", algorithm = "mahony" }}
"#,
        root = root.display()
    )
}

fn service(name: &str) -> MockLemnosd {
    let root =
        std::env::temp_dir().join(format!("lemnosd-alloc-board-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    sys(&root.join("sys"));
    let hardware = MockHardware::new();
    hardware
        .i2c(1)
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_target(0x10, AddressWidth::Bits8)
        .with_target(0x40, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f])
        .with_registers(0x10, 0x40, &[0x32])
        .with_registers(0x40, 0x3e, &[0x54, 0x49])
        .with_registers(0x40, 0x3f, &[0x23, 0x81])
        .with_sampler(0x18, 0x12, Duration::from_millis(5))
        .set_logging(false);
    let board = board(&root);
    MockLemnosd::start_in(root, &board, hardware).unwrap()
}

/// A board agent's clients, as a live-data agent runs them when its streams are sampled: the
/// IMU, the magnetometer and the orientation subscribed at 10 ms, the other sensors at 100 ms,
/// and the ring's frames watched at 30 fps. Each runs on its own thread, left out of
/// the process-wide count, so [`measure`] counts only the service's allocations.
struct Agent {
    stop: Arc<AtomicBool>,
    events: Arc<AtomicUsize>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Agent {
    fn start(service: &MockLemnosd) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        let socket = service.socket().to_path_buf();
        let (s, e, path) = (stop.clone(), events.clone(), socket.clone());
        threads.push(
            std::thread::Builder::new()
                .name("agent-devices".into())
                .spawn(move || {
                    test_alloc::leave_out_of_total();
                    let mut client = ClientOptions::new(&path, "board-agent").devices().unwrap();
                    for (device, period) in [
                        ("imu", 10),
                        ("magnetometer", 10),
                        ("orientation", 10),
                        ("power", 100),
                        ("fan", 100),
                        ("cpu-thermal", 100),
                    ] {
                        client.subscribe(device, period).unwrap();
                    }
                    while !s.load(Ordering::Relaxed) {
                        if let Ok(Some(_)) = client.next_event_timeout(Duration::from_millis(20)) {
                            e.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
                .unwrap(),
        );
        let (s, e) = (stop.clone(), events.clone());
        threads.push(
            std::thread::Builder::new()
                .name("agent-light".into())
                .spawn(move || {
                    test_alloc::leave_out_of_total();
                    let mut client = ClientOptions::new(&socket, "board-agent-light")
                        .leds()
                        .unwrap();
                    client.watch_frames("status-ring", 30).unwrap();
                    while !s.load(Ordering::Relaxed) {
                        if let Ok(Some(_)) = client.next_frame(Some(Duration::from_millis(20))) {
                            e.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
                .unwrap(),
        );
        Self {
            stop,
            events,
            threads,
        }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

/// Allocations by every other thread over `span` (the agent's threads are left out).
fn measure(span: Duration, record: bool) -> usize {
    let mine = ALLOCATIONS.with(std::cell::Cell::get);
    let all = TOTAL.load(Ordering::Relaxed);
    test_alloc::set_recording(record);
    std::thread::sleep(span);
    test_alloc::set_recording(false);
    let mine = ALLOCATIONS.with(std::cell::Cell::get) - mine;
    (TOTAL.load(Ordering::Relaxed) - all).saturating_sub(mine)
}

/// The board's service with no client allocates nothing once its sensors have been read
/// and the ring has started (mock I2C: the PIO bus that carries the IMU on the board is not
/// in it).
#[test]
fn a_whole_board_allocates_nothing_without_clients() {
    let _serial = one_at_a_time();
    let service = service("steady");
    let _ = measure(Duration::from_secs(3), false);
    let n = measure(Duration::from_secs(3), false);
    service.stop();
    assert_eq!(n, 0, "allocations in 3 s");
}

/// With a board agent's subscriptions and frame watch (fusion running at 100 Hz, frames at
/// 30 fps), the service still allocates nothing once warm.
#[test]
fn a_whole_board_allocates_nothing_while_an_agent_streams_it() {
    let _serial = one_at_a_time();
    let service = service("agent");
    let agent = Agent::start(&service);
    let _ = measure(Duration::from_secs(3), false);
    let before = agent.events.load(Ordering::Relaxed);
    let n = measure(Duration::from_secs(3), false);
    let events = agent.events.load(Ordering::Relaxed) - before;
    agent.stop();
    service.stop();
    println!("{events} readings and frames, {n} allocations");
    // About 1000 when the machine is idle; the floor only proves the agent streamed, since a
    // starved test machine delivers far fewer (110 on one CPU at nice 19).
    assert!(events >= 60, "only {events} readings and frames in 3 s");
    assert_eq!(n, 0, "allocations in 3 s over {events} readings and frames");
}

#[test]
#[ignore = "diagnosis: prints where a whole board's service allocates with no client"]
fn where_the_board_allocates() {
    let _serial = one_at_a_time();
    let service = service("where");
    let agent = std::env::var_os("WITH_AGENT").map(|_| Agent::start(&service));
    let _ = measure(Duration::from_secs(3), false);
    let n = measure(Duration::from_secs(2), false);
    println!("unrecorded: {n} allocations in 2 s");
    let n = measure(Duration::from_millis(500), true);
    let mut records = test_alloc::take_records();
    records.retain(|r| !r.starts_with("thread agent-"));
    println!("recorded: {n} allocations, {} traces", records.len());
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for r in &records {
        let site: String = r
            .lines()
            .filter(|l| (l.contains("lemnos") || l.contains(" at ")) && !l.contains("test_alloc"))
            .take(10)
            .collect::<Vec<_>>()
            .join(" | ");
        *seen
            .entry(format!("{} :: {site}", r.lines().next().unwrap_or("")))
            .or_default() += 1;
    }
    let mut sites: Vec<_> = seen.into_iter().collect();
    sites.sort_by_key(|s| std::cmp::Reverse(s.1));
    for (site, n) in sites.iter().take(12) {
        println!("{n:6}  {site}");
    }
    if let Some(agent) = agent {
        agent.stop();
    }
    service.stop();
}
