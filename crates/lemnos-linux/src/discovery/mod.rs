use lemnos_discovery::DiscoveryError;
use std::path::Path;

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
mod thermal;
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
pub use thermal::ThermalDiscoveryProbe;
#[cfg(feature = "uart")]
pub use uart::UartDiscoveryProbe;
#[cfg(feature = "usb")]
pub use usb::UsbDiscoveryProbe;

/// A probe's failed filesystem read, named by what it was doing.
fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
