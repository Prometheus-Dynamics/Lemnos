use lemnos_runtime::{Runtime, RuntimeResult};

pub struct BuiltInDriverBundle;

impl BuiltInDriverBundle {
    pub const DRIVER_IDS: [&'static str; 6] = [
        "lemnos.gpio.generic",
        "lemnos.pwm.generic",
        "lemnos.i2c.generic",
        "lemnos.spi.generic",
        "lemnos.uart.generic",
        "lemnos.usb.generic",
    ];

    pub fn register_into(runtime: &mut Runtime) -> RuntimeResult<()> {
        runtime.register_driver(lemnos_drivers_gpio::GpioDriver)?;
        runtime.register_driver(lemnos_drivers_pwm::PwmDriver)?;
        runtime.register_driver(lemnos_drivers_i2c::I2cDriver)?;
        runtime.register_driver(lemnos_drivers_spi::SpiDriver)?;
        runtime.register_driver(lemnos_drivers_uart::UartDriver)?;
        runtime.register_driver(lemnos_drivers_usb::UsbDriver)?;
        Ok(())
    }
}
