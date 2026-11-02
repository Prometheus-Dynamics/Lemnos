#![forbid(unsafe_code)]

#[macro_use]
mod trace;

#[cfg(all(feature = "hotplug", feature = "tokio"))]
mod async_watch;
mod backend;
pub mod device_watch;
mod discovery;
#[cfg(feature = "dma")]
pub mod dma;
pub mod hal;
mod metadata;
mod paths;
#[cfg(any(feature = "i2c", feature = "spi"))]
pub mod transfer;
mod transport;
pub mod uevent;
mod util;
#[cfg(feature = "v4l2-m2m")]
pub mod v4l2;
#[cfg(feature = "hotplug")]
mod watch;

#[cfg(all(feature = "hotplug", feature = "tokio"))]
pub use async_watch::AsyncLinuxHotplugWatcher;
pub use backend::LinuxBackend;
pub use backend::LinuxTransportConfig;
pub use device_watch::{DeviceWatchSource, DeviceWatcher, Filter};
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
