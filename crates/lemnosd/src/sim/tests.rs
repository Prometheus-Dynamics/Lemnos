//! The simulated board's physics and devices, driven step by step in simulated
//! time (no threads): what the chips' registers say, how the faults, the
//! loads, the fan and the scripted lines behave. The end-to-end checks
//! (readings over the socket, fusion, the fan through the service) are in
//! `tests/sim.rs`.

use super::bus::SimBus;
use super::world::{GRAVITY, Quat};
use super::*;
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::raw::{LineConfig, RawLine};
use std::path::{Path, PathBuf};

/// The example board's layout: the IMU on the adapter a selector names (the
/// first adapter after the board's numbered buses, 16 here), the magnetometer
/// and the power monitor on bus 1.
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
config = { gyro_address = 0x68, accel_range = "6g", accel_rate = "400hz", gyro_range = "2000dps", gyro_rate = "400hz-47" }

[[devices]]
id = "magnetometer"
driver = "bmm150"
bus = "i2c-1"
address = 0x10
config = { preset = "regular", data_rate = "10hz" }

[[devices]]
id = "power"
driver = "ina238"
bus = "i2c-1"
address = 0x40
config = { shunt_micro_ohms = 10000, max_current_micro_amps = 5000000 }

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = { name = "pwmfan" }
writers = ["lemnosd.fan", "app"]

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = { type = "cpu-thermal" }

[[devices]]
id = "status-ring"
driver = "ws2812"
path = "/dev/leds0"
config = { count = 16, wire = "rgb", offset = 5, direction = "cw", gpio = 13 }

[[devices]]
id = "usb-a-power"
driver = "gpio-power-switch"
config = { chip = "pinctrl-rp1", line = 20, default_on = true, persist = true, on_exit = "keep" }
"#;

const IMU_BUS: u32 = 16;
const MAG_BUS: u32 = 1;

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lemnosd-sim-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    root
}

fn script(extra: &str) -> Script {
    Script::from_toml_str(&format!(
        "format = \"lemnos.sim\"\nschema_version = 1\n{extra}"
    ))
    .unwrap()
}

fn build_with(name: &str, extra: &str) -> Built {
    let mut definition = BoardDefinition::from_toml_str(BOARD).unwrap();
    build(&mut definition, &script(extra), &root(name)).unwrap()
}

fn le16(bus: &SimBus, address: u8, register: u8) -> i16 {
    i16::from_le_bytes([bus.byte(address, register), bus.byte(address, register + 1)])
}

/// A big-endian word register (the power monitor's).
fn be16(bus: &SimBus, address: u8, register: u8) -> u16 {
    let w = bus.word(address, register);
    u16::from_be_bytes([w[0], w[1]])
}

/// The accelerometer's counts in m/s² at the reset range (±6 g).
fn accel_mps2(bus: &SimBus) -> [f64; 3] {
    let lsb = 6.0 * GRAVITY / 32768.0;
    [0, 2, 4].map(|o| f64::from(le16(bus, 0x18, 0x12 + o)) * lsb)
}

/// The magnetometer's field in µT, decoded as the driver does.
fn field_ut(bus: &SimBus) -> [f64; 3] {
    let x = (le16(bus, 0x10, 0x42) >> 3) as f64;
    let y = (le16(bus, 0x10, 0x44) >> 3) as f64;
    let z = (le16(bus, 0x10, 0x46) >> 1) as f64;
    [x * 5.0 / 16.0, y * 5.0 / 16.0, z * 8.0 / 16.0]
}

fn close(a: [f64; 3], b: [f64; 3], tol: f64) {
    for i in 0..3 {
        assert!(
            (a[i] - b[i]).abs() <= tol,
            "{a:?} != {b:?} (tolerance {tol})"
        );
    }
}

#[test]
fn still_board_reads_gravity_and_the_earth_field() {
    let mut built = build_with("still", "");
    built.plant.step(0.5);
    let imu = built.i2c[&IMU_BUS].clone();
    // Gravity on +z, within the noise (0.02 m/s²) and a count.
    close(accel_mps2(&imu), [0.0, 0.0, GRAVITY], 0.2);
    let mag = built.i2c[&MAG_BUS].clone();
    close(field_ut(&mag), [20.0, 0.0, -40.0], 1.5);
    // The chip IDs the drivers verify.
    assert_eq!(imu.byte(0x18, 0x00), 0x1e);
    assert_eq!(imu.byte(0x68, 0x00), 0x0f);
    assert_eq!(mag.byte(0x10, 0x40), 0x32);
}

#[test]
fn rotation_turns_gravity_and_the_field_together() {
    // A 90° roll: body +y points at world +z, and gravity's specific force
    // reads on the body's -y axis.
    let mut built = build_with(
        "roll",
        "[imu]\naccel_noise = 0.0\n[mag]\nnoise_ut = 0.0\n[[motion]]\nkind = \"attitude\"\nseconds = 0.5\nroll_deg = 90.0\npitch_deg = 0.0\nyaw_deg = 0.0\n",
    );
    // In steps, as the plant runs: the gyroscope reads the rate over a step.
    let mut t = 0.0;
    while t < 1.0 {
        t += 0.01;
        built.plant.step(t);
    }
    let q = Quat::from_euler_deg(90.0, 0.0, 0.0);
    let gravity = q.rotate_inverse([0.0, 0.0, GRAVITY]);
    let imu = built.i2c[&IMU_BUS].clone();
    close(accel_mps2(&imu), gravity, 0.01);
    let field = q.rotate_inverse([20.0, 0.0, -40.0]);
    close(field_ut(&built.i2c[&MAG_BUS].clone()), field, 0.3);
    // Gyroscope: the roll has ended, so the rate is zero.
    let gyro = [0, 2, 4].map(|o| f64::from(le16(&imu, 0x68, 0x02 + o)));
    for g in gyro {
        assert!(g.abs() < 20.0, "{g}");
    }
}

#[test]
fn gyroscope_reads_a_commanded_rotation_in_its_range() {
    let mut built = build_with(
        "spin",
        "[imu]\ngyro_noise = 0.0\n[[motion]]\nkind = \"rotate\"\nseconds = 5.0\nrate_dps = [0.0, 0.0, 100.0]\n",
    );
    built.plant.step(0.5);
    let imu = built.i2c[&IMU_BUS].clone();
    // 100 °/s on the reset ±2000 °/s range: 100/2000 × 32768 counts.
    let z = f64::from(le16(&imu, 0x68, 0x06));
    assert!((z - 1638.4).abs() < 2.0, "{z}");
}

#[test]
fn supply_current_follows_the_switched_ports() {
    let mut built = build_with("ports", "[power]\nnoise_a = 0.0\n");
    // The driver's calibration for 5 A full scale on 10 mΩ, normal range.
    let lsb = 5.0 / 32768.0;
    let cal = (819.2e6 * lsb * 0.01_f64).round() as u16;
    let power = built.i2c[&MAG_BUS].clone();
    power.set_word(0x40, 0x02, &cal.to_be_bytes());
    // Port 20 is an output the service drives: on at the default level.
    let mut port = built.hardware.line("pinctrl-rp1", 20);
    port.configure(&LineConfig::output(true)).unwrap();
    built.plant.step(0.5);
    let current = |bus: &SimBus| f64::from(be16(bus, 0x40, 0x07) as i16) * lsb;
    let on = current(&power);
    assert!((on - 0.80).abs() < 0.01, "{on}");
    assert!((built.plant.current_a() - 0.80).abs() < 1e-9);
    // Off: the port's load is gone.
    port.configure(&LineConfig::output(false)).unwrap();
    built.plant.step(0.6);
    let off = current(&power);
    assert!((off - 0.35).abs() < 0.01, "{off}");
    // The bus sags with the load; power is the bus voltage times the current.
    let vbus = f64::from(be16(&power, 0x40, 0x05)) * 3.125e-3;
    let expected = 5.1 - 0.04 * 0.35;
    assert!((vbus - expected).abs() < 0.01, "{vbus} vs {expected}");
    let p = power.word(0x40, 0x08);
    let p_raw = u32::from_be_bytes([0, p[0], p[1], p[2]]);
    let watts = f64::from(p_raw) * 0.2 * lsb;
    assert!((watts - expected * 0.35).abs() < 0.02, "{watts}");
}

#[test]
fn fan_tachometer_lags_the_duty() {
    let mut built = build_with("fan", "");
    let fan = built.sys.join("class/hwmon/hwmon0");
    fs::write(fan.join("pwm1"), "255\n").unwrap();
    fs::write(fan.join("pwm1_enable"), "1\n").unwrap();
    let mut t = 0.0;
    // One time constant (0.8 s) at full duty: about 63 % of 4200 rpm.
    while t < 0.8 - 1e-9 {
        t += 0.05;
        built.plant.step(t);
    }
    let rpm = built.plant.rotor_rpm();
    assert!((rpm - 0.632 * 4200.0).abs() < 120.0, "{rpm}");
    // The tachometer file shows it, rounded.
    let read: f64 = fs::read_to_string(fan.join("fan1_input"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!((read - rpm.round()).abs() < 1.0);
    // Settled, after several time constants.
    while t < 6.0 {
        t += 0.05;
        built.plant.step(t);
    }
    assert!((built.plant.rotor_rpm() - 4200.0).abs() < 5.0);
    // Half duty settles at half speed.
    fs::write(fan.join("pwm1"), "128\n").unwrap();
    while t < 12.0 {
        t += 0.05;
        built.plant.step(t);
    }
    assert!(
        (built.plant.rotor_rpm() - 2100.0).abs() < 10.0,
        "{}",
        built.plant.rotor_rpm()
    );
}

#[test]
fn full_speed_mode_ignores_the_duty() {
    let mut built = build_with("fanmode", "");
    let fan = built.sys.join("class/hwmon/hwmon0");
    fs::write(fan.join("pwm1"), "0\n").unwrap();
    fs::write(fan.join("pwm1_enable"), "0\n").unwrap();
    let mut t = 0.0;
    while t < 6.0 {
        t += 0.05;
        built.plant.step(t);
    }
    assert!((built.plant.rotor_rpm() - 4200.0).abs() < 5.0);
}

#[test]
fn unresponsive_fault_silences_the_device_for_its_window() {
    let mut built = build_with(
        "unresponsive",
        "[[fault]]\ndevice = \"magnetometer\"\nkind = \"unresponsive\"\nat = 1.0\nseconds = 1.0\n",
    );
    let mag = built.i2c[&MAG_BUS].clone();
    let dead = |bus: &SimBus| bus.is_dead(0x10);
    built.plant.step(0.5);
    assert!(!dead(&mag));
    built.plant.step(1.2);
    assert!(dead(&mag));
    built.plant.step(2.5);
    assert!(!dead(&mag));
}

#[test]
fn bus_error_fault_nacks_at_its_rate_and_then_recovers() {
    let mut built = build_with(
        "buserror",
        "[[fault]]\ndevice = \"imu\"\nkind = \"bus-error\"\nat = 1.0\nseconds = 1.0\nrate = 1.0\n",
    );
    let imu = built.i2c[&IMU_BUS].clone();
    built.plant.step(1.5);
    assert!(imu.is_dead(0x18) && imu.is_dead(0x68));
    built.plant.step(2.5);
    assert!(!imu.is_dead(0x18) && !imu.is_dead(0x68));
}

#[test]
fn stuck_fault_holds_the_last_samples() {
    let mut built = build_with(
        "stuck",
        "[imu]\naccel_noise = 0.0\ngyro_noise = 0.0\n[[motion]]\nkind = \"rotate\"\nseconds = 1.0\nrate_dps = [0.0, 0.0, 200.0]\n[[motion]]\nkind = \"still\"\nseconds = 5.0\n[[fault]]\ndevice = \"imu\"\nkind = \"stuck\"\nat = 1.0\nseconds = 2.0\n",
    );
    let imu = built.i2c[&IMU_BUS].clone();
    built.plant.step(0.9);
    // Gyroscope Z: 200 °/s on the reset ±2000 °/s range is 3276.8 counts.
    let z = |bus: &SimBus| le16(bus, 0x68, 0x06);
    built.plant.step(0.9);
    let before = z(&imu);
    assert!((f64::from(before) - 3276.8).abs() < 2.0, "{before}");
    built.plant.step(2.0);
    assert_eq!(z(&imu), before, "the stuck sensor holds its last sample");
    built.plant.step(3.5);
    assert_eq!(z(&imu), 0, "after the fault, the still board reads zero");
}

#[test]
fn scripted_gpio_level_applies_at_its_time() {
    let mut built = build_with(
        "gpio",
        "[[gpio]]\nchip = \"gpiochip0\"\nline = 5\nlevel = true\nat = 1.0\n",
    );
    let mut line = built.hardware.line("gpiochip0", 5);
    built.plant.step(0.5);
    assert!(!RawLine::get(&mut line).unwrap());
    built.plant.step(1.5);
    assert!(RawLine::get(&mut line).unwrap());
}

#[test]
fn led_frames_go_to_a_file_in_the_sim_root() {
    let mut definition = BoardDefinition::from_toml_str(BOARD).unwrap();
    let sim_root = root("leds");
    let built = build(&mut definition, &script(""), &sim_root).unwrap();
    let path = definition
        .devices
        .iter()
        .find(|d| d.id == "status-ring")
        .and_then(|d| d.path.clone())
        .unwrap();
    assert!(Path::new(&path).starts_with(&sim_root), "{path}");
    assert!(Path::new(&path).is_file());
    drop(built);
}

#[test]
fn a_fault_on_an_unknown_device_is_refused() {
    let mut definition = BoardDefinition::from_toml_str(BOARD).unwrap();
    let error = build(
        &mut definition,
        &script("[[fault]]\ndevice = \"nope\"\nkind = \"stuck\"\nat = 0.0\n"),
        &root("unknown"),
    )
    .map(|_| ())
    .unwrap_err();
    assert!(error.to_string().contains("no simulated chip"), "{error}");
}

#[test]
fn drivers_without_a_model_are_refused() {
    let text = BOARD.replace("driver = \"ina238\"", "driver = \"ina226\"");
    let mut definition = BoardDefinition::from_toml_str(&text).unwrap();
    let error = build(&mut definition, &script(""), &root("ina226"))
        .map(|_| ())
        .unwrap_err();
    assert!(error.to_string().contains("no simulated model"), "{error}");
}

#[test]
fn scripts_are_checked() {
    let bad = |text: &str| Script::from_toml_str(text).unwrap_err().to_string();
    assert!(bad("format = \"nope\"\nschema_version = 1\n").contains("format"));
    assert!(bad("format = \"lemnos.sim\"\nschema_version = 2\n").contains("schema_version"));
    assert!(bad("format = \"lemnos.sim\"\nschema_version = 1\nspeed = 0.0\n").contains("speed"));
    assert!(bad("format = \"lemnos.sim\"\nschema_version = 1\nunknown = 1\n").contains("unknown"));
    assert!(
        bad("format = \"lemnos.sim\"\nschema_version = 1\n[[fault]]\ndevice = \"imu\"\nkind = \"stuck\"\nat = 0.0\nrate = 0.5\n")
            .contains("bus-error")
    );
    assert!(
        bad("format = \"lemnos.sim\"\nschema_version = 1\n[[motion]]\nkind = \"still\"\nseconds = 0.0\n")
            .contains("seconds")
    );
}

#[test]
fn bus_selectors_resolve_to_the_fake_adapter() {
    let built = build_with("selector", "");
    // The IMU's adapter exists under the sim root's sys tree, with its node,
    // and the board's selector finds it.
    let node = built.sys.join("bus/i2c/devices/i2c-16/of_node/compatible");
    assert_eq!(fs::read(node).unwrap(), b"i2c-gpio\0");
    let sys = SysRoot::new(&built.sys);
    let selector = lemnos_board::I2cSelector::parse("compatible=i2c-gpio").unwrap();
    assert_eq!(selector.resolve(&sys), Ok(IMU_BUS));
}
