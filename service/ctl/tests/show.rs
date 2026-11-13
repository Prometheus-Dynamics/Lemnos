//! `lemnos-ctl show` on the simulated board: a device as a panel, from its description, and the
//! panel code holds no name of a driver or a contract.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_host::Options;
use lemnos_host::board::parse;
use lemnos_sim::{Layout, SimBoard};
use lemnosd::{Config, Service};

const DEVICES: &str = "\
[[devices]]\nid = \"power\"\ndriver = \"ina2xx\"\nbus = \"i2c-1\"\nlabel = \"Board power\"\n\
config = { chip = \"ina226\", shunt_ohms = 0.01, max_current_a = 8.192 }\n\
[[devices]]\nid = \"imu\"\ndriver = \"bmi088\"\nbus = \"i2c-1\"\n\
[[devices]]\nid = \"orientation\"\ndriver = \"orientation\"\nuses = { accel = \"imu\", gyro = \"imu\" }\n\
[[devices]]\nid = \"switch\"\ndriver = \"gpio-power-switch\"\nbus = \"gpio-power\"\n\
[[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\nbus = \"pwm-fan\"\n\
[[devices]]\nid = \"light\"\ndriver = \"ws2812\"\nbus = \"spidev0.0\"\nconfig = { count = 8 }\n\
[[devices]]\nid = \"fake-accel\"\ndriver = \"fake-accelerometer\"\n\
[[devices]]\nid = \"counter\"\ndriver = \"fake-counter\"\nconfig = { start = 41 }\n";

struct Running {
    _service: Service,
    socket: PathBuf,
}

fn start() -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-show-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let board = SimBoard::new();
    board.add_line("gpio-power");
    board.add_pwm("pwm-fan");
    board.add_spi("spidev0.0");
    let sim = board.add_i2c("i2c-1");
    sim.attach(0x40, Layout::Words);
    sim.set_word(0x40, 0xfe, &0x5449u16.to_be_bytes());
    sim.set_word(0x40, 0xff, &0x2260u16.to_be_bytes());
    sim.attach(0x18, Layout::Bytes);
    sim.set_bytes(0x18, 0x00, &[0x1e]);
    sim.attach(0x68, Layout::Bytes);
    sim.set_bytes(0x68, 0x00, &[0x0f]);
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{DEVICES}");
    let config = Config {
        socket: socket.clone(),
        name: "show-test".to_string(),
        board: parse(&text).unwrap(),
        drivers: {
            let mut drivers = lemnosd::builtin::drivers();
            drivers.push(lemnos_host::entry::<lemnos_fake_devices::FakeAccelerometer>());
            drivers.push(lemnos_host::entry::<lemnos_fake_devices::FakeCounter>());
            drivers
        },
        backend: Box::new(board),
        options: Options {
            call_deadline: Duration::from_millis(200),
            ..Options::default()
        },
    };
    let service = Service::start(config).unwrap();
    Running {
        _service: service,
        socket,
    }
}

/// The panel of `device`, asking again while the device is busy.
fn show(running: &Running, device: &str) -> Result<String, String> {
    let args = vec![
        "--socket".to_string(),
        running.socket.display().to_string(),
        "show".to_string(),
        device.to_string(),
    ];
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut out = Vec::new();
        match lemnos_ctl::run(&args, &mut out) {
            Err(error) if error.starts_with("refused (-16)") && Instant::now() < deadline => {
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
            Ok(()) => return Ok(String::from_utf8(out).unwrap()),
        }
    }
}

#[test]
fn a_device_prints_its_name_kind_bus_address_state_and_each_value() {
    let running = start();
    let panel = show(&running, "power").unwrap();
    let first = panel.lines().next().unwrap();
    assert_eq!(first, "Board power");
    assert!(panel.contains("  kind     power\n"), "{panel}");
    assert!(panel.contains("  bus      i2c-1\n"), "{panel}");
    assert!(panel.contains("  address  0x40\n"), "{panel}");
    // Running, with the driver's own note on its health as the reason (the chip is not verified).
    assert!(panel.contains("  state    running ("), "{panel}");
    // The INA226's bus span is 32767 counts of 1.25 mV.
    assert!(
        panel.contains(
            "  bus_voltage  unit V  range 0..40.95875  step 0.00125  limit none  type number"
        ),
        "{panel}"
    );
}

#[test]
fn a_device_without_a_label_is_named_by_its_id_and_an_unknown_figure_is_said_so() {
    let running = start();
    let panel = show(&running, "orientation").unwrap();
    assert!(panel.starts_with("orientation\n"), "{panel}");
    assert!(panel.contains("  bus      none\n"), "{panel}");
    assert!(panel.contains("  address  unknown\n"), "{panel}");
    assert!(panel.contains("group attitude x"), "{panel}");
}

#[test]
fn the_panel_code_holds_no_name_of_a_driver_or_a_contract() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/show.rs")).unwrap();
    for name in [
        "ina2xx",
        "bmi088",
        "bmm150",
        "orientation",
        "hwmon-fan",
        "pwm-channel-fan",
        "fan-control",
        "ws2812",
        "ov9782",
        "accelerometer",
        "gyroscope",
        "magnetometer",
        "temperature/",
        "power-monitor",
        "power-switch",
        "camera.",
        "system.identity",
        "system.slots",
    ] {
        assert!(!source.contains(name), "src/show.rs names {name:?}");
    }
}

/// Every device of the spread, as panels, written to the file `LEMNOS_SHOW_OUT` names when it is
/// set (the output for a change's record). Each panel says its kind and state.
#[test]
fn the_spread_of_devices_prints_a_panel_for_each() {
    let running = start();
    let mut all = String::new();
    for device in [
        "power",
        "imu",
        "orientation",
        "switch",
        "fan",
        "light",
        "fake-accel",
        "counter",
    ] {
        let panel = show(&running, device).unwrap();
        assert!(panel.contains("\n  kind     "), "{device}: {panel}");
        assert!(panel.contains("\n  state    running"), "{device}: {panel}");
        all.push_str(&panel);
        all.push('\n');
    }
    if let Ok(path) = std::env::var("LEMNOS_SHOW_OUT") {
        std::fs::write(path, all).unwrap();
    }
}
