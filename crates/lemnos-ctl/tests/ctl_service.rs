//! `lemnos-ctl` against a mock `lemnosd`, end to end: the raw bus and line
//! commands, the claims that end with the process, `set` and `restore`, and
//! what a read of a faulted device says.

use lemnos_hal::raw::LineConfig;
use lemnos_ipc::{ClientOptions, DeviceClient};
use lemnosd::mock::{MockHardware, MockLemnosd};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
raw = ["selftest"]
config = { gyro_address = 0x68 }

[[devices]]
id = "usb-power"
driver = "gpio-output"
config = { chip = "pinctrl-rp1", line = 20 }

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/leds0"
config = { count = 4, fade_ms = 0 }

[[lines]]
name = "aux"
chip = "pinctrl-rp1"
line = 5
safe = "low"

[[pwms]]
name = "buzzer"
chip = 0
channel = 1
"#;

fn hardware() -> MockHardware {
    let hardware = MockHardware::new();
    hardware.label_chip("pinctrl-rp1", "gpiochip0");
    hardware.name_line("BUTTON", "gpiochip0", 9);
    let bus = hardware.i2c(1);
    // The IMU's chip ids, and an unowned EEPROM-like target at 0x50.
    let _ = bus
        .clone()
        .with_target(0x18, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_target(0x68, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x68, 0x00, &[0x0f])
        .with_target(0x50, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x50, 0x10, &[0xaa, 0xbb]);
    hardware
}

fn start() -> MockLemnosd {
    let root = std::env::temp_dir().join(format!(
        "lemnos-ctl-service-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sys")).unwrap();
    std::fs::write(root.join("leds0"), "").unwrap();
    MockLemnosd::start_in(root, BOARD, hardware()).unwrap()
}

fn client(service: &MockLemnosd, name: &str) -> DeviceClient {
    ClientOptions::new(service.socket(), name)
        .devices()
        .unwrap()
}

/// Runs `lemnos-ctl --socket <service> <args>` and returns its stdout.
fn ctl(service: &MockLemnosd, args: &[&str]) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lemnos-ctl"))
        .arg("--socket")
        .arg(service.socket())
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn lemnos_ctl_raw_commands() {
    let service = start();
    let hw = service.hardware().clone();
    assert_eq!(
        ctl(&service, &["i2c", "read", "1", "0x50", "0x10", "2"]),
        "aa bb"
    );
    ctl(&service, &["i2c", "write", "1", "0x50", "0x30", "0x07"]);
    assert_eq!(hw.i2c(1).register(0x50, 0x30), 7);
    assert_eq!(
        ctl(&service, &["i2c", "xfer", "1", "0x50", "w:30", "r:1"]),
        "07"
    );
    hw.spi(0, 0).respond(&[0x12, 0x34]);
    assert_eq!(
        ctl(&service, &["spi", "xfer", "0.0", "9f00", "--mode", "3"]),
        "12 34"
    );

    // A kept claim outlives lemnos-ctl until released by handle.
    let out = ctl(&service, &["gpio", "set", "aux", "1", "--keep"]);
    let handle = out.split_whitespace().nth(1).unwrap().to_string();
    assert_eq!(hw.line("gpiochip0", 5).level(), Some(true));
    ctl(&service, &["gpio", "release", &handle]);
    assert_eq!(
        hw.line("gpiochip0", 5).level(),
        Some(false),
        "board safe state"
    );
    // A held claim ends with the process.
    ctl(
        &service,
        &["gpio", "set", "pinctrl-rp1:8", "1", "--hold", "0"],
    );
    assert_eq!(
        hw.line("gpiochip0", 8).config(),
        LineConfig::high_impedance()
    );
    hw.line("gpiochip0", 3).drive(true);
    assert_eq!(ctl(&service, &["gpio", "get", "gpiochip0:3"]), "1");

    // `set` persists after lemnos-ctl exits; `restore` undoes it.
    ctl(&service, &["set", "ring", "brightness", "0.5"]);
    let mut watcher = client(&service, "watcher");
    assert!((watcher.get("ring", "brightness").unwrap() - 0.5).abs() < 0.01);
    ctl(&service, &["restore", "ring"]);
    assert_eq!(watcher.get("ring", "brightness").unwrap(), 1.0);

    ctl(
        &service,
        &[
            "pwm", "set", "0:2", "--period", "1000", "--duty", "500", "--hold", "0",
        ],
    );
    assert!(!hw.pwm(0, 2).current().enabled);
    assert_eq!(hw.pwm(0, 2).history()[0].duty_ns, 500);
}

#[test]
fn read_of_a_faulted_device_says_why() {
    let hardware = MockHardware::new();
    // The accelerometer answers; the gyroscope does not.
    let _ = hardware
        .i2c(2)
        .with_target(0x18, lemnos_hal::AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e]);
    let service = MockLemnosd::start(
        r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "example"
[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-2"
backend = "userspace"
"#,
        hardware,
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lemnos-ctl"))
        .arg("--socket")
        .arg(service.socket())
        .args(["read", "imu"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("gyro chip id"), "{stderr}");
}
