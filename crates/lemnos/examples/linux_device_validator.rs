#![allow(clippy::print_stderr, clippy::print_stdout)]

#[path = "linux_device_validator/inventory.rs"]
mod inventory;
#[path = "support/linux_hwmon_fan.rs"]
mod linux_hwmon_fan;
#[path = "support/linux_led.rs"]
mod linux_led;
#[path = "support/linux_test_root.rs"]
mod linux_test_root;
#[path = "linux_device_validator/setup.rs"]
mod setup;
mod support;
#[path = "linux_device_validator/validation.rs"]
mod validation;

use lemnos::discovery::{DiscoveryContext, DiscoveryProbe};
use lemnos::linux::LinuxBackend;
use lemnos::prelude::*;
use std::env;
use std::process::ExitCode;
use support::board_validator::config::ValidatorConfig;
use support::board_validator::drivers::{
    Bmi055Config, Bmi055Driver, Bmm150Config, Bmm150Driver, PowerSensorConfig, PowerSensorDriver,
};
use support::board_validator::report::ValidatorReport;

fn main() -> ExitCode {
    match run() {
        Ok(exit_code) => exit_code,
        Err(error) => {
            if error.to_string().starts_with("usage: ") {
                println!("{error}");
                return ExitCode::SUCCESS;
            }
            eprintln!("validator setup failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    setup::keep_example_support_symbols();

    let args = env::args().collect::<Vec<_>>();
    let config = ValidatorConfig::from_args(&args)?;
    let backend = LinuxBackend::new();
    let context = DiscoveryContext::new();
    let mut report = ValidatorReport::new();

    print_header(&config);

    let mut lemnos = Lemnos::builder()
        .with_linux_backend_ref(&backend)
        .with_builtin_drivers()?
        .with_driver(linux_led::ExampleLinuxLedDriver)?
        .with_driver(linux_hwmon_fan::ExampleLinuxHwmonFanDriver)?
        .build();

    let bmi055_config = config
        .bmi055
        .as_ref()
        .map(setup::build_bmi055_config)
        .transpose()?;
    register_bmi055_driver(&mut lemnos, &bmi055_config)?;

    let bmm150_config = config
        .bmm150
        .as_ref()
        .map(setup::build_bmm150_config)
        .transpose()?;
    register_bmm150_driver(&mut lemnos, &bmm150_config)?;

    let power_config = config
        .power
        .as_ref()
        .map(setup::build_power_config)
        .transpose()?;
    register_power_driver(&mut lemnos, &power_config)?;

    let gpio_probe = backend.gpio_probe();
    let led_probe = backend.led_probe();
    let pwm_probe = backend.pwm_probe();
    let hwmon_probe = backend.hwmon_probe();
    let i2c_probe = backend.i2c_probe();
    let spi_probe = backend.spi_probe();
    let uart_probe = backend.uart_probe();
    let usb_probe = backend.usb_probe();
    let bmi055_probe = bmi055_config
        .clone()
        .map(|config| Bmi055Config::configured_probe("validator-bmi055", vec![config]));
    let bmm150_probe = bmm150_config
        .clone()
        .map(|config| Bmm150Config::configured_probe("validator-bmm150", vec![config]));
    let power_probe = power_config
        .clone()
        .map(|config| PowerSensorConfig::configured_probe("validator-power", vec![config]));

    let mut probes: Vec<&dyn DiscoveryProbe> = vec![
        &gpio_probe,
        &led_probe,
        &pwm_probe,
        &hwmon_probe,
        &i2c_probe,
        &spi_probe,
        &uart_probe,
        &usb_probe,
    ];
    if let Some(probe) = &bmi055_probe {
        probes.push(probe);
    }
    if let Some(probe) = &bmm150_probe {
        probes.push(probe);
    }
    if let Some(probe) = &power_probe {
        probes.push(probe);
    }
    lemnos.refresh(&context, &probes)?;

    inventory::print_inventory_summary(lemnos.inventory());
    validation::run_validations(
        &mut lemnos,
        &config,
        bmi055_config.as_ref(),
        bmm150_config.as_ref(),
        power_config.as_ref(),
        &mut report,
    );

    report.print_summary();

    Ok(if report.has_failures() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn print_header(config: &ValidatorConfig) {
    println!("lemnos linux device validator");
    println!("board: {}", config.board_name);
    if let Some(path) = &config.config_path {
        println!("config: {}", path.display());
    } else {
        println!("config: <none>");
    }
    println!();
}

fn register_bmi055_driver(
    lemnos: &mut Lemnos,
    config: &Option<Bmi055Config>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(config) = config {
        lemnos.register_driver(Bmi055Driver::from_configs([config.clone()]))?;
    }
    Ok(())
}

fn register_bmm150_driver(
    lemnos: &mut Lemnos,
    config: &Option<Bmm150Config>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(config) = config {
        lemnos.register_driver(Bmm150Driver::from_configs([config.clone()]))?;
    }
    Ok(())
}

fn register_power_driver(
    lemnos: &mut Lemnos,
    config: &Option<PowerSensorConfig>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(config) = config {
        lemnos.register_driver(PowerSensorDriver::from_configs([config.clone()]))?;
    }
    Ok(())
}
