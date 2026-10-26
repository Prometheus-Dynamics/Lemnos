//! A network link (`/sys/class/net/<name>`, such as a CAN bus `can_s0`) as a
//! read-only device-model sensor: whether the link is up, and whether it has
//! carrier.
//!
//! Only sysfs is read. A CAN controller's bus state (error-active,
//! error-passive, bus-off) is not in sysfs; it needs netlink, which this
//! driver does not use.

use crate::{SysfsError, sysfs};
use lemnos_device::{
    Channel, Device, DeviceClass, DeviceError, DeviceInfo, Quantity, Sensor, check_buffer,
};
use std::path::{Path, PathBuf};

/// Two channels, each 0 or 1: `up` (the operational state is `up`) and
/// `carrier` (the link sees a carrier; 0 while the link is down).
pub static NET_LINK_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Other,
    "net-link",
    &[
        Channel::new("up", Quantity::Level, 0),
        Channel::new("carrier", Quantity::Level, 0),
    ],
    &[],
);

/// A network link by its class directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetLink {
    root: PathBuf,
}

impl NetLink {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `operstate` reads `up`.
    pub fn is_up(&self) -> Result<bool, SysfsError> {
        Ok(sysfs::read(&self.root.join("operstate"))? == "up")
    }

    /// Whether `carrier` reads 1. The kernel refuses to read `carrier` while
    /// the link is down; that is no carrier, not an error.
    pub fn has_carrier(&self) -> bool {
        matches!(
            sysfs::read_optional(&self.root.join("carrier")),
            Ok(Some(text)) if text == "1"
        )
    }
}

impl Device for NetLink {
    type Error = SysfsError;

    fn info(&self) -> &'static DeviceInfo {
        &NET_LINK_INFO
    }

    fn init(
        &mut self,
        _delay: &mut dyn embedded_hal::delay::DelayNs,
    ) -> Result<(), DeviceError<SysfsError>> {
        self.is_up().map_err(DeviceError::Driver)?;
        Ok(())
    }
}

impl Sensor for NetLink {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<SysfsError>> {
        check_buffer(&NET_LINK_INFO, out)?;
        out[0] = i32::from(self.is_up().map_err(DeviceError::Driver)?);
        out[1] = i32::from(self.has_carrier());
        Ok(())
    }
}
