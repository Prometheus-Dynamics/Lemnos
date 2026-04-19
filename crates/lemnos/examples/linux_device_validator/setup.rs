use crate::support::board_validator::config::{Bmi055Target, Bmm150Target, PowerTarget};
use crate::support::board_validator::drivers::{Bmi055Config, Bmm150Config, PowerSensorConfig};
use crate::{linux_hwmon_fan, linux_led};
use lemnos::core::{ConfiguredGpioSignal, DeviceId};
use lemnos::prelude::Lemnos;
use std::path::Path;

pub fn keep_example_support_symbols() {
    let _ = linux_led::LinuxLedTestRoot::new as fn() -> linux_led::LinuxLedTestRoot;
    let _ = linux_led::LinuxLedTestRoot::paths
        as fn(&linux_led::LinuxLedTestRoot) -> lemnos::linux::LinuxPaths;
    let _ = |root: &linux_led::LinuxLedTestRoot| {
        root.create_led("x", 0, 1, "none", "leds-gpio");
        let _ = root.read(Path::new("x"));
        root.create_dir(Path::new("x"));
        root.write(Path::new("x"), "");
    };
    let _ = linux_led::led_device_id as fn(&Lemnos) -> Result<DeviceId, Box<dyn std::error::Error>>;

    let _ = linux_hwmon_fan::LinuxHwmonFanTestRoot::new
        as fn() -> linux_hwmon_fan::LinuxHwmonFanTestRoot;
    let _ = linux_hwmon_fan::LinuxHwmonFanTestRoot::paths
        as fn(&linux_hwmon_fan::LinuxHwmonFanTestRoot) -> lemnos::linux::LinuxPaths;
    let _ = |root: &linux_hwmon_fan::LinuxHwmonFanTestRoot| {
        root.create_fan("x", "x", 0, 1, 0, "pwm-fan");
        let _ = root.read(Path::new("x"));
        root.create_dir(Path::new("x"));
        root.write(Path::new("x"), "");
    };
    let _ = linux_hwmon_fan::fan_device_id
        as fn(&Lemnos) -> Result<DeviceId, Box<dyn std::error::Error>>;
}

pub fn build_bmi055_config(
    target: &Bmi055Target,
) -> Result<Bmi055Config, Box<dyn std::error::Error>> {
    let mut builder = Bmi055Config::builder()
        .bus(target.bus)
        .accel_address(target.accel_address)
        .gyro_address(target.gyro_address)
        .label(target.label.clone());
    if let Some(interrupt) = &target.accel_interrupt {
        builder = builder.accel_int(ConfiguredGpioSignal::by_chip_line(
            interrupt.chip_name.clone(),
            interrupt.offset,
        ));
    }
    if let Some(interrupt) = &target.gyro_interrupt {
        builder = builder.gyro_int(ConfiguredGpioSignal::by_chip_line(
            interrupt.chip_name.clone(),
            interrupt.offset,
        ));
    }
    Ok(builder.build()?)
}

pub fn build_bmm150_config(
    target: &Bmm150Target,
) -> Result<Bmm150Config, Box<dyn std::error::Error>> {
    Ok(Bmm150Config::builder()
        .bus(target.bus)
        .address(target.address)
        .label(target.label.clone())
        .build()?)
}

pub fn build_power_config(
    target: &PowerTarget,
) -> Result<PowerSensorConfig, Box<dyn std::error::Error>> {
    Ok(PowerSensorConfig::builder()
        .bus(target.bus)
        .address(target.address)
        .label(target.label.clone())
        .kind(target.kind)
        .shunt_resistance_ohms(target.shunt_resistance_ohms)
        .max_current_a(target.max_current_a)
        .build()?)
}
