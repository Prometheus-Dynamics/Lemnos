use crate::{DriverError, DriverResult};
use lemnos_core::{DeviceControlSurface, DeviceDescriptor, DeviceId};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LinuxClassDeviceIo {
    driver_id: String,
    device_id: DeviceId,
    root: PathBuf,
}

impl LinuxClassDeviceIo {
    pub fn from_device(driver_id: &str, device: &DeviceDescriptor) -> DriverResult<Self> {
        let DeviceControlSurface::LinuxClass { root } = device
            .control_surface
            .as_ref()
            .ok_or_else(|| DriverError::BindRejected {
                driver_id: driver_id.to_string(),
                device_id: device.id.clone(),
                reason: "device is missing required typed Linux class control surface".into(),
            })?;
        Ok(Self::new(driver_id, device.id.clone(), PathBuf::from(root)))
    }

    pub fn new(driver_id: &str, device_id: DeviceId, root: PathBuf) -> Self {
        Self {
            driver_id: driver_id.to_string(),
            device_id,
            root,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    pub fn exists(&self, relative: impl AsRef<Path>) -> bool {
        self.path(relative).exists()
    }

    pub fn read_trimmed(&self, relative: impl AsRef<Path>) -> DriverResult<String> {
        let relative = relative.as_ref();
        let path = self.path(relative);
        let contents = fs::read_to_string(&path).map_err(|source| {
            self.host_io_error(format!("read '{}'", relative.display()), source)
        })?;
        Ok(contents.trim().to_string())
    }

    pub fn read_optional_trimmed(
        &self,
        relative: impl AsRef<Path>,
    ) -> DriverResult<Option<String>> {
        let relative = relative.as_ref();
        let path = self.path(relative);
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let trimmed = contents.trim().to_string();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed))
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => {
                Err(self.host_io_error(format!("read '{}'", relative.display()), source))
            }
        }
    }

    pub fn read_u64(&self, relative: impl AsRef<Path>) -> DriverResult<u64> {
        let relative = relative.as_ref();
        let contents = self.read_trimmed(relative)?;
        contents
            .parse::<u64>()
            .map_err(|error| DriverError::InvariantViolation {
                driver_id: self.driver_id.clone(),
                device_id: self.device_id.clone(),
                reason: format!(
                    "failed to parse '{}' at '{}' as u64: {error}",
                    contents,
                    self.path(relative).display()
                ),
            })
    }

    pub fn read_optional_u64(&self, relative: impl AsRef<Path>) -> DriverResult<Option<u64>> {
        let relative = relative.as_ref();
        match self.read_optional_trimmed(relative)? {
            Some(value) => {
                value
                    .parse::<u64>()
                    .map(Some)
                    .map_err(|error| DriverError::InvariantViolation {
                        driver_id: self.driver_id.clone(),
                        device_id: self.device_id.clone(),
                        reason: format!(
                            "failed to parse '{}' at '{}' as u64: {error}",
                            value,
                            self.path(relative).display()
                        ),
                    })
            }
            None => Ok(None),
        }
    }

    pub fn write_str(
        &self,
        relative: impl AsRef<Path>,
        value: impl AsRef<str>,
    ) -> DriverResult<()> {
        let relative = relative.as_ref();
        fs::write(self.path(relative), value.as_ref())
            .map_err(|source| self.host_io_error(format!("write '{}'", relative.display()), source))
    }

    pub fn write_u64(&self, relative: impl AsRef<Path>, value: u64) -> DriverResult<()> {
        self.write_str(relative, value.to_string())
    }

    fn host_io_error(&self, action: String, source: io::Error) -> DriverError {
        DriverError::HostIo {
            driver_id: self.driver_id.clone(),
            device_id: self.device_id.clone(),
            action,
            source,
        }
    }
}
