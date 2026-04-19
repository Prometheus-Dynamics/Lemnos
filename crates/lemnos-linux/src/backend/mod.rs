use crate::LinuxPaths;
use lemnos_core::InterfaceKind;

mod config;
mod discovery;
mod transport;

pub use config::LinuxTransportConfig;

pub(crate) const BACKEND_NAME: &str = "linux";

#[cfg(feature = "tracing")]
macro_rules! backend_debug {
    ($($arg:tt)*) => {
        { tracing::debug!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! backend_debug {
    ($($arg:tt)*) => {};
}

#[cfg(feature = "tracing")]
macro_rules! backend_info {
    ($($arg:tt)*) => {
        { tracing::info!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! backend_info {
    ($($arg:tt)*) => {};
}

#[cfg(feature = "tracing")]
macro_rules! backend_warn {
    ($($arg:tt)*) => {
        { tracing::warn!($($arg)*) }
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! backend_warn {
    ($($arg:tt)*) => {};
}

pub(crate) use backend_debug;
pub(crate) use backend_info;
pub(crate) use backend_warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBackend {
    paths: LinuxPaths,
    transport_config: LinuxTransportConfig,
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxBackend {
    pub const SUPPORTED_INTERFACES: &'static [InterfaceKind] = &[
        InterfaceKind::Gpio,
        #[cfg(feature = "pwm")]
        InterfaceKind::Pwm,
        #[cfg(feature = "i2c")]
        InterfaceKind::I2c,
        #[cfg(feature = "spi")]
        InterfaceKind::Spi,
        #[cfg(feature = "uart")]
        InterfaceKind::Uart,
        #[cfg(feature = "usb")]
        InterfaceKind::Usb,
    ];

    pub const PLANNED_INTERFACES: &'static [InterfaceKind] = Self::SUPPORTED_INTERFACES;

    pub fn new() -> Self {
        Self {
            paths: LinuxPaths::default(),
            transport_config: LinuxTransportConfig::default(),
        }
    }

    pub fn with_paths(paths: LinuxPaths) -> Self {
        Self {
            paths,
            transport_config: LinuxTransportConfig::default(),
        }
    }

    pub fn with_config(transport_config: LinuxTransportConfig) -> Self {
        Self {
            paths: LinuxPaths::default(),
            transport_config,
        }
    }

    pub fn with_paths_and_config(
        paths: LinuxPaths,
        transport_config: LinuxTransportConfig,
    ) -> Self {
        Self {
            paths,
            transport_config,
        }
    }

    pub fn paths(&self) -> &LinuxPaths {
        &self.paths
    }

    pub fn transport_config(&self) -> &LinuxTransportConfig {
        &self.transport_config
    }

    pub fn supported_interfaces() -> &'static [InterfaceKind] {
        Self::SUPPORTED_INTERFACES
    }

    pub fn planned_interfaces() -> &'static [InterfaceKind] {
        Self::PLANNED_INTERFACES
    }
}
