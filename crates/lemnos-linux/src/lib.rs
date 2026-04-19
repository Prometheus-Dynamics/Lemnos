#![forbid(unsafe_code)]

mod backend;
mod discovery;
mod metadata;
mod paths;
mod transport;
mod util;
#[cfg(feature = "hotplug")]
mod watch;

pub use backend::LinuxBackend;
pub use backend::LinuxTransportConfig;
#[cfg(feature = "i2c")]
pub use discovery::I2cDiscoveryProbe;
#[cfg(feature = "spi")]
pub use discovery::SpiDiscoveryProbe;
#[cfg(feature = "uart")]
pub use discovery::UartDiscoveryProbe;
#[cfg(feature = "usb")]
pub use discovery::UsbDiscoveryProbe;
pub use discovery::{GpioDiscoveryProbe, LedDiscoveryProbe};
#[cfg(feature = "pwm")]
pub use discovery::{HwmonDiscoveryProbe, PwmDiscoveryProbe};
pub use paths::LinuxPaths;
#[cfg(feature = "hotplug")]
pub use watch::LinuxHotplugWatcher;

#[cfg(test)]
mod tests;
