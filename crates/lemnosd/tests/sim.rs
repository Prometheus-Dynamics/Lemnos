//! `lemnosd --sim`, end to end: the real binary serves a simulated board over
//! its socket, and the client API reads, drives and watches it. The unit
//! tests in `src/sim/tests.rs` check the physics and the device models step by
//! step; these check what a client sees.
//!
//! Run with `cargo test -p lemnosd --features sim`.
#![cfg(feature = "sim")]

use lemnos_device::DeviceStatus;
use lemnos_ipc::{ClientEvent, ClientOptions, DeviceClient, FrameUpdate};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// The example board's devices (the layout of a sensor board with a fan, a
/// thermal zone, a status ring and two switched USB ports), plus a fusion
/// device for the orientation test.
const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "sim-test"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c:compatible=i2c-gpio"
address = 0x18
poll_ms = 10
config = { gyro_address = 0x68, accel_range = "6g", accel_rate = "400hz", gyro_range = "2000dps", gyro_rate = "400hz-47" }

[[devices]]
id = "magnetometer"
driver = "bmm150"
bus = "i2c-1"
address = 0x10
poll_ms = 100
config = { preset = "regular", data_rate = "10hz" }

[[devices]]
id = "power"
driver = "ina238"
bus = "i2c-1"
address = 0x40
poll_ms = 100
config = { shunt_micro_ohms = 10000, max_current_micro_amps = 5000000 }

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = { name = "pwmfan" }
poll_ms = 1000
writers = ["lemnosd.fan", "app"]

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = { type = "cpu-thermal" }
poll_ms = 1000

[[devices]]
id = "status-ring"
driver = "ws2812"
path = "/dev/leds0"
config = { count = 16, wire = "rgb", offset = 5, direction = "cw", gpio = 13, fade_ms = 250, easing = "ease-in-out", status_effect = "breathe" }

[[devices]]
id = "usb-a-power"
driver = "gpio-power-switch"
writers = ["orion:*", "atlas"]
config = { chip = "pinctrl-rp1", line = 20, default_on = true, persist = true, on_exit = "keep" }

[[devices]]
id = "usb-c-power"
driver = "gpio-power-switch"
writers = ["orion:*", "atlas"]
config = { chip = "pinctrl-rp1", line = 16, default_on = true, persist = true, on_exit = "keep" }

[[devices]]
id = "orientation"
driver = "fusion"
config = { imu = "imu", mag = "magnetometer", mode = "9axis", always = true }
"#;

/// A running `lemnosd --sim` on its own socket and sim root.
struct Sim {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
}

impl Sim {
    /// Starts the service on `script` (TOML) and connects a device client to it.
    fn start(name: &str, script: &str) -> (Self, DeviceClient) {
        let dir =
            std::env::temp_dir().join(format!("lemnosd-sim-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let board = dir.join("board.toml");
        fs::write(&board, BOARD).unwrap();
        let script_path = dir.join("sim.toml");
        fs::write(&script_path, script).unwrap();
        let socket = dir.join("s.sock");
        let log = fs::File::create(dir.join("lemnosd.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_lemnosd"))
            .arg("--sim")
            .arg("--board")
            .arg(&board)
            .arg("--socket")
            .arg(&socket)
            .arg("--sim-script")
            .arg(&script_path)
            .arg("--sim-root")
            .arg(dir.join("root"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let sim = Self { child, dir, socket };
        // The client's name is one the board's writers admit (the fan's).
        let client = match ClientOptions::new(&sim.socket, "app")
            .wait(Duration::from_secs(20))
            .devices()
        {
            Ok(client) => client,
            Err(error) => {
                sim.log_and_fail(&format!("connect: {error}"));
            }
        };
        (sim, client)
    }

    fn log_and_fail(&self, why: &str) -> ! {
        let log = fs::read_to_string(self.dir.join("lemnosd.log")).unwrap_or_default();
        panic!("{why}\nlemnosd log:\n{log}");
    }

    fn sim_root(&self) -> PathBuf {
        self.dir.join("root")
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// `actual` within `tol` of `expected`, with a message naming both.
fn near(what: &str, actual: Option<f64>, expected: f64, tol: f64) {
    let actual = actual.unwrap_or_else(|| panic!("{what}: no value"));
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: {actual} is not {expected} (± {tol})"
    );
}

/// Reads `device`'s `channel` until `done` accepts its value (the service
/// reads each device on its own poll, so a value can lag the first read).
fn wait_for_value(
    client: &mut DeviceClient,
    device: &str,
    channel: &str,
    timeout: Duration,
    done: impl Fn(f64) -> bool,
) -> Option<f64> {
    let deadline = Instant::now() + timeout;
    loop {
        let value = client.read(device).ok().and_then(|r| r.value(channel));
        if value.is_some_and(&done) || Instant::now() >= deadline {
            return value;
        }
        sleep(Duration::from_millis(100));
    }
}

/// Reads `device` until `done` accepts its status (or a read fails), for at
/// most `timeout`. Returns the last status seen (None: the read was refused).
fn wait_for_status(
    client: &mut DeviceClient,
    device: &str,
    timeout: Duration,
    done: impl Fn(Option<DeviceStatus>) -> bool,
) -> Option<DeviceStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = client.read(device).ok().map(|r| r.status);
        if done(status) || Instant::now() >= deadline {
            return status;
        }
        sleep(Duration::from_millis(100));
    }
}

#[test]
fn serves_the_simulated_readings_over_the_socket() {
    let (sim, mut client) = Sim::start("readings", "format = \"lemnos.sim\"\nschema_version = 1\n");
    let names: Vec<String> = client.list().unwrap().into_iter().map(|d| d.id).collect();
    for id in [
        "imu",
        "magnetometer",
        "power",
        "fan",
        "cpu-thermal",
        "status-ring",
        "usb-a-power",
        "orientation",
    ] {
        assert!(
            names.iter().any(|n| n == id),
            "{id} is not listed: {names:?}"
        );
    }

    // A still board: gravity on +z, the Earth field at (20, 0, -40) µT, and the
    // two switched ports on (the default level): 0.35 A + 2 × 0.45 A.
    let imu = client.read("imu").unwrap();
    assert_eq!(imu.status, DeviceStatus::Available);
    near("acceleration.z", imu.value("acceleration.z"), 9.806, 0.15);
    near("acceleration.x", imu.value("acceleration.x"), 0.0, 0.15);
    near("angular_rate.z", imu.value("angular_rate.z"), 0.0, 0.01);

    let mag = client.read("magnetometer").unwrap();
    near(
        "magnetic_field.x",
        mag.value("magnetic_field.x"),
        20e-6,
        2e-6,
    );
    near(
        "magnetic_field.z",
        mag.value("magnetic_field.z"),
        -40e-6,
        2e-6,
    );

    // The ports switch on at start: wait for the supply to show their load.
    let current = wait_for_value(
        &mut client,
        "power",
        "current",
        Duration::from_secs(5),
        |a| a > 1.0,
    );
    near("current", current, 1.25, 0.03);
    let power = client.read("power").unwrap();
    near(
        "bus_voltage",
        power.value("bus_voltage"),
        5.1 - 0.04 * 1.25,
        0.03,
    );

    let thermal = client.read("cpu-thermal").unwrap();
    let t = thermal.value("temperature").unwrap();
    assert!((33.0..40.0).contains(&t), "temperature {t}");
    drop(sim);
}

#[test]
fn fusion_converges_to_the_scripted_attitude() {
    // Still for half a second, then a 20° roll and -10° pitch, held. Two
    // simulated seconds per real second.
    let script = "format = \"lemnos.sim\"\nschema_version = 1\nspeed = 2.0\n\
        [[motion]]\nkind = \"still\"\nseconds = 0.5\n\
        [[motion]]\nkind = \"attitude\"\nseconds = 1.0\nroll_deg = 20.0\npitch_deg = -10.0\nyaw_deg = 0.0\n";
    let (sim, mut client) = Sim::start("fusion", script);
    // The filter needs a few seconds of simulated time to settle.
    sleep(Duration::from_secs(4));
    let orientation = client.read("orientation").unwrap();
    let deg = |v: Option<f64>| v.map(f64::to_degrees);
    near("roll (deg)", deg(orientation.value("roll")), 20.0, 2.0);
    near("pitch (deg)", deg(orientation.value("pitch")), -10.0, 2.0);
    drop(sim);
}

#[test]
fn the_fan_follows_its_duty_with_lag() {
    let (sim, mut client) = Sim::start(
        "fan",
        "format = \"lemnos.sim\"\nschema_version = 1\nspeed = 2.0\n",
    );
    client.set("fan", "duty", 0.8).unwrap();
    // Right after the write the rotor has barely moved from its 50 % speed.
    let at_once = client.read("fan").unwrap();
    let speed = at_once.value("speed").unwrap();
    assert!(speed < 2900.0, "the rotor jumped to {speed} rpm at once");
    // Several time constants later (0.8 s each, two per real second), it is
    // at 80 % of 4200 rpm.
    sleep(Duration::from_secs(4));
    let settled = client.read("fan").unwrap();
    near(
        "fan speed (rpm)",
        settled.value("speed"),
        0.8 * 4200.0,
        120.0,
    );
    near("fan duty", settled.value("duty"), 0.8, 0.01);
    drop(sim);
}

#[test]
fn an_unresponsive_device_shows_as_a_fault_then_recovers() {
    // The magnetometer stops answering from 1 s to 3 s of simulated time.
    let script = "format = \"lemnos.sim\"\nschema_version = 1\n\
        [[fault]]\ndevice = \"magnetometer\"\nkind = \"unresponsive\"\nat = 1.0\nseconds = 2.0\n";
    let (sim, mut client) = Sim::start("fault", script);
    let mut seen_fault = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let status = client.read("magnetometer").ok().map(|r| r.status);
        if status != Some(DeviceStatus::Available) {
            seen_fault = Some(status);
            break;
        }
        sleep(Duration::from_millis(100));
    }
    let fault = seen_fault.unwrap_or_else(|| sim.log_and_fail("the fault never showed"));
    assert_ne!(fault, Some(DeviceStatus::Available));
    let recovered = wait_for_status(&mut client, "magnetometer", Duration::from_secs(15), |s| {
        s == Some(DeviceStatus::Available)
    });
    assert_eq!(
        recovered,
        Some(DeviceStatus::Available),
        "the device did not recover"
    );
    drop(sim);
}

#[test]
fn led_frames_are_visible_through_the_frame_watch() {
    let (sim, _client) = Sim::start("leds", "format = \"lemnos.sim\"\nschema_version = 1\n");
    let mut leds = ClientOptions::new(&sim.socket, "app").leds().unwrap();
    leds.color(0x00ff00).unwrap();
    leds.watch_frames("status-ring", 20).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut green = 0;
    while Instant::now() < deadline && green == 0 {
        if let Ok(Some(ClientEvent::Data(FrameUpdate::Frame(frame)))) =
            leds.next_frame(Some(Duration::from_millis(500)))
        {
            // 0xWWRRGGBB per logical LED: count the green ones (the ring's
            // default brightness scales the colour, so the test looks for green
            // dominance, not 0xff).
            green = frame
                .pixels
                .iter()
                .filter(|p| {
                    let (r, g, b) = ((**p >> 16) & 0xff, (**p >> 8) & 0xff, **p & 0xff);
                    g >= 0x40 && r < 0x10 && b < 0x10
                })
                .count();
        }
    }
    assert!(green >= 8, "the watched frames show {green} green LEDs");
    // The strip's frames are also a file in the sim root.
    let file = sim.sim_root().join("dev").join("leds0");
    assert!(Path::new(&file).is_file());
    drop(sim);
}
