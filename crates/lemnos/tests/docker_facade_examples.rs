#![cfg(feature = "mock")]

mod support;

use support::docker_facade::{DockerFacade, output_text};

#[test]
#[ignore = "requires docker"]
fn docker_facade_example_mock_gpio_reports_high_level() {
    let facade = DockerFacade::start();
    let output = facade.run_example_output("mock_gpio");
    assert!(
        output.status.success(),
        "mock_gpio failed\n{}",
        output_text(&output)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("device mock.gpio.gpiochip0.17 level ="));
    assert!(stdout.contains("String(\"high\")"));
}

#[test]
#[ignore = "requires docker"]
fn docker_facade_example_mock_gpio_async_reports_high_level() {
    let facade = DockerFacade::start();
    let output = facade.run_example_output("mock_gpio_async");
    assert!(
        output.status.success(),
        "mock_gpio_async failed\n{}",
        output_text(&output)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("device mock.gpio.gpiochip0.17 level ="));
    assert!(stdout.contains("String(\"high\")"));
}

#[test]
#[ignore = "requires docker"]
fn docker_facade_example_mock_usb_hotplug_runs_rebind_cycle() {
    let facade = DockerFacade::start();
    let output = facade.run_example_output("mock_usb_hotplug");
    assert!(
        output.status.success(),
        "mock_usb_hotplug failed\n{}",
        output_text(&output)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("claim response"));
    assert!(stdout.contains("after removal inventory contains interface: false"));
    assert!(stdout.contains("control response"));
}

#[test]
#[ignore = "requires docker"]
fn docker_facade_example_mock_power_sensor_driver_reports_sample() {
    let facade = DockerFacade::start();
    let output = facade.run_example_output("mock_power_sensor_driver");
    assert!(
        output.status.success(),
        "mock_power_sensor_driver failed\n{}",
        output_text(&output)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sample: label=main-battery"));
    assert!(stdout.contains("cached telemetry: power_w=Some(F64(4.0))"));
}

#[test]
#[ignore = "requires docker"]
fn docker_facade_example_mock_ina226_driver_reports_configured_sample() {
    let facade = DockerFacade::start();
    let output = facade.run_example_output("mock_ina226_driver");
    assert!(
        output.status.success(),
        "mock_ina226_driver failed\n{}",
        output_text(&output)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("INA226 response"));
    assert!(stdout.contains("battery-rail"));
    assert!(stdout.contains("bus_voltage_v"));
}

#[test]
#[ignore = "requires docker"]
fn docker_facade_example_mock_bmm150_driver_reports_configured_sample() {
    let facade = DockerFacade::start();
    let output = facade.run_example_output("mock_bmm150_driver");
    assert!(
        output.status.success(),
        "mock_bmm150_driver failed\n{}",
        output_text(&output)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("BMM150 response"));
    assert!(stdout.contains("deck-mag"));
    assert!(stdout.contains("rhall_raw"));
}

#[test]
#[ignore = "requires docker"]
fn docker_facade_example_mock_bmi088_driver_reports_configured_sample() {
    let facade = DockerFacade::start();
    let output = facade.run_example_output("mock_bmi088_driver");
    assert!(
        output.status.success(),
        "mock_bmi088_driver failed\n{}",
        output_text(&output)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("BMI088 response"));
    assert!(stdout.contains("board-imu"));
    assert!(stdout.contains("gyro_dps"));
}
