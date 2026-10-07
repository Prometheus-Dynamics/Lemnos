//! The generic binding from a chip's [`KernelBinding`] to the device model:
//! a mainline kernel driver (IIO or hwmon) serves the same [`DeviceInfo`]
//! as the chip's userspace driver, with no code per chip.

use crate::{SysfsError, sysfs};
use lemnos_device::kernel::{KernelBinding, KernelChannel, Subsystem};
use lemnos_device::{Device, DeviceError, DeviceInfo, NO_VALUE, Sensor, check_buffer};
use lemnos_hal::ErrorKind;
use std::path::{Path, PathBuf};

/// Where the kernel's class directories live (a test tree, or `/sys`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysRoot {
    root: PathBuf,
}

impl Default for SysRoot {
    fn default() -> Self {
        Self::new("/sys")
    }
}

impl SysRoot {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// `<sys>/bus/iio/devices`.
    pub fn iio(&self) -> PathBuf {
        self.root.join("bus/iio/devices")
    }

    /// `<sys>/class/hwmon`.
    pub fn hwmon(&self) -> PathBuf {
        self.root.join("class/hwmon")
    }

    /// `<sys>/class/thermal`.
    pub fn thermal(&self) -> PathBuf {
        self.root.join("class/thermal")
    }

    fn subsystem(&self, subsystem: Subsystem) -> PathBuf {
        match subsystem {
            Subsystem::Iio => self.iio(),
            Subsystem::Hwmon => self.hwmon(),
        }
    }
}

/// Narrows a lookup to the chip at an I2C bus and one of its addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I2cLocation<'a> {
    pub bus: u32,
    pub addresses: &'a [u16],
}

/// A chip served by its mainline kernel driver.
#[derive(Debug, Clone, PartialEq)]
pub struct KernelDevice {
    info: &'static DeviceInfo,
    binding: &'static KernelBinding,
    parts: Vec<PathBuf>,
}

impl KernelDevice {
    /// A device whose parts are at `parts` (one class directory per
    /// [`KernelBinding::parts`] entry, in order).
    pub fn new(
        info: &'static DeviceInfo,
        binding: &'static KernelBinding,
        parts: Vec<PathBuf>,
    ) -> Result<Self, SysfsError> {
        if parts.len() != binding.parts.len() || binding.channels.len() != info.channels.len() {
            return Err(SysfsError::new(
                ErrorKind::Configuration,
                parts.first().cloned().unwrap_or_default(),
                format!(
                    "{} needs {} kernel parts and {} channel bindings",
                    info.model,
                    binding.parts.len(),
                    info.channels.len()
                ),
            ));
        }
        Ok(Self {
            info,
            binding,
            parts,
        })
    }

    /// Finds every part of `binding` under `sys` by the class device's
    /// `name` attribute, optionally only at `location`. `None` when a part is
    /// missing: the kernel driver is not bound (no overlay, or the chip is
    /// absent).
    pub fn find(
        info: &'static DeviceInfo,
        binding: &'static KernelBinding,
        sys: &SysRoot,
        location: Option<I2cLocation<'_>>,
    ) -> Result<Option<Self>, SysfsError> {
        let mut parts = Vec::with_capacity(binding.parts.len());
        for part in binding.parts {
            let mut found = None;
            for entry in sysfs::entries(&sys.subsystem(part.subsystem))? {
                let Some(name) = sysfs::read_optional(&entry.join("name"))? else {
                    continue;
                };
                if !part.names.contains(&name.as_str()) {
                    continue;
                }
                let at = location.is_none_or(|l| {
                    l.addresses
                        .iter()
                        .any(|a| sysfs::is_i2c_client(&entry, l.bus, *a))
                });
                if at && !parts.contains(&entry) {
                    found = Some(entry);
                    break;
                }
            }
            match found {
                Some(entry) => parts.push(entry),
                None => return Ok(None),
            }
        }
        Self::new(info, binding, parts).map(Some)
    }

    /// The class directory of each part.
    pub fn parts(&self) -> &[PathBuf] {
        &self.parts
    }

    fn channel(&self, index: usize, channel: &KernelChannel) -> Result<i32, SysfsError> {
        let root = &self.parts[usize::from(channel.part)];
        let subsystem = self.binding.parts[usize::from(channel.part)].subsystem;
        let exponent = self.info.channels[index].exponent;
        let shift = i32::from(channel.unit_exponent) - i32::from(exponent);
        match subsystem {
            Subsystem::Hwmon => {
                let path = root.join(format!("{}_input", channel.attribute));
                Ok(match sysfs::read_parsed_optional::<i64>(&path)? {
                    Some(value) => {
                        lemnos_device::fixed::rescale(value, channel.unit_exponent, exponent)
                    }
                    None => NO_VALUE,
                })
            }
            Subsystem::Iio => {
                let Some(value) = iio_value(root, channel.attribute)? else {
                    return Ok(NO_VALUE);
                };
                let scaled = (value * 10f64.powi(shift)).round();
                Ok(if scaled.is_finite() {
                    scaled.clamp(f64::from(NO_VALUE) + 1.0, f64::from(i32::MAX)) as i32
                } else {
                    NO_VALUE
                })
            }
        }
    }
}

/// An IIO channel's value in the kernel's unit: `<attr>_input` if the driver
/// processes it, else `(<attr>_raw + offset) × scale` with the channel's own
/// or the shared (`in_accel_scale`) scale and offset.
fn iio_value(root: &Path, attribute: &str) -> Result<Option<f64>, SysfsError> {
    if let Some(value) =
        sysfs::read_parsed_optional::<f64>(&root.join(format!("{attribute}_input")))?
    {
        return Ok(Some(value));
    }
    let Some(raw) = sysfs::read_parsed_optional::<f64>(&root.join(format!("{attribute}_raw")))?
    else {
        return Ok(None);
    };
    let shared = attribute
        .strip_suffix("_x")
        .or_else(|| attribute.strip_suffix("_y"))
        .or_else(|| attribute.strip_suffix("_z"))
        .unwrap_or(attribute);
    let attr = |suffix: &str| -> Result<Option<f64>, SysfsError> {
        match sysfs::read_parsed_optional(&root.join(format!("{attribute}_{suffix}")))? {
            Some(v) => Ok(Some(v)),
            None => sysfs::read_parsed_optional(&root.join(format!("{shared}_{suffix}"))),
        }
    };
    let offset = attr("offset")?.unwrap_or(0.0);
    let scale = attr("scale")?.unwrap_or(1.0);
    Ok(Some((raw + offset) * scale))
}

impl Device for KernelDevice {
    type Error = SysfsError;

    fn info(&self) -> &'static DeviceInfo {
        self.info
    }

    /// The kernel already initialized the chip; this checks every part is
    /// still there.
    fn init(
        &mut self,
        _delay: &mut dyn embedded_hal::delay::DelayNs,
    ) -> Result<(), DeviceError<SysfsError>> {
        for part in &self.parts {
            if !part.exists() {
                return Err(DeviceError::Driver(SysfsError::new(
                    ErrorKind::NotFound,
                    part,
                    "kernel device is gone",
                )));
            }
        }
        Ok(())
    }
}

impl Sensor for KernelDevice {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<SysfsError>> {
        check_buffer(self.info, out)?;
        for (index, (slot, binding)) in out.iter_mut().zip(self.binding.channels).enumerate() {
            *slot = match binding {
                Some(channel) => self.channel(index, channel).map_err(DeviceError::Driver)?,
                None => NO_VALUE,
            };
        }
        Ok(())
    }
}
