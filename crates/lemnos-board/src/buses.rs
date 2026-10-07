//! Where devices get their buses: the host decides (Linux device nodes, a
//! runtime session, a test double), the factories do not.

use crate::BoardError;
use embedded_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_drivers_linux::SysRoot;
use lemnos_hal::erased::Kind;
use lemnos_hal::{ErrorKind, HalError};

/// An I2C bus of any type, its errors reduced to [`ErrorKind`].
pub struct DynI2c(Box<dyn I2c<Error = ErrorKind> + Send>);

impl DynI2c {
    pub fn new<B>(bus: B) -> Self
    where
        B: I2c + Send + 'static,
        B::Error: HalError,
    {
        Self(Box::new(Kind(bus)))
    }
}

impl ErrorType for DynI2c {
    type Error = ErrorKind;
}

impl I2c for DynI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        self.0.transaction(address, operations)
    }
}

impl std::fmt::Debug for DynI2c {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DynI2c")
    }
}

/// What a host gives the factories.
pub trait Buses {
    /// Opens I2C bus `bus` for one device.
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError>;

    /// Where the kernel's class directories are, for kernel-backed devices,
    /// fans and thermal zones.
    fn sys(&self) -> SysRoot {
        SysRoot::default()
    }
}

/// Buses from Linux device nodes (`/dev/i2c-N`) through
/// `lemnos_linux::hal`.
#[cfg(feature = "linux")]
#[derive(Debug, Clone)]
pub struct LinuxBuses {
    sys: SysRoot,
    dev: std::path::PathBuf,
}

#[cfg(feature = "linux")]
impl Default for LinuxBuses {
    fn default() -> Self {
        Self {
            sys: SysRoot::default(),
            dev: "/dev".into(),
        }
    }
}

#[cfg(feature = "linux")]
impl LinuxBuses {
    /// Buses under other roots (a test tree, a chroot).
    pub fn with_roots(
        sys: impl Into<std::path::PathBuf>,
        dev: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            sys: SysRoot::new(sys),
            dev: dev.into(),
        }
    }
}

#[cfg(feature = "linux")]
impl Buses for LinuxBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        let path = self.dev.join(format!("i2c-{bus}"));
        lemnos_linux::hal::I2cBus::open_path(&path)
            .map(DynI2c::new)
            .map_err(|e| BoardError::Device {
                device: path.display().to_string(),
                kind: ErrorKind::from_io(e.kind()),
                reason: e.to_string(),
            })
    }

    fn sys(&self) -> SysRoot {
        self.sys.clone()
    }
}
