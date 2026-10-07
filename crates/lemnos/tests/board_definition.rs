//! A board definition bound by the runtime: the BMI088 and BMM150 come from
//! mock I2C hardware through the runtime's I2C backend, the fan and the
//! thermal zone from a fake sysfs tree, and every device is a
//! `lemnos-device` driver behind the generic adapter.
#![cfg(all(feature = "board", feature = "mock"))]

use lemnos::board::{BoardDefinition, BoardSetup};
use lemnos::core::{InteractionResponse, Value};
use lemnos::discovery::DiscoveryContext;
use lemnos::mock::{MockHardware, MockI2cDevice};
use lemnos::prelude::*;
use std::fs;
use std::path::PathBuf;

fn tree() -> PathBuf {
    let root = std::env::temp_dir().join(format!("lemnos-board-runtime-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for (path, contents) in [
        ("class/hwmon/hwmon2/name", "pwm-fan"),
        ("class/hwmon/hwmon2/pwm1", "128"),
        ("class/hwmon/hwmon2/pwm1_enable", "2"),
        ("class/hwmon/hwmon2/fan1_input", "2400"),
        ("class/thermal/thermal_zone0/type", "cpu-thermal"),
        ("class/thermal/thermal_zone0/temp", "47500"),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    root
}

fn axes(values: [i16; 3]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-4"
address = 0x18
backend = "userspace"

[[devices]]
id = "magnetometer"
driver = "bmm150"
bus = "i2c-4"
address = 0x10
backend = "userspace"

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = { name = "pwm-fan" }

[[devices]]
id = "cpu-thermal"
driver = "thermal-zone"
match = { type = "cpu-thermal" }
"#;

#[test]
fn board_devices_bind_and_report_device_model_telemetry() {
    let sys = tree();
    let hardware = MockHardware::builder()
        .with_i2c_device(
            MockI2cDevice::new(4, 0x18)
                .with_bytes(0x00, [0x1e])
                .with_bytes(0x12, axes([16384, 0, -16384])),
        )
        .with_i2c_device(MockI2cDevice::new(4, 0x68).with_bytes(0x00, [0x0f]))
        .with_i2c_device(MockI2cDevice::new(4, 0x10).with_bytes(0x40, [0x32]))
        .build();
    let setup = BoardSetup::new(BoardDefinition::from_toml_str(BOARD).unwrap())
        .unwrap()
        .with_sys_root(&sys);
    let probe = setup.probe();
    let mut lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_board(&setup)
        .unwrap()
        .build();
    let report = lemnos
        .refresh(&DiscoveryContext::new(), &[&hardware, &probe])
        .unwrap();
    assert!(report.rebinds.failed.is_empty(), "{:?}", report.rebinds);

    let imu = setup.device_id("imu").unwrap();
    assert_eq!(imu.as_str(), "board.raze.imu");
    assert!(lemnos.is_bound(&imu));
    let state = lemnos.refresh_state(&imu).unwrap().unwrap();
    // Half of ±6 g, in m/s².
    let x = state.telemetry["acceleration.x"].as_f64().unwrap();
    assert!((x - 29.419).abs() < 1e-3, "{x}");
    assert!(state.telemetry["acceleration.z"].as_f64().unwrap() < -29.0);
    assert_eq!(state.realized_config["device.class"], Value::from("imu"));
    let descriptor = lemnos
        .inventory()
        .devices
        .iter()
        .find(|d| d.id == imu)
        .unwrap()
        .clone();
    assert_eq!(descriptor.properties["board.driver"], Value::from("bmi088"));
    assert_eq!(descriptor.labels["device.class"], "imu");

    let mag = setup.device_id("magnetometer").unwrap();
    let state = lemnos.refresh_state(&mag).unwrap().unwrap();
    assert!(state.telemetry.contains_key("magnetic_field.x"));

    let zone = setup.device_id("cpu-thermal").unwrap();
    let state = lemnos.refresh_state(&zone).unwrap().unwrap();
    assert_eq!(state.telemetry["temperature"].as_f64(), Some(47.5));

    let fan = setup.device_id("fan").unwrap();
    let state = lemnos.refresh_state(&fan).unwrap().unwrap();
    assert_eq!(state.telemetry["speed"].as_f64(), Some(2400.0));
    lemnos
        .request_custom_value(fan.clone(), "pwm_mode.set", 1u64)
        .unwrap();
    let response = lemnos
        .request_custom_value(fan.clone(), "duty.set", 0.7)
        .unwrap();
    let InteractionResponse::Custom(custom) = response.interaction else {
        panic!("custom response");
    };
    // 70 % is pwm 179, which reads back as 70.2 %.
    assert_eq!(custom.output.and_then(|v| v.as_f64()), Some(0.702));
    assert_eq!(
        fs::read_to_string(sys.join("class/hwmon/hwmon2/pwm1")).unwrap(),
        "179"
    );
    assert!(lemnos.request_custom_value(fan, "duty.set", 1.5).is_err());
    let _ = fs::remove_dir_all(sys);
}
