use lemnos_core::{InterfaceKind, Value};
use lemnos_device::BoxedDevice;
use lemnos_driver_manifest::{MatchCondition, MatchRule};
use lemnos_driver_sdk::LinuxClassDeviceIo;
use lemnos_driver_sdk::l1::L1Driver;
use lemnos_runtime::{Runtime, RuntimeResult};

/// The thermal-zone driver id.
pub const THERMAL_ZONE_DRIVER: &str = "lemnos.linux.thermal-zone";

/// Binds the Linux backend's thermal zones (`linux.subsystem = "thermal"`)
/// as device-model temperature sensors (`lemnos_drivers_linux::ThermalZone`).
pub fn thermal_zone_driver() -> L1Driver {
    L1Driver::new(
        THERMAL_ZONE_DRIVER,
        "Linux thermal zone",
        InterfaceKind::Platform,
        |device, _context| {
            let io = LinuxClassDeviceIo::from_device(THERMAL_ZONE_DRIVER, device)?;
            Ok(BoxedDevice::sensor(lemnos_drivers_linux::ThermalZone::new(
                io.root(),
            )))
        },
    )
    .matching(MatchRule::new(200).described("Linux thermal zone").require(
        MatchCondition::PropertyEq {
            key: "linux.subsystem".into(),
            value: Value::from("thermal"),
        },
    ))
}

pub struct BuiltInDriverBundle;

impl BuiltInDriverBundle {
    pub const DRIVER_IDS: [&'static str; 8] = [
        "lemnos.gpio.generic",
        "lemnos.pwm.generic",
        "lemnos.pwm.hwmon-fan",
        "lemnos.i2c.generic",
        "lemnos.spi.generic",
        "lemnos.uart.generic",
        "lemnos.usb.generic",
        THERMAL_ZONE_DRIVER,
    ];

    pub fn register_into(runtime: &mut Runtime) -> RuntimeResult<()> {
        runtime.register_driver(lemnos_drivers_gpio::GpioDriver)?;
        runtime.register_driver(lemnos_drivers_pwm::PwmDriver)?;
        runtime.register_driver(lemnos_drivers_pwm::HwmonFanDriver)?;
        runtime.register_driver(lemnos_drivers_i2c::I2cDriver)?;
        runtime.register_driver(lemnos_drivers_spi::SpiDriver)?;
        runtime.register_driver(lemnos_drivers_uart::UartDriver)?;
        runtime.register_driver(lemnos_drivers_usb::UsbDriver)?;
        runtime.register_driver(thermal_zone_driver())?;
        Ok(())
    }
}
