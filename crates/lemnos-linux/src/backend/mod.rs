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

/// Probes added to the backend's own ([`LinuxBackend::with_probe`]).
/// Compared by identity.
#[derive(Clone, Default)]
struct ExtraProbes(Vec<std::sync::Arc<dyn lemnos_discovery::DiscoveryProbe>>);

impl PartialEq for ExtraProbes {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .zip(&other.0)
                .all(|(a, b)| std::sync::Arc::ptr_eq(a, b))
    }
}

impl Eq for ExtraProbes {}

impl std::fmt::Debug for ExtraProbes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|probe| probe.name()))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBackend {
    paths: LinuxPaths,
    transport_config: LinuxTransportConfig,
    extra_probes: ExtraProbes,
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxBackend {
    pub const SUPPORTED_INTERFACES: &'static [InterfaceKind] = &[
        InterfaceKind::Gpio,
        InterfaceKind::Platform,
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
            extra_probes: ExtraProbes::default(),
        }
    }

    pub fn with_paths(paths: LinuxPaths) -> Self {
        Self {
            paths,
            transport_config: LinuxTransportConfig::default(),
            extra_probes: ExtraProbes::default(),
        }
    }

    pub fn with_config(transport_config: LinuxTransportConfig) -> Self {
        Self {
            paths: LinuxPaths::default(),
            transport_config,
            extra_probes: ExtraProbes::default(),
        }
    }

    pub fn with_paths_and_config(
        paths: LinuxPaths,
        transport_config: LinuxTransportConfig,
    ) -> Self {
        Self {
            paths,
            transport_config,
            extra_probes: ExtraProbes::default(),
        }
    }

    /// Adds a probe that runs with the backend's own in every refresh made
    /// through it (`refresh_with_linux`, watcher refreshes), such as a board
    /// definition's configured devices.
    pub fn with_probe(
        mut self,
        probe: std::sync::Arc<dyn lemnos_discovery::DiscoveryProbe>,
    ) -> Self {
        self.extra_probes.0.push(probe);
        self
    }

    /// The probes added with [`with_probe`](Self::with_probe).
    pub fn extra_probes(&self) -> impl Iterator<Item = &dyn lemnos_discovery::DiscoveryProbe> {
        self.extra_probes.0.iter().map(|probe| &**probe)
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
