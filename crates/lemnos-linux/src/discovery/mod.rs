mod gpio;
#[cfg(feature = "pwm")]
mod hwmon;
#[cfg(feature = "i2c")]
mod i2c;
mod led;
#[cfg(feature = "pwm")]
mod pwm;
#[cfg(feature = "spi")]
mod spi;
#[cfg(feature = "uart")]
mod uart;
#[cfg(feature = "usb")]
mod usb;

pub use gpio::GpioDiscoveryProbe;
#[cfg(feature = "pwm")]
pub use hwmon::HwmonDiscoveryProbe;
#[cfg(feature = "i2c")]
pub use i2c::I2cDiscoveryProbe;
pub use led::LedDiscoveryProbe;
#[cfg(feature = "pwm")]
pub use pwm::PwmDiscoveryProbe;
#[cfg(feature = "spi")]
pub use spi::SpiDiscoveryProbe;
#[cfg(feature = "uart")]
pub use uart::UartDiscoveryProbe;
#[cfg(feature = "usb")]
pub use usb::UsbDiscoveryProbe;
