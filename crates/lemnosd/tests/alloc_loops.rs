//! Allocations in lemnosd's steady state over a fake sysfs tree: an LED ring running an
//! animated status effect, a fan controller stepping its fan from a thermal zone, and that
//! zone subscribed at 50 Hz. Counted over every thread but this one (the client's decoding is
//! not the service's). Its own test binary: the count is process-wide, so no other test may
//! run beside it.

#![allow(clippy::print_stdout)]

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::SysRoot;
use lemnos_ipc::{ClientEvent, ClientOptions, LedStatus, Update};
use lemnos_linux_sys::test_alloc::{self, ALLOCATIONS, Counting, TOTAL};
use lemnosd::{Service, ServiceConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct SysBuses(PathBuf);

impl Buses for SysBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        Err(BoardError::Device {
            device: format!("i2c-{bus}"),
            kind: lemnos_hal::ErrorKind::NotFound,
            reason: "no I2C in this test".into(),
        })
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.0)
    }
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

/// A `pwm-fan` fan with its cooling device and thermal zone, and an LED ring's device file.
fn tree(root: &Path) {
    let _ = fs::remove_dir_all(root);
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

fn board(root: &Path) -> BoardDefinition {
    BoardDefinition::from_toml_str(&format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "alloc-loops"

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = {{ type = "cpu-thermal" }}
poll_ms = 20

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = {{ name = "pwmfan" }}
writers = ["lemnosd.fan"]

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/dev/leds0"
config = {{ count = 12, status_effect = "breathe", ok = 0x00ff00 }}

[[composites]]
id = "fan-control"
driver = "fan-controller"
config = {{ input_cpu = "cpu-thermal.temperature", output_fan = "fan", floor = 300, hysteresis = 0, slew = 50, curve_cpu = [[30000, 300], [60000, 600], [75000, 1000]] }}
"#,
        root = root.display()
    ))
    .unwrap()
}

/// Readings received, LED frames written and allocations made by every other thread over
/// `span`.
fn measure(
    client: &mut lemnos_ipc::DeviceClient,
    root: &Path,
    span: Duration,
    record: bool,
) -> (usize, usize, usize) {
    let mine = ALLOCATIONS.with(std::cell::Cell::get);
    let all = TOTAL.load(Ordering::Relaxed);
    test_alloc::set_recording(record);
    let end = Instant::now() + span;
    let (mut readings, mut frames) = (0, 0);
    let mut last = Vec::with_capacity(64);
    let mut now = Vec::with_capacity(64);
    let temp = root.join("devices/virtual/thermal/thermal_zone0/temp");
    let staged = root.join("temp.next");
    let mut step = 0u32;
    while Instant::now() < end {
        // A new temperature each time (an unchanged one is not delivered again), swinging
        // across the fan curve.
        step += 1;
        // Written whole (a rename), as sysfs is: a read never sees half a value.
        let _ = fs::write(&staged, format!("{}", 40_000 + (step % 400) * 50));
        let _ = fs::rename(&staged, &temp);
        if let Ok(Some(ClientEvent::Data(Update::Reading(_)))) =
            client.next_event_timeout(Duration::from_millis(5))
        {
            readings += 1;
        }
        // The ring's bytes changing: the effect is animating (this thread's reads are not
        // counted against the service).
        now.clear();
        if let Ok(mut f) = fs::File::open(root.join("dev/leds0")) {
            let _ = std::io::Read::read_to_end(&mut f, &mut now);
        }
        if now != last {
            frames += 1;
            std::mem::swap(&mut now, &mut last);
        }
    }
    test_alloc::set_recording(false);
    let mine = ALLOCATIONS.with(std::cell::Cell::get) - mine;
    let all = TOTAL.load(Ordering::Relaxed) - all;
    (readings, frames, all.saturating_sub(mine))
}

struct Running {
    root: PathBuf,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Running {
    fn start(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("lemnosd-alloc-loops-{name}-{}", std::process::id()));
        tree(&root);
        let socket = root.join("run/lemnosd.sock");
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (thread_stop, thread_root, thread_socket) =
            (Arc::clone(&stop), root.clone(), socket.clone());
        let handle = std::thread::Builder::new()
            .name("lemnosd".into())
            .spawn(move || {
                let config = ServiceConfig::new(board(&thread_root), thread_socket);
                let mut service = Service::new(config, Box::new(SysBuses(thread_root))).unwrap();
                ready_tx.send(()).unwrap();
                service.run(&thread_stop).unwrap();
                service.shutdown(false);
            })
            .unwrap();
        ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        Self {
            root,
            socket,
            stop,
            handle: Some(handle),
        }
    }

    /// A client subscribed to the zone at 50 Hz, with the ring breathing its OK status.
    fn clients(&self) -> (lemnos_ipc::DeviceClient, lemnos_ipc::LedClient) {
        let mut leds = ClientOptions::new(&self.socket, "status").leds().unwrap();
        leds.status(LedStatus::Ok).unwrap();
        leds.sync().unwrap();
        let mut client = ClientOptions::new(&self.socket, "viewer")
            .devices()
            .unwrap();
        client.subscribe("cpu-thermal", 20).unwrap();
        (client, leds)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// After a warm-up (the first reads, the first fan step, the effect's first frames), the
/// service's loops cost nothing from the allocator: the 50 Hz subscription's reads and
/// frames, the effect's frames to the ring, and the fan controller's steps (one a second).
#[test]
fn the_steady_state_loops_allocate_nothing() {
    let running = Running::start("steady");
    let (mut client, _leds) = running.clients();
    let _ = measure(
        &mut client,
        &running.root,
        Duration::from_millis(2_500),
        false,
    );
    let (readings, frames, allocations) =
        measure(&mut client, &running.root, Duration::from_secs(3), false);
    println!("{readings} readings, {frames} ring frames, {allocations} allocations");
    assert!(readings >= 100, "only {readings} readings");
    assert!(frames >= 10, "the effect is not animating: {frames} frames");
    assert_eq!(
        allocations, 0,
        "over {readings} readings, {frames} frames and 3 fan steps"
    );
}

#[test]
#[ignore = "diagnosis: prints where the steady-state loops allocate"]
fn where_the_loops_allocate() {
    let running = Running::start("where");
    let (mut client, _leds) = running.clients();
    let _ = measure(
        &mut client,
        &running.root,
        Duration::from_millis(2_500),
        false,
    );
    let (readings, frames, allocations) = measure(
        &mut client,
        &running.root,
        Duration::from_millis(1_500),
        true,
    );
    println!("{readings} readings, {frames} frames, {allocations} allocations elsewhere");
    let mut records = test_alloc::take_records();
    records.retain(|r| !r.starts_with("thread where_"));
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for r in &records {
        let site: String = r
            .lines()
            .filter(|l| l.contains("lemnos") && !l.contains("test_alloc"))
            .take(8)
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
}
