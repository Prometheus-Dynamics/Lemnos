use crate::{BoundDevice, Driver, DriverBindContext, DriverError, DriverMatch, DriverResult};
use lemnos_core::{DeviceDescriptor, InterfaceKind};
use lemnos_driver_manifest::{DriverManifest, ManifestError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConformanceError {
    #[error("driver '{driver_id}' produced an invalid manifest: {source}")]
    InvalidManifest {
        driver_id: String,
        #[source]
        source: Box<ManifestError>,
    },
    #[error("driver id '{driver_id}' did not match manifest id '{manifest_id}'")]
    DriverIdMismatch {
        driver_id: String,
        manifest_id: String,
    },
    #[error(
        "driver '{driver_id}' reported interface '{driver_interface}' but manifest '{manifest_id}' did not include it"
    )]
    ManifestMissingDriverInterface {
        driver_id: String,
        manifest_id: String,
        driver_interface: InterfaceKind,
    },
    #[error("driver '{driver_id}' unexpectedly rejected device '{device_id}': {reason}")]
    UnexpectedRejection {
        driver_id: String,
        device_id: String,
        reason: String,
    },
    #[error("driver '{driver_id}' unexpectedly claimed support for device '{device_id}'")]
    UnexpectedSupport {
        driver_id: String,
        device_id: String,
    },
}

pub type ConformanceResult<T> = Result<T, ConformanceError>;

pub struct DriverConformanceHarness<'a, D> {
    driver: &'a D,
    context: DriverBindContext<'a>,
}

impl<'a, D> DriverConformanceHarness<'a, D>
where
    D: Driver,
{
    pub fn new(driver: &'a D) -> Self {
        Self {
            driver,
            context: DriverBindContext::default(),
        }
    }

    pub fn with_context(mut self, context: DriverBindContext<'a>) -> Self {
        self.context = context;
        self
    }

    pub fn driver(&self) -> &D {
        self.driver
    }

    pub fn context(&self) -> &DriverBindContext<'a> {
        &self.context
    }

    pub fn validate_manifest(&self) -> ConformanceResult<DriverManifest> {
        let manifest = self.driver.manifest_ref();
        manifest
            .validate()
            .map_err(|source| ConformanceError::InvalidManifest {
                driver_id: self.driver.id().to_string(),
                source: Box::new(source),
            })?;

        if manifest.id != self.driver.id() {
            return Err(ConformanceError::DriverIdMismatch {
                driver_id: self.driver.id().to_string(),
                manifest_id: manifest.id.clone(),
            });
        }

        if !manifest.interfaces.contains(&self.driver.interface()) {
            return Err(ConformanceError::ManifestMissingDriverInterface {
                driver_id: self.driver.id().to_string(),
                manifest_id: manifest.id.clone(),
                driver_interface: self.driver.interface(),
            });
        }

        Ok(manifest.into_owned())
    }

    pub fn match_device(&self, device: &DeviceDescriptor) -> DriverMatch {
        self.driver.matches(device)
    }

    pub fn expect_supported(&self, device: &DeviceDescriptor) -> ConformanceResult<DriverMatch> {
        let matched = self.match_device(device);
        if matched.is_supported() {
            Ok(matched)
        } else {
            Err(ConformanceError::UnexpectedRejection {
                driver_id: self.driver.id().to_string(),
                device_id: device.id.as_str().to_string(),
                reason: matched
                    .reasons
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "no reason provided".to_string()),
            })
        }
    }

    pub fn expect_unsupported(&self, device: &DeviceDescriptor) -> ConformanceResult<DriverMatch> {
        let matched = self.match_device(device);
        if matched.is_supported() {
            Err(ConformanceError::UnexpectedSupport {
                driver_id: self.driver.id().to_string(),
                device_id: device.id.as_str().to_string(),
            })
        } else {
            Ok(matched)
        }
    }

    pub fn bind(&self, device: &DeviceDescriptor) -> DriverResult<Box<dyn BoundDevice>> {
        self.driver.bind(device, &self.context)
    }

    pub fn bind_supported(
        &self,
        device: &DeviceDescriptor,
    ) -> ConformanceResult<Box<dyn BoundDevice>> {
        self.expect_supported(device)?;
        self.bind(device).map_err(|error| match error {
            DriverError::MissingBackend { .. }
            | DriverError::BindRejected { .. }
            | DriverError::BindFailed { .. }
            | DriverError::InvalidRequest { .. }
            | DriverError::Transport { .. }
            | DriverError::HostIo { .. }
            | DriverError::InvariantViolation { .. }
            | DriverError::UnsupportedAction { .. }
            | DriverError::NotImplemented { .. } => ConformanceError::UnexpectedRejection {
                driver_id: self.driver.id().to_string(),
                device_id: device.id.as_str().to_string(),
                reason: error.to_string(),
            },
        })
    }
}
