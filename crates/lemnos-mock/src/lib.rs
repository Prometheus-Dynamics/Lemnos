#![forbid(unsafe_code)]

mod faults;
mod gpio;
mod hardware;
mod hwmon;
mod i2c;
mod pwm;
mod spi;
mod uart;
mod usb;

pub use faults::{MockFaultScript, MockFaultStep};
pub use gpio::MockGpioLine;
pub use hardware::{MockHardware, MockHardwareBuilder};
pub use hwmon::MockHwmonFan;
pub use i2c::MockI2cDevice;
pub use pwm::MockPwmChannel;
pub use spi::MockSpiDevice;
pub use uart::MockUartPort;
pub use usb::MockUsbDevice;

#[cfg(test)]
mod hal_tests;
#[cfg(test)]
mod tests;
