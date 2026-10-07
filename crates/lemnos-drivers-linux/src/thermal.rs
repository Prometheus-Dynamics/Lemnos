//! A thermal zone (`/sys/class/thermal/thermal_zoneN`) as a device-model
//! temperature sensor.

use crate::{SysfsError, sysfs};
use lemnos_device::{
    Channel, Device, DeviceClass, DeviceError, DeviceInfo, Quantity, Sensor, check_buffer,
};
use std::path::{Path, PathBuf};

/// One channel, `temperature`, in m°C.
pub static THERMAL_INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Temperature,
    "thermal-zone",
    &[Channel::new("temperature", Quantity::Temperature, -3)],
    &[],
);

/// A Linux thermal zone by its class directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThermalZone {
    root: PathBuf,
}

impl ThermalZone {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The first zone under `thermal_root` (`/sys/class/thermal`) whose
    /// `type` is `zone_type` (`cpu-thermal`).
    pub fn find(thermal_root: &Path, zone_type: &str) -> Result<Option<Self>, SysfsError> {
        for entry in sysfs::entries(thermal_root)? {
            let is_zone = entry
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("thermal_zone"));
            if is_zone && sysfs::read_optional(&entry.join("type"))?.as_deref() == Some(zone_type) {
                return Ok(Some(Self::new(entry)));
            }
        }
        Ok(None)
    }

    /// Every zone under `thermal_root`.
    pub fn all(thermal_root: &Path) -> Result<Vec<Self>, SysfsError> {
        Ok(sysfs::entries(thermal_root)?
            .into_iter()
            .filter(|entry| {
                entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("thermal_zone"))
            })
            .map(Self::new)
            .collect())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The zone's `type` (`cpu-thermal`).
    pub fn zone_type(&self) -> Result<String, SysfsError> {
        sysfs::read(&self.root.join("type"))
    }

    /// The temperature in m°C.
    pub fn temperature_mc(&self) -> Result<i32, SysfsError> {
        sysfs::read_parsed(&self.root.join("temp"))
    }
}

impl Device for ThermalZone {
    type Error = SysfsError;

    fn info(&self) -> &'static DeviceInfo {
        &THERMAL_INFO
    }

    fn init(
        &mut self,
        _delay: &mut dyn embedded_hal::delay::DelayNs,
    ) -> Result<(), DeviceError<SysfsError>> {
        self.temperature_mc().map_err(DeviceError::Driver)?;
        Ok(())
    }
}

impl Sensor for ThermalZone {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<SysfsError>> {
        check_buffer(&THERMAL_INFO, out)?;
        out[0] = self.temperature_mc().map_err(DeviceError::Driver)?;
        Ok(())
    }
}
