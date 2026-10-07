#![forbid(unsafe_code)]

#[cfg(all(feature = "hotplug", feature = "tokio"))]
mod async_watch;
mod backend;
mod discovery;
pub mod hal;
mod metadata;
mod paths;
mod transport;
pub mod uevent;
mod util;
#[cfg(feature = "hotplug")]
mod watch;

#[cfg(all(feature = "hotplug", feature = "tokio"))]
pub use async_watch::AsyncLinuxHotplugWatcher;
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
pub use discovery::{GpioDiscoveryProbe, LedDiscoveryProbe, ThermalDiscoveryProbe};
#[cfg(feature = "pwm")]
pub use discovery::{HwmonDiscoveryProbe, PwmDiscoveryProbe};
pub use paths::LinuxPaths;
#[cfg(feature = "hotplug")]
pub use watch::{HotplugSource, LinuxHotplugWatcher};

#[cfg(test)]
mod tests;
