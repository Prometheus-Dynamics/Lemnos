//! A client that sets a fan's duty and is killed with `SIGKILL` gets its override handed back by
//! lemnosd. The client runs no code when it dies: its connection just ends, and lemnosd does the
//! rest. The fan goes back where the client found it (hwmon-fan: its `pwm1` and mode, as the
//! take-over recorded them) or to full duty (pwm-channel-fan, which has no baseline yet).
//!
//! The bound, `BOUND`: lemnosd's poll wakes on the connection's end at once. It then tells the
//! fan's driver through its lane, and the driver hands the fan back in that same call, so no tick
//! is waited for. The lane answers within the call deadline (200 ms in this harness), and the two
//! sysfs writes or the PWM configure are microseconds. `BOUND` is one second: the deadline plus
//! margin for a loaded machine. The measured time is printed for each run.
//!
//! The client is this test binary run again (see [`a_client_that_sets_a_fan_and_waits`]), so the
//! kill is a real `SIGKILL` of a real process that holds a real connection.

// The client role reports on stdout, where the parent reads it. The measured times are printed for the record.
#![allow(clippy::print_stdout)]

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::start_board;
use lemnos_client::Client;
use lemnos_contract_fan::Fan;
use lemnos_contract_fan_control::FanControl;
use lemnos_driver_fan_control::FanController;
use lemnos_driver_hwmon_fan::HwmonFan;
use lemnos_driver_pwm_channel_fan::PwmChannelFan;
use lemnos_fake_devices::FakeThermalZone;
use lemnos_host::entry;
use lemnos_sim::SimBoard;

/// How long the fan may take to be handed back after its client is killed.
const BOUND: Duration = Duration::from_secs(1);
/// How long the controller may take to hold the fan again after its override's client is killed: its step
/// comes once a second, so one step is the wait, and this is that plus margin.
const CONTROLLER_BOUND: Duration = Duration::from_secs(2);
/// The duty the client sets, per mille. On hwmon that is `pwm1` 153; on the PWM channel, 24000 ns of 40000.
const OVERRIDE: u16 = 600;
/// The client's test name, and the environment that makes this binary the client.
const CLIENT_TEST: &str = "a_client_that_sets_a_fan_and_waits";
const CLIENT_SOCKET: &str = "LEMNOS_FAN_CLIENT_SOCKET";
const CLIENT_DEVICE: &str = "LEMNOS_FAN_CLIENT_DEVICE";
/// The line the client prints once its override is in place.
const OVERRIDDEN: &str = "FAN-CLIENT OVERRIDDEN";

/// Not a test of its own: when this binary is started as the client, it sets the fan's duty and
/// waits to be killed. Run normally it does nothing.
#[test]
fn a_client_that_sets_a_fan_and_waits() {
    let (Ok(socket), Ok(device)) = (std::env::var(CLIENT_SOCKET), std::env::var(CLIENT_DEVICE))
    else {
        return;
    };
    let client = Client::connect(Path::new(&socket), "fan-client").unwrap();
    let mut fan = client.open::<dyn Fan>(&device).unwrap();
    fan.set_duty(OVERRIDE).unwrap();
    println!("{OVERRIDDEN}");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// The client process, killed on drop so a failing test leaves none behind.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts this binary as the client on `device`, and returns once it reports its override.
fn spawn_client(socket: &Path, device: &str) -> Killed {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CLIENT_TEST, "--nocapture", "--test-threads=1"])
        .env(CLIENT_SOCKET, socket)
        .env(CLIENT_DEVICE, device)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if line.contains(OVERRIDDEN) {
                let _ = tx.send(());
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(20))
        .expect("the client never set the fan");
    Killed(child)
}

/// Waits until `done` holds, and panics on `what` when it does not hold in time.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A scratch directory for one test, removed when the test starts.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fan-caller-gone-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Polls `read` until it equals `want`, for at most [`BOUND`] from `started`. Returns how long
/// that took, or the last reading if it never got there.
fn hand_back_time<T: PartialEq + std::fmt::Debug>(
    started: Instant,
    bound: Duration,
    want: &T,
    mut read: impl FnMut() -> T,
) -> Result<Duration, T> {
    loop {
        let now = read();
        if now == *want {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= bound {
            return Err(now);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_killed_client_hands_the_hwmon_fan_back_within_the_bound() {
    let dir = scratch("hwmon");
    let sys = dir.join("sys");
    let fan_dir = sys.join("class/hwmon/hwmon2");
    fs::create_dir_all(&fan_dir).unwrap();
    // The fan as the kernel left it: automatic mode (2) at pwm1 77. The take-over records these.
    fs::write(fan_dir.join("name"), "pwmfan\n").unwrap();
    fs::write(fan_dir.join("pwm1"), "77\n").unwrap();
    fs::write(fan_dir.join("pwm1_enable"), "2\n").unwrap();
    fs::write(fan_dir.join("fan1_input"), "900\n").unwrap();
    let board = format!(
        "[[devices]]\nid = \"fan\"\ndriver = \"hwmon-fan\"\n\
         config = {{ hwmon = \"hwmon2\", sys_root = \"{}\" }}\n",
        sys.display()
    );
    let running = start_board(&board, vec![entry::<HwmonFan>()], SimBoard::new(), None);
    let client = Client::connect(&running.socket, "test").unwrap();
    wait_for("the fan is bound", || client.open::<dyn Fan>("fan").is_ok());

    let read = |name: &str| {
        fs::read_to_string(fan_dir.join(name))
            .unwrap()
            .trim()
            .to_string()
    };
    let mut child = spawn_client(&running.socket, "fan");
    wait_for("the client's override is on the fan", || {
        read("pwm1") == "153"
    });
    assert_eq!(
        read("pwm1_enable"),
        "1",
        "the override puts the fan in manual mode"
    );

    let started = Instant::now();
    let _ = child.0.kill();
    let _ = child.0.wait();
    let back = hand_back_time(started, BOUND, &("77".to_string(), "2".to_string()), || {
        (read("pwm1"), read("pwm1_enable"))
    });
    match back {
        Ok(took) => println!(
            "hwmon-fan hand-back after SIGKILL: {} ms (bound {} ms)",
            took.as_millis(),
            BOUND.as_millis()
        ),
        Err(state) => panic!(
            "the fan stayed at (pwm1, pwm1_enable) = {state:?} for the {} ms bound after its client was killed",
            BOUND.as_millis()
        ),
    }
    drop(running);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_killed_client_hands_the_pwm_fan_back_within_the_bound() {
    let dir = scratch("pwm");
    let sim = SimBoard::new();
    let pwm = sim.add_pwm("pwm0");
    let board = "[[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\n\
                 uses = { pwm = \"pwm0\" }\nconfig = { frequency_hz = 25000 }\n";
    let running = start_board(board, vec![entry::<PwmChannelFan>()], sim, None);
    let client = Client::connect(&running.socket, "test").unwrap();
    wait_for("the fan starts at full duty", || {
        client.open::<dyn Fan>("fan").is_ok() && pwm.current().duty_ns == 40_000
    });

    let mut child = spawn_client(&running.socket, "fan");
    wait_for("the client's override is on the channel", || {
        pwm.current().duty_ns == 24_000
    });

    let started = Instant::now();
    let _ = child.0.kill();
    let _ = child.0.wait();
    let back = hand_back_time(started, BOUND, &40_000, || pwm.current().duty_ns);
    match back {
        Ok(took) => println!(
            "pwm-channel-fan hand-back after SIGKILL: {} ms (bound {} ms)",
            took.as_millis(),
            BOUND.as_millis()
        ),
        Err(duty_ns) => panic!(
            "the channel stayed at duty {duty_ns} ns (full is 40000) for the {} ms bound after its client was killed",
            BOUND.as_millis()
        ),
    }
    drop(running);
    let _ = fs::remove_dir_all(&dir);
}

/// The fan controller over a real hwmon fan. The controller steps once a second and writes its duty each
/// step, so a client's override lasts until the next step. If the client goes after a step, the controller
/// is the last writer and nothing is handed back. If it goes between two steps, the fan is handed back to its
/// baseline (the kernel's automatic mode) for the gap, and the controller takes the fan again at its next step:
/// the wait is one step, measured at about 1 s. The test checks the controller is back within two steps.
#[test]
fn a_killed_override_under_a_controller_leaves_the_controller_in_charge() {
    let dir = scratch("controller");
    let sys = dir.join("sys");
    let fan_dir = sys.join("class/hwmon/hwmon2");
    fs::create_dir_all(&fan_dir).unwrap();
    fs::write(fan_dir.join("name"), "pwmfan\n").unwrap();
    fs::write(fan_dir.join("pwm1"), "77\n").unwrap();
    fs::write(fan_dir.join("pwm1_enable"), "2\n").unwrap();
    fs::write(fan_dir.join("fan1_input"), "900\n").unwrap();
    let zone = dir.join("zone");
    fs::create_dir_all(&zone).unwrap();
    // 45 C on the B14 curve is 450 per mille: pwm1 115.
    fs::write(zone.join("temp"), "45000\n").unwrap();
    let board = format!(
        "[[devices]]\nid = \"fan\"\ndriver = \"hwmon-fan\"\n\
         config = {{ hwmon = \"hwmon2\", sys_root = \"{}\" }}\n\
         [[devices]]\nid = \"cpu-thermal\"\ndriver = \"fake-thermal-zone\"\n\
         config = {{ path = \"{}\" }}\n\
         [[devices]]\nid = \"fan-control\"\ndriver = \"fan-control\"\n\
         uses = {{ fan = \"fan\", t0 = \"cpu-thermal\" }}\n\
         config = {{ curve_t0 = \"30000:300,60000:600,75000:1000\", floor = 300, slew_per_s = 50, max_age_ms = 3000 }}\n",
        sys.display(),
        zone.display()
    );
    let drivers = vec![
        entry::<HwmonFan>(),
        entry::<FanController>(),
        entry::<FakeThermalZone>(),
    ];
    let running = start_board(&board, drivers, SimBoard::new(), None);
    let client = Client::connect(&running.socket, "test").unwrap();
    let mut control = client.open::<dyn FanControl>("fan-control").unwrap();

    let read = |name: &str| {
        fs::read_to_string(fan_dir.join(name))
            .unwrap()
            .trim()
            .to_string()
    };
    wait_for("the controller holds the fan at its curve", || {
        read("pwm1") == "115" && control.state().map(|s| s.duty) == Ok(450)
    });

    // Killed straight after the override: the worst case, when no controller step has come since it. The
    // fan is handed back to the kernel's baseline (automatic mode) until the controller's next step.
    let mut child = spawn_client(&running.socket, "fan");
    let started = Instant::now();
    let _ = child.0.kill();
    let _ = child.0.wait();
    let back = hand_back_time(
        started,
        CONTROLLER_BOUND,
        &("115".to_string(), "1".to_string()),
        || (read("pwm1"), read("pwm1_enable")),
    );
    match back {
        Ok(took) => println!(
            "controller back in charge after the override's client was killed: {} ms (bound {} ms)",
            took.as_millis(),
            CONTROLLER_BOUND.as_millis()
        ),
        Err(state) => panic!(
            "the controller did not take the fan back within {} ms: (pwm1, pwm1_enable) = {state:?}",
            CONTROLLER_BOUND.as_millis()
        ),
    }
    // It stays in charge: a later step keeps the curve's duty.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(read("pwm1"), "115");
    assert_eq!(control.state().unwrap().duty, 450);
    drop(running);
    let _ = fs::remove_dir_all(&dir);
}
